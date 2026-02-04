use std::{
    fs::File,
    io::{self, BufReader, Read, Seek, Write},
    path::{Path, PathBuf},
};

mod mtbin;
// Import MVersion from mtbin to access the new 26.10.20 option
use crate::mtbin::{handle_lightmaps, MVersion};

use anyhow::Context;
use clap::{
    builder::{
        styling::{AnsiColor, Style},
        Styles,
    },
    Parser,
};
use console::style;
use materialbin::{CompiledMaterialDefinition, MinecraftVersion, WriteError};
use scroll::Pread;
use tempfile::tempfile;
use zip::{
    write::{ExtendedFileOptions, FileOptions},
    ZipArchive, ZipWriter,
};

#[derive(Parser)]
#[clap(name = "Material Updater", version = "0.1.13")]
#[command(version, about, long_about = None, styles = get_style())]
struct Options {
    /// Shader pack file to update
    #[clap(required = true)]
    file: String,

    /// Output zip compression level
    #[clap(short, long)]
    zip_compression: Option<u32>,

    /// Process the file, but dont write anything
    #[clap(short, long)]
    yeet: bool,

    /// Output version
    #[clap(short, long)]
    target_version: Option<MVersion>,

    /// Output path
    #[arg(short, long)]
    output: Option<PathBuf>,
}

const fn get_style() -> Styles {
    Styles::styled()
        .header(AnsiColor::BrightYellow.on_default())
        .usage(AnsiColor::Green.on_default())
        .literal(Style::new().fg_color(None).bold())
        .placeholder(AnsiColor::Green.on_default())
}

fn main() -> anyhow::Result<()> {
    let opts = Options::parse();
    
    // Default to the new 26.10.20 if not specified, or fallback to stable
    let target_mversion = opts.target_version.unwrap_or(MVersion::V26_10_20);
    
    // Get the binary version (e.g., 26.10.20 -> 1.21.110 binary format)
    let binary_mcversion = target_mversion.as_version();

    if opts.target_version.is_none() {
        println!(
            "No target version specified, updating to latest preview: 26.10.20 (Binary: {})",
            binary_mcversion
        );
    }

    let mut input_file =
        BufReader::new(File::open(&opts.file).with_context(|| "Error while opening input file")?);
    
    if opts.file.ends_with(".material.bin") {
        let output_filename: PathBuf = match &opts.output {
            Some(output_name) => output_name.to_owned(),
            None => {
                let auto_name = opts.file.to_string().into();
                println!("No output name specified, overwriting input file.");
                auto_name
            }
        };
        let mut tmp_file = tempfile()?;
        let mut output_file = file_to_shrodinger(&mut tmp_file, opts.yeet)?;
        println!("Processing input {}", style(opts.file).cyan());
        
        // Pass the MVersion wrapper so we know if we are doing the 26.10.20 fix
        file_update(&mut input_file, &mut output_file, target_mversion)?;
        
        tmp_file.rewind()?;
        if !opts.yeet {
            let mut output_file = File::create(output_filename)?;
            io::copy(&mut tmp_file, &mut output_file)?;
        }
        return Ok(());
    }
    if opts.file.ends_with(".zip") || opts.file.ends_with(".mcpack") {
        let extension = Path::new(&opts.file)
            .extension()
            .with_context(|| "Input file does not have any extension??, weird")?
            .to_str()
            .unwrap();
        let extension = ".".to_string() + extension;
        let output_filename: PathBuf = match &opts.output {
            Some(output_name) => output_name.to_owned(),
            None => {
                // Use binary version for filename suffix (e.g. _1.21.110.mcpack)
                let auto_name = update_filename(&opts.file, &binary_mcversion, &extension)?;
                println!("No output name specified, using {auto_name:?}");
                auto_name
            }
        };
        let mut tmp_file = tempfile()?;
        let mut output_file = file_to_shrodinger(&mut tmp_file, opts.yeet)?;
        println!("Processing input zip {}", style(opts.file).cyan());
        
        zip_update(
            &mut input_file,
            &mut output_file,
            target_mversion,
            opts.zip_compression,
        )?;
        
        tmp_file.rewind()?;
        if !opts.yeet {
            let mut output_file = File::create(output_filename)?;
            io::copy(&mut tmp_file, &mut output_file)?;
        }
    }
    Ok(())
}

fn file_to_shrodinger<'a>(
    file: &'a mut File,
    dissapear: bool,
) -> anyhow::Result<ShrodingerOutput<'a>> {
    if dissapear {
        Ok(ShrodingerOutput::Nothing)
    } else {
        Ok(ShrodingerOutput::File(file))
    }
}

fn update_filename(
    filename: &str,
    version: &MinecraftVersion,
    postfix: &str,
) -> anyhow::Result<PathBuf> {
    let stripped = filename
        .strip_suffix(postfix)
        .with_context(|| "String does not contain expected postfix")?;
    Ok((stripped.to_string() + "_" + &version.to_string() + postfix).into())
}

// Updated signature: takes MVersion
fn file_update<R, W>(input: &mut R, output: &mut W, version: MVersion) -> anyhow::Result<()>
where
    R: Read + Seek,
    W: Write + Seek,
{
    let mut data = Vec::new();
    let _read = input.read_to_end(&mut data)?;
    let mut material = read_material(&data)?;

    // Check if we need to fix lightmaps for specific versions
    if (material.name == "RenderChunk") && 
       (version == MVersion::V1_21_110 || version == MVersion::V26_10_20) 
    {
        handle_lightmaps(&mut material, version);
    };

    // Write using the underlying binary version
    material.write(output, version.as_version())?;
    Ok(())
}

// Updated signature: takes MVersion
fn zip_update<R, W>(
    input: &mut R,
    output: &mut W,
    version: MVersion,
    compression_level: Option<u32>,
) -> anyhow::Result<()>
where
    R: Read + Seek,
    W: Write + Seek,
{
    let mut input_zip = ZipArchive::new(input)?;
    let mut output_zip = ZipWriter::new(output);
    let mut translated_shaders = 0;
    let mut warnings = 0;
    
    // Extract binary version for the file header writing
    let bin_ver = version.as_version();

    for index in 0..input_zip.len() {
        let mut file = input_zip.by_index(index)?;
        if !file.name().ends_with(".material.bin") {
            output_zip.raw_copy_file(file)?;
            continue;
        }
        print!("Processing file {}", style(file.name()).cyan());
        let mut data = Vec::with_capacity(file.size().try_into()?);
        file.read_to_end(&mut data)?;
        let mut material = match read_material(&data) {
            Ok(material) => material,
            Err(_) => {
                anyhow::bail!("Material file {} is invalid for all versions", file.name());
            }
        };

        // Check if we need to fix lightmaps using the high-level MVersion
        if (material.name == "RenderChunk") && 
           (version == MVersion::V1_21_110 || version == MVersion::V26_10_20) 
        {
            handle_lightmaps(&mut material, version);
        };

        let file_options = FileOptions::<ExtendedFileOptions>::default()
            .compression_level(compression_level.map(|v| v.into()));
        output_zip.start_file(file.name(), file_options)?;
        
        // Write using the binary version
        let result = material.write(&mut output_zip, bin_ver);
        if let Err(err) = result {
            match err {
                WriteError::Compat(issue) => {
                    println!(
                        "{}:\n{}",
                        style("Ignoring materialbin because of compatibility error:")
                            .on_yellow()
                            .red(),
                        issue
                    );
                    translated_shaders -= 1;
                    warnings += 1;
                }
                _ => return Err(err.into()),
            }
            output_zip.abort_file()?;
        }
        translated_shaders += 1;
    }
    output_zip.finish()?;
    if warnings != 0 {
        println!(
            "{}",
            style(format!("{warnings} warnings while updating")).yellow()
        );
    }
    println!(
        "Ported {} materials in zip to version {}",
        style(translated_shaders.to_string()).green(),
        style(bin_ver.to_string()).cyan()
    );
    Ok(())
}

fn read_material(data: &[u8]) -> anyhow::Result<CompiledMaterialDefinition> {
    for version in materialbin::ALL_VERSIONS {
        if let Ok(material) = data.pread_with(0, version) {
            println!("{}", style(format!(" [{version}]")).dim());
            return Ok(material);
        }
    }

    anyhow::bail!("Material file is invalid");
}

enum ShrodingerOutput<'a> {
    File(&'a mut File),
    Nothing,
}
impl<'a> Write for ShrodingerOutput<'a> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        match self {
            Self::File(f) => f.write(buf),
            Self::Nothing => Ok(buf.len()),
        }
    }
    fn flush(&mut self) -> std::io::Result<()> {
        match self {
            Self::File(f) => f.flush(),
            Self::Nothing => Ok(()),
        }
    }
}
impl<'a> Seek for ShrodingerOutput<'a> {
    fn seek(&mut self, pos: std::io::SeekFrom) -> std::io::Result<u64> {
        match self {
            Self::File(f) => f.seek(pos),
            Self::Nothing => Ok(0),
        }
    }
}

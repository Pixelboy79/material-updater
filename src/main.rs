use std::{
    fs::File,
    io::{self, BufReader, Read, Seek, Write},
    path::{Path, PathBuf},
};

use anyhow::Context;
use clap::{
    builder::{
        styling::{AnsiColor, Style},
        Styles,
    },
    Parser, ValueEnum,
};

use materialbin::{
    bgfx_shader::BgfxShader, CompiledMaterialDefinition, MinecraftVersion, WriteError,
};
use owo_colors::{colors::Yellow, OwoColorize};
use scroll::Pread;
use tempfile::tempfile;
use zip::{
    write::{ExtendedFileOptions, FileOptions},
    ZipArchive, ZipWriter,
};

#[derive(Parser)]
#[clap(name = "Material Updater", version = "0.1.12")]
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
    #[clap(short, long)]
    verbose: bool,
    /// Output version
    #[clap(short, long)]
    target_version: Option<MVersion>,

    /// Output path
    #[arg(short, long)]
    output: Option<PathBuf>,
}

#[derive(ValueEnum, Clone)]
enum MVersion {
    V26_10, // NEW: 26.10 Target
    V26_0_24,
    V1_21_110,
    V1_21_20,
    V1_20_80,
    V1_19_60,
    V1_18_30,
}

impl MVersion {
    const fn as_version(&self) -> MinecraftVersion {
        match self {
            Self::V1_20_80 => MinecraftVersion::V1_20_80,
            Self::V1_19_60 => MinecraftVersion::V1_19_60,
            Self::V1_18_30 => MinecraftVersion::V1_18_30,
            Self::V1_21_20 => MinecraftVersion::V1_21_20,
            Self::V1_21_110 => MinecraftVersion::V1_21_110,
            Self::V26_0_24 => MinecraftVersion::V26_0_24,
            Self::V26_10 => MinecraftVersion::V1_21_110, // Binary matches 1.21.110
        }
    }
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
    let target_mversion = match opts.target_version {
        Some(version) => version,
        None => {
            println!("No target version specified, updating to latest stable: V1_21_110");
            MVersion::V1_21_110
        }
    };
    
    let mcversion = target_mversion.as_version();
    let mut input_file =
        BufReader::new(File::open(&opts.file).with_context(|| "Error while opening input file")?);
        
    if opts.file.ends_with(".material.bin") {
        let output_filename: PathBuf = match &opts.output {
            Some(output_name) => output_name.to_owned(),
            None => {
                let auto_name = update_filename(&opts.file, &mcversion, ".material.bin")?;
                println!("No output name specified, using {auto_name:?}");
                auto_name
            }
        };
        let mut tmp_file = tempfile()?;
        let mut output_file = file_to_shrodinger(&mut tmp_file, opts.yeet)?;
        println!("Processing input {}", opts.file.cyan());
        
        file_update(&mut input_file, &mut output_file, &target_mversion, opts.verbose)?;
        
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
                let auto_name = update_filename(&opts.file, &mcversion, &extension)?;
                println!("No output name specified, using {auto_name:?}");
                auto_name
            }
        };
        let mut tmp_file = tempfile()?;
        let mut output_file = file_to_shrodinger(&mut tmp_file, opts.yeet)?;
        println!("Processing input zip {}", opts.file.cyan());
        
        zip_update(
            &mut input_file,
            &mut output_file,
            &target_mversion,
            opts.zip_compression,
            opts.verbose,
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

// INLINE SHADER PATCHES
const LIGHTMAP_26_10_FIX: &[u8] = b"
vec2 lightmapUtil_26_10_new(vec2 tc1) {
    return fract(tc1.y * vec2(256.0, 4096.0));
}
#ifdef a_texcoord1
 #undef a_texcoord1
#endif
#define a_texcoord1 lightmapUtil_26_10_new(a_texcoord1)
";

const SAMPLER_FIX: &[u8] = b"
#if __VERSION__ >= 300
  #define texture(tex,uv) vec4(texture(tex,uv).rgb,textureLod(tex,uv,0.0).a)
#else
  #define texture2D(tex,uv) vec4(texture2D(tex,uv).rgb,texture2DLod(tex,uv,0.0).a)
#endif
";

// Utility to find bytes without adding the memchr dependency to the updater
fn find_subsequence(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|window| window == needle)
}

// THE NEW PATCHING LOGIC
fn patch_material(material: &mut CompiledMaterialDefinition, target_version: &MVersion) {
    let is_26_10 = matches!(target_version, MVersion::V26_10);

    for (_, pass) in material.passes.iter_mut() {
        for variant in pass.variants.iter_mut() {
            for (stage, scode) in variant.shader_codes.iter_mut() {
                let mut bgfx: BgfxShader = match scode.bgfx_shader_data.pread(0) {
                    Ok(s) => s,
                    Err(_) => continue,
                };

                let mut changed = false;

                // 26.10+ Lightmap Y-Component Patch
                if is_26_10 
                    && stage.stage == materialbin::pass::ShaderStage::Vertex 
                    && (stage.platform == materialbin::pass::ShaderCodePlatform::Essl100 || stage.platform == materialbin::pass::ShaderCodePlatform::Essl300) 
                {
                    if find_subsequence(&bgfx.code, b"vec2(256.0, 4096.0)").is_none() {
                        if let Some(pos) = find_subsequence(&bgfx.code, b"void main") {
                            bgfx.code.splice(pos..pos, LIGHTMAP_26_10_FIX.iter().cloned());
                            changed = true;
                        }
                    }
                }

                // Fragment Mipmap Sampler Patch (Opaque / AlphaTest)
                if stage.stage == materialbin::pass::ShaderStage::Fragment && stage.platform_name == "ESSL_100" {
                    if find_subsequence(&bgfx.code, b"textureLod(tex,uv,0.0).a").is_none() {
                        // Using 'void main ()' as the injection point
                        if let Some(pos) = find_subsequence(&bgfx.code, b"void main ()") {
                            bgfx.code.splice(pos..pos, SAMPLER_FIX.iter().cloned());
                            changed = true;
                        }
                    }
                }

                if changed {
                    scode.bgfx_shader_data.clear();
                    let _ = bgfx.write(&mut scode.bgfx_shader_data);
                }
            }
        }
    }
}

fn file_update<R, W>(
    input: &mut R,
    output: &mut W,
    version: &MVersion,
    verbose: bool,
) -> anyhow::Result<()>
where
    R: Read + Seek,
    W: Write + Seek,
{
    let mut data = Vec::new();
    input.read_to_end(&mut data)?;
    
    let mut material = read_material(&data, verbose)?;
    
    // Patch before writing!
    patch_material(&mut material, version);
    material.write(output, version.as_version())?;
    
    Ok(())
}

fn zip_update<R, W>(
    input: &mut R,
    output: &mut W,
    version: &MVersion,
    compression_level: Option<u32>,
    verbose: bool,
) -> anyhow::Result<()>
where
    R: Read + Seek,
    W: Write + Seek,
{
    let mut input_zip = ZipArchive::new(input)?;
    let mut output_zip = ZipWriter::new(output);
    let mut translated_shaders = 0;
    let mut warnings = 0;
    let mut data = Vec::new();
    
    for index in 0..input_zip.len() {
        let mut file = input_zip.by_index(index)?;
        if !file.name().ends_with(".material.bin") {
            output_zip.raw_copy_file(file)?;
            continue;
        }
        print!("Processing file {}", file.name().green());
        data.clear();
        data.reserve(file.size().try_into()?);
        file.read_to_end(&mut data)?;
        
        let mut material = match read_material(&data, verbose) {
            Ok(material) => material,
            Err(_) => {
                anyhow::bail!("Material file {} is invalid for all versions", file.name());
            }
        };
        
        // Patch before writing!
        patch_material(&mut material, version);
        sus(&material);
        
        let file_options = FileOptions::<ExtendedFileOptions>::default()
            .compression_level(compression_level.map(|v| v.into()));
        output_zip.start_file(file.name(), file_options)?;
        
        let result = material.write(&mut output_zip, version.as_version());
        if let Err(err) = result {
            match err {
                WriteError::Compat(issue) => {
                    println!(
                        "{}:\n{}",
                        "Ignoring materialbin because of compatibility error:"
                            .fg::<Yellow>()
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
        println!("{}", format!("{warnings} warnings while updating").yellow());
    }
    println!(
        "Ported {} materials in zip to version {}",
        translated_shaders.to_string().green(),
        version.as_version().to_string().cyan()
    );
    Ok(())
}

fn read_material(data: &[u8], verbose: bool) -> anyhow::Result<CompiledMaterialDefinition> {
    for version in materialbin::ALL_VERSIONS {
        match data.pread_with(0, version) {
            Ok(material) => {
                print!("{}", format!(" [{version}]\n").dimmed());
                return Ok(material);
            }
            Err(e) => {
                if verbose {
                    println!(
                        "Failed [{version}] {}, backtrace:{}",
                        &e,
                        e.get_backtracey()
                    )
                }
            }
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

fn sus(mt: &CompiledMaterialDefinition) {
    for (_, code) in mt
        .passes
        .iter()
        .flat_map(|(_, pass)| &pass.variants)
        .flat_map(|variants| &variants.shader_codes)
    {
        let _sh: BgfxShader = code.bgfx_shader_data.pread(0).unwrap();
    }
}

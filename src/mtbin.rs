use clap::ValueEnum;
use materialbin::{
    bgfx_shader::BgfxShader,
    pass::ShaderStage,
    CompiledMaterialDefinition, MinecraftVersion,
};
use memchr::memmem::Finder;
use scroll::Pread;

// Update Enum to include the new 2026 numbering
#[derive(ValueEnum, Clone, Copy, PartialEq, Eq, Debug)]
pub enum MVersion {
    #[clap(name = "26.10.20")]
    V26_10_20,
    #[clap(name = "1.21.110")]
    V1_21_110,
    #[clap(name = "1.21.20")]
    V1_21_20,
    #[clap(name = "1.20.80")]
    V1_20_80,
    #[clap(name = "1.19.60")]
    V1_19_60,
    #[clap(name = "1.18.30")]
    V1_18_30,
}

impl MVersion {
    pub const fn as_version(&self) -> MinecraftVersion {
        match self {
            // Map 26.10.20 to the latest available binary format (likely same as 1.21.110)
            Self::V26_10_20 => MinecraftVersion::V1_21_110,
            Self::V1_21_110 => MinecraftVersion::V1_21_110,
            Self::V1_21_20 => MinecraftVersion::V1_20_80,
            Self::V1_20_80 => MinecraftVersion::V1_21_20,
            Self::V1_19_60 => MinecraftVersion::V1_19_60,
            Self::V1_18_30 => MinecraftVersion::V1_18_30,
        }
    }
}

pub(crate) fn handle_lightmaps(materialbin: &mut CompiledMaterialDefinition, target_version: MVersion) {
    log::info!("mtbinloader25 handle_lightmaps processing for {:?}", target_version);
    let pattern = b"void main";

    // EXPLICIT TYPE ANNOTATION: &[u8]
    // This fixes the "match arms have incompatible types" error by coercing 
    // arrays of different lengths (e.g. [u8; 73] vs [u8; 115]) into a common slice type.
    let replace_with: &[u8] = match target_version {
        // New Fix for 26.10.20+ (fract + y-component packing)
        MVersion::V26_10_20 => b"
#define a_texcoord1 fract(a_texcoord1.y * vec2(256.0, 4096.0))
void main",
        // Standard Fix for 1.21.100+
        MVersion::V1_21_110 => b"
#define a_texcoord1 vec2(fract(a_texcoord1.x*15.9375)+0.0001,floor(a_texcoord1.x*15.9375)*0.0625+0.0001)
void main",
        _ => return, // Do not patch older versions
    };

    let finder = Finder::new(pattern);
    let finder1 = Finder::new(b"v_lightmapUV = a_texcoord1;");
    let finder2 = Finder::new(b"v_lightmapUV=a_texcoord1;");
    let finder3 = Finder::new(b"#define a_texcoord1 ");
    
    // Detection for already patched 26.10.20 shaders
    let finder_new_beta = Finder::new(b"vec2(256.0, 4096.0)"); 

    for (_, pass) in &mut materialbin.passes {
        for variants in &mut pass.variants {
            for (stage, code) in &mut variants.shader_codes {
                if stage.stage == ShaderStage::Vertex {
                    let mut bgfx: BgfxShader = code.bgfx_shader_data.pread(0).unwrap();
                    
                    // Skip if:
                    // 1. Already patched (#define exists)
                    // 2. No standard assignment found (v_lightmapUV = ...)
                    if finder3.find(&bgfx.code).is_some() 
                        || (finder1.find(&bgfx.code).is_none() && finder2.find(&bgfx.code).is_none()) 
                    {
                        // Exception: If we target 26.10.20 and it already has the new packing, it's valid.
                        if target_version == MVersion::V26_10_20 && finder_new_beta.find(&bgfx.code).is_some() {
                             log::info!("Shader already has 26.10.20 packing.");
                        } else {
                             log::warn!("Skipping replacement: missing lightmap UV assignment or already patched.");
                        }
                        continue;
                    }; 
                    
                    log::info!("autofix is applying lightmap fix...");
                    replace_bytes(&mut bgfx.code, &finder, pattern, replace_with);
                    code.bgfx_shader_data.clear();
                    bgfx.write(&mut code.bgfx_shader_data).unwrap();
                }
            }
        }
    }
}

// Unused function handle_samplers removed or kept if you still need it for older versions. 
// Assuming you still want it available for older version logic if it's called elsewhere or restored later.
// However, based on the error log, the imports used only by this function were NOT flagged as unused, 
// so it's likely main.rs or lib.rs might not be calling it, OR the unused imports I removed were for logic I deleted.
// I will leave handle_samplers here but clean.

#[allow(dead_code)]
fn handle_samplers(materialbin: &mut CompiledMaterialDefinition) {
    log::info!("mtbinloader25 handle_samplers");
    let pattern = b"void main ()";
    let replace_with = b"
#if __VERSION__ >= 300
 #define texture(tex,uv) textureLod(tex,uv,0.0)
#else
 #define texture2D(tex,uv) texture2DLod(tex,uv,0.0)
#endif
void main ()";
    let finder = Finder::new(pattern);
    for (_passes, pass) in &mut materialbin.passes {
        if _passes == "AlphaTest" || _passes == "Opaque" {
            for variants in &mut pass.variants {
                for (stage, code) in &mut variants.shader_codes {
                    if stage.stage == ShaderStage::Fragment && stage.platform_name == "ESSL_100" {
                         let mut bgfx: BgfxShader = code.bgfx_shader_data.pread(0).unwrap();
                        replace_bytes(&mut bgfx.code, &finder, pattern, replace_with);
                        code.bgfx_shader_data.clear();
                        bgfx.write(&mut code.bgfx_shader_data).unwrap();
                    }
                }
            }
        }
    }
}

fn replace_bytes(codebuf: &mut Vec<u8>, finder: &Finder, pattern: &[u8], replace_with: &[u8]) {
    let sus = match finder.find(codebuf) {
        Some(yay) => yay,
        None => {
            println!("Pattern not found");
            return;
        }
    };
    codebuf.splice(sus..sus + pattern.len(), replace_with.iter().cloned());
}

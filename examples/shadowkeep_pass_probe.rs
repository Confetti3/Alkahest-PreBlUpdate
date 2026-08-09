use std::str::FromStr;

use anyhow::{Context, Result};
use alkahest_data::tfx::shadowkeep::{SShadowkeepTechnique, ShadowkeepEraProfile};
use tiger_parse::PackageManagerExt;
use tiger_pkg::{TagHash, package_manager};

fn main() -> Result<()> {
    let packages = std::env::args().nth(1).context("usage: shadowkeep_pass_probe <packages-dir>")?;
    alkahest_core::initialize_package_manager(None, Some(packages.as_str()))?;
    let light_tag = TagHash::from_str("80C58336")?;
    let light: SShadowkeepTechnique = package_manager().read_tag_struct(light_tag)?;
    println!(
        "light tag={light_tag} bind={:?} states=0x{:08x} scopes=0x{:08x}",
        light.bind_mode, light.states.0, light.used_scopes.0
    );
    for (label, shader) in [("vs", &light.shader_vertex), ("ps", &light.shader_pixel)] {
        println!(
            "  light {label}: shader={} textures={:?} bytecode={:02x?} constants={:?} inline={:?} samplers={:?} cbuffer_slot={} cbuffer={}",
            shader.shader,
            shader.textures.iter().map(|t| (t.slot, t.texture)).collect::<Vec<_>>(),
            shader.constants.bytecode,
            shader.constants.bytecode_constants,
            shader.constants.inline_constants,
            shader.constants.samplers.iter().map(|s| s.sampler).collect::<Vec<_>>(),
            shader.constants.constant_buffer_slot,
            shader.constants.constant_buffer,
        );
    }
    let bootstrap = ShadowkeepEraProfile.load_bootstrap()?;
    for name in [
        "global_lighting",
        "deferred_shading",
        "deferred_shading_no_atm",
        "sky_generate_sky_mask",
        "sky_lookup_generate",
        "sky_direction_lookup_generate",
        "sky",
    ] {
        let tag = bootstrap.pipelines.get(name).copied().with_context(|| format!("{name} is absent"))?;
        let technique: SShadowkeepTechnique = package_manager().read_tag_struct(tag)?;
        println!("{name} tag={tag} bind={:?} states=0x{:08x} scopes=0x{:08x}", technique.bind_mode, technique.states.0, technique.used_scopes.0);
        for (label, shader) in [("vs", &technique.shader_vertex), ("ps", &technique.shader_pixel)] {
            println!("  {label}: shader={} cbuffer_slot={} cbuffer={} bytecode={} constants={} inline={} samplers={} textures={}",
                shader.shader, shader.constants.constant_buffer_slot, shader.constants.constant_buffer,
                shader.constants.bytecode.len(), shader.constants.bytecode_constants.len(),
                shader.constants.inline_constants.len(), shader.constants.samplers.len(), shader.textures.len());
            println!("    bytecode={:02x?}", shader.constants.bytecode);
            println!("    inline_constants[0..8]={:?}", &shader.constants.inline_constants[..shader.constants.inline_constants.len().min(8)]);
            println!("    textures={:?}", shader.textures.iter().map(|t| (t.slot, t.texture)).collect::<Vec<_>>());
        }
    }
    Ok(())
}

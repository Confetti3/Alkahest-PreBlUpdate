//! Read-only validation probe for the preserved Shadowkeep renderer bootstrap.

use anyhow::Context;
use tiger_parse::PackageManagerExt;
use tiger_pkg::package_manager;

fn main() -> anyhow::Result<()> {
    let packages = std::env::args().nth(1).context("usage: shadowkeep_renderer_probe <packages-dir>")?;
    alkahest_core::initialize_package_manager(None, Some(packages.as_str()))?;

    let bootstrap = alkahest_data::tfx::shadowkeep::ShadowkeepEraProfile.load_bootstrap()?;
    println!("{} scopes, {} pipelines, {} input layouts", bootstrap.scopes.len(), bootstrap.pipelines.len(), bootstrap.input_layout_count);
    let channel_tag = bootstrap.channel_defaults;
    let channel_entry = package_manager()
        .get_entry(channel_tag)
        .with_context(|| format!("missing channel-default entry {channel_tag}"))?;
    let channel_bytes = package_manager().read_tag(channel_tag)?;
    println!(
        "channel_defaults tag={channel_tag} reference=0x{:08X} class={:02X}:{:02X} declared_size={} raw_len={}",
        channel_entry.reference,
        channel_entry.file_type,
        channel_entry.file_subtype,
        channel_entry.file_size,
        channel_bytes.len(),
    );
    println!(
        "channel_defaults hex={}",
        channel_bytes.iter().map(|byte| format!("{byte:02x}")).collect::<String>()
    );

    let mut absent_scopes = Vec::new();
    for (name, tag) in &bootstrap.scopes {
        if tag.is_none() {
            absent_scopes.push(name);
            continue;
        }
        package_manager()
            .read_tag_struct::<alkahest_data::tfx::shadowkeep::SShadowkeepScope>(*tag)
            .with_context(|| format!("failed to parse Shadowkeep scope {name} ({tag})"))?;
    }
    let mut absent_pipelines = Vec::new();
    for (name, tag) in &bootstrap.pipelines {
        if tag.is_none() {
            absent_pipelines.push(name);
            continue;
        }
        package_manager()
            .read_tag_struct::<alkahest_data::tfx::shadowkeep::SShadowkeepTechnique>(*tag)
            .with_context(|| format!("failed to parse Shadowkeep technique {name} ({tag})"))?;
    }
    println!("all referenced scopes and techniques parsed; {} scopes and {} techniques are explicitly null", absent_scopes.len(), absent_pipelines.len());
    let mut atmosphere_pipelines = bootstrap
        .pipelines
        .iter()
        .filter(|(name, _)| {
            name.contains("sky")
                || name.contains("atmosphere")
                || name.starts_with("cubemap_apply_cube_")
        })
        .collect::<Vec<_>>();
    atmosphere_pipelines.sort_unstable_by_key(|(name, _)| name.as_str());
    println!("sky/atmosphere pipelines:");
    for (name, tag) in atmosphere_pipelines {
        println!("  {name}={tag}");
    }
    for name in [
        "global_lighting",
        "deferred_shading",
        "deferred_shading_no_atm",
        "final_combine",
        "fxaa",
        "downsample_depth_buffer",
        "uber_depth_default",
        "sky_generate_sky_mask",
        "sky_lookup_generate_near",
        "sky_lookup_generate_far",
        "sky",
    ] {
        match bootstrap.pipelines.get(name) {
            Some(tag) if tag.is_some() => println!("pipeline {name}=ready ({tag})"),
            Some(_) => println!("pipeline {name}=explicit-null"),
            None => println!("pipeline {name}=missing"),
        }
    }
    Ok(())
}

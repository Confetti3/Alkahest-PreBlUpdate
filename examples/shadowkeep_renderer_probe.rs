//! Read-only validation probe for the preserved Shadowkeep renderer bootstrap.

use anyhow::Context;
use tiger_parse::PackageManagerExt;
use tiger_pkg::package_manager;

fn main() -> anyhow::Result<()> {
    let packages = std::env::args().nth(1).context("usage: shadowkeep_renderer_probe <packages-dir>")?;
    alkahest_core::initialize_package_manager(None, Some(packages.as_str()))?;

    let bootstrap = alkahest_data::tfx::shadowkeep::ShadowkeepEraProfile.load_bootstrap()?;
    println!("{} scopes, {} pipelines, {} input layouts", bootstrap.scopes.len(), bootstrap.pipelines.len(), bootstrap.input_layout_count);

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
    for name in [
        "global_lighting",
        "deferred_shading",
        "deferred_shading_no_atm",
        "final_combine",
        "fxaa",
        "downsample_depth_buffer",
        "uber_depth_default",
    ] {
        match bootstrap.pipelines.get(name) {
            Some(tag) if tag.is_some() => println!("pipeline {name}=ready ({tag})"),
            Some(_) => println!("pipeline {name}=explicit-null"),
            None => println!("pipeline {name}=missing"),
        }
    }
    Ok(())
}

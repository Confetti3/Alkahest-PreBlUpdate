//! Walk aligned package-tag references from a preserved resource.
//!
//! Usage: `cargo run --example shadowkeep_tag_probe -- <packages-dir> <tag> [depth]`.

use std::{collections::BTreeSet, str::FromStr};

use anyhow::{Context, Result};
use alkahest_data::shadowkeep::SShadowkeepEntityResource;
use tiger_parse::PackageManagerExt;
use tiger_pkg::{TagHash, package_manager};

const PREVIEW_LIMIT: usize = 0x100;

fn walk(tag: TagHash, depth: usize, visited: &mut BTreeSet<TagHash>, indent: usize) -> Result<()> {
    if !visited.insert(tag) {
        println!("{:indent$}{tag} (visited)", "");
        return Ok(());
    }
    let manager = package_manager();
    let entry = manager
        .get_entry(tag)
        .with_context(|| format!("missing package entry {tag}"))?;
    let bytes = manager.read_tag(tag)?;
    println!(
        "{:indent$}{tag} reference=0x{:08X} class={:02X}:{:02X} size={}",
        "",
        entry.reference,
        entry.file_type,
        entry.file_subtype,
        bytes.len(),
    );
    let preview = &bytes[..bytes.len().min(PREVIEW_LIMIT)];
    println!(
        "{:indent$}hex={}",
        "",
        preview
            .iter()
            .map(|byte| format!("{byte:02X}"))
            .collect::<String>(),
        indent = indent + 2,
    );
    if entry.reference == 0x8080_9C36 {
        let resource: SShadowkeepEntityResource = manager.read_tag_struct(tag)?;
        println!("{:indent$}resource={resource:#?}", "", indent = indent + 2);
    }
    if depth == 0 {
        return Ok(());
    }

    let mut children = BTreeSet::new();
    for chunk in bytes.chunks_exact(4) {
        let candidate = TagHash(u32::from_le_bytes(chunk.try_into().unwrap()));
        if candidate != tag && manager.get_entry(candidate).is_some() {
            children.insert(candidate);
        }
    }
    for child in children {
        walk(child, depth - 1, visited, indent + 2)?;
    }
    Ok(())
}

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let packages = args
        .next()
        .context("usage: shadowkeep_tag_probe <packages-dir> <tag> [depth]")?;
    let tag = TagHash::from_str(
        &args
            .next()
            .context("usage: shadowkeep_tag_probe <packages-dir> <tag> [depth]")?,
    )?;
    let depth = args
        .next()
        .map(|value| value.parse())
        .transpose()
        .context("depth must be an integer")?
        .unwrap_or(2);
    alkahest_core::initialize_package_manager(None, Some(packages.as_str()))?;
    walk(tag, depth, &mut BTreeSet::new(), 0)
}

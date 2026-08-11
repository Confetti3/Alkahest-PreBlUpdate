//! Enumerate preserved table-local map-atmosphere resources and lookup textures.
//!
//! Usage: `cargo run --example shadowkeep_atmosphere_probe -- <packages-dir> [package-name-filter]`.

use std::{
    collections::BTreeMap,
    io::{Cursor, Seek, SeekFrom},
};

use alkahest_data::{
    shadowkeep::{SShadowkeepMapDataTable, SShadowkeepTextureHeader},
    tfx::atmosphere::SAtmosphereDataComponent,
};
use anyhow::{Context, Result};
use tiger_parse::{PackageManagerExt, TigerReadable};
use tiger_pkg::{TagHash, TagHash64, package_manager};

const MAP_DATA_TABLE_CLASS: u32 = 0x8080_99D6;
const MAP_ATMOSPHERE_CLASS: u32 = 0x8080_6BC1;
fn resolve_texture_wide(bytes: &[u8], offset: usize) -> Option<TagHash> {
    let hash32 = TagHash(u32::from_le_bytes(
        bytes.get(offset..offset + 4)?.try_into().ok()?,
    ));
    let is_hash32 = u32::from_le_bytes(bytes.get(offset + 4..offset + 8)?.try_into().ok()?);
    let tag = if is_hash32 != 0 {
        hash32.is_some().then_some(hash32)?
    } else {
        let hash64 = u64::from_le_bytes(bytes.get(offset + 8..offset + 16)?.try_into().ok()?);
        package_manager().lookup.tag64_entries.get(&hash64)?.hash32
    };
    package_manager().get_entry(tag).is_some().then_some(tag)
}

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let packages = args
        .next()
        .context("usage: shadowkeep_atmosphere_probe <packages-dir> [package-name-filter]")?;
    let package_filter = args.next();
    alkahest_core::initialize_package_manager(None, Some(packages.as_str()))?;

    let manager = package_manager();
    let atmosphere_tags = package_manager().get_all_by_reference(MAP_ATMOSPHERE_CLASS);
    println!("atmosphere_class_tags={}", atmosphere_tags.len());
    for (tag, entry) in atmosphere_tags.iter().take(20) {
        let package_name = &package_manager().package_paths[&tag.pkg_id()].name;
        println!(
            "  atmosphere_tag={tag} reference=0x{:08X} class={:02X}:{:02X} package={package_name}",
            entry.reference, entry.file_type, entry.file_subtype,
        );
    }

    for hash in [
        TagHash64(0x36F0_C0D2_9A44_0000),
        TagHash64(0x4193_74B9_90AB_0000),
    ] {
        println!(
            "fallback_lookup={hash} resolved={:?}",
            manager
                .lookup
                .tag64_entries
                .get(&hash.0)
                .map(|entry| entry.hash32)
        );
    }
    println!(
        "fallback_vertical=80BD7A1E resolved={}",
        manager.get_entry(TagHash(0x80BD_7A1E)).is_some()
    );
    let mut tables_scanned = 0usize;
    let mut atmosphere_resources = 0usize;
    let mut lookup_candidates = 0usize;
    for (table_tag, _) in manager.get_all_by_reference(MAP_DATA_TABLE_CLASS) {
        let package_name = &manager.package_paths[&table_tag.pkg_id()].name;
        if package_filter
            .as_ref()
            .is_some_and(|filter| !package_name.contains(filter))
        {
            continue;
        }
        tables_scanned += 1;
        let table: SShadowkeepMapDataTable = manager.read_tag_struct(table_tag)?;
        let bytes = manager.read_tag(table_tag)?;
        for entry in &table.data_entries {
            if entry.data_resource.is_valid {
                let start = usize::try_from(entry.data_resource.offset).unwrap_or(bytes.len());
                let end = start.saturating_add(0x400).min(bytes.len());
                for offset in (start..end).step_by(8) {
                    let lookups = [offset, offset + 0x10, offset + 0x20, offset + 0x30]
                        .map(|candidate| resolve_texture_wide(&bytes, candidate));
                    if lookups.iter().all(Option::is_some) {
                        lookup_candidates += 1;
                        println!(
                            "lookup_candidate table={table_tag} package={package_name} \
                             type=0x{:08X} offset=0x{start:X}+0x{:X} textures={:?}",
                            entry.data_resource.resource_type,
                            offset - start,
                            lookups.map(Option::unwrap),
                        );
                    }
                }
            }
            if entry.data_resource.resource_type != MAP_ATMOSPHERE_CLASS {
                continue;
            }
            atmosphere_resources += 1;
            let mut cursor = Cursor::new(bytes.as_slice());
            cursor.seek(SeekFrom::Start(entry.data_resource.offset + 0x10))?;
            let atmosphere = SAtmosphereDataComponent::read_ds(&mut cursor).with_context(|| {
                format!(
                    "reading atmosphere {table_tag}@0x{:X}",
                    entry.data_resource.offset
                )
            })?;
            let lookups = [
                atmosphere.unk80_tex.hash32_checked(),
                atmosphere.unk90_tex.hash32_checked(),
                atmosphere.unka0_tex.hash32_checked(),
                atmosphere.unkb0_tex.hash32_checked(),
            ];
            println!(
                "atmosphere table={table_tag} package={package_name} offset=0x{:X}",
                entry.data_resource.offset,
            );
            for (index, texture_tag) in lookups.into_iter().enumerate() {
                let Some(texture_tag) = texture_tag else {
                    println!("  lookup_{index}=unresolved");
                    continue;
                };
                let texture: SShadowkeepTextureHeader = manager
                    .read_tag_struct(texture_tag)
                    .with_context(|| format!("reading atmosphere lookup {texture_tag}"))?;
                println!(
                    "  lookup_{index}={texture_tag} {}x{}x{} array={} mips={} format={:?}",
                    texture.width,
                    texture.height,
                    texture.depth,
                    texture.array_size,
                    texture.mip_count,
                    texture.format,
                );
            }
        }
    }
    println!(
        "tables_scanned={tables_scanned} map_atmosphere_resources={atmosphere_resources} \
         lookup_candidates={lookup_candidates}"
    );
    let mut texture_shapes = BTreeMap::<(u16, u16, u16, u16, u8, String), (usize, TagHash)>::new();
    for (tag, _) in manager.get_all_by_type(0x20, None) {
        let package_name = &manager.package_paths[&tag.pkg_id()].name;
        if package_filter
            .as_ref()
            .is_some_and(|filter| !package_name.contains(filter))
        {
            continue;
        }
        let Ok(texture) = manager.read_tag_struct::<SShadowkeepTextureHeader>(tag) else {
            continue;
        };
        if texture.width > 512 || texture.height > 512 {
            continue;
        }
        let key = (
            texture.width,
            texture.height,
            texture.depth,
            texture.array_size,
            texture.mip_count,
            format!("{:?}", texture.format),
        );
        let summary = texture_shapes.entry(key).or_insert((0, tag));
        summary.0 += 1;
    }
    for ((width, height, depth, array, mips, format), (count, sample)) in texture_shapes {
        println!(
            "texture_shape={width}x{height}x{depth} array={array} mips={mips} format={format} \
             count={count} sample={sample}"
        );
    }
    Ok(())
}

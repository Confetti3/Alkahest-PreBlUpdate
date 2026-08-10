//! Inspect table-local Shadowkeep map resources without assigning semantics.
//!
//! Usage: `cargo run --example shadowkeep_map_resource_probe -- <packages-dir> <bubble-tag> [resource-class]`.

use std::{
    collections::BTreeMap,
    io::{Cursor, Seek, SeekFrom},
    str::FromStr,
};

use alkahest_data::shadowkeep::{
    SShadowkeepBubbleDefinition, SShadowkeepBubbleParent, SShadowkeepCubemapPlacement,
    SShadowkeepEntity, SShadowkeepMapDataTable, SShadowkeepTextureHeader,
};
use anyhow::{Context, Result};
use tiger_parse::{PackageManagerExt, TigerReadable};
use tiger_pkg::{TagHash, package_manager};

const PREVIEW_LIMIT: usize = 0x300;
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
    package_manager()
        .get_entry(tag)
        .is_some_and(|entry| entry.file_type == 0x20)
        .then_some(tag)
}

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let packages = args
        .next()
        .context("usage: shadowkeep_map_resource_probe <packages-dir> <bubble-tag>")?;
    let bubble_tag = TagHash::from_str(
        &args
            .next()
            .context("usage: shadowkeep_map_resource_probe <packages-dir> <bubble-tag>")?,
    )?;
    let resource_filter = args
        .next()
        .map(|value| u32::from_str_radix(value.trim_start_matches("0x"), 16))
        .transpose()
        .context("resource class must be hexadecimal")?;
    alkahest_core::initialize_package_manager(None, Some(packages.as_str()))?;

    let manager = package_manager();
    let parent: SShadowkeepBubbleParent = manager.read_tag_struct(bubble_tag)?;
    let definition: SShadowkeepBubbleDefinition = manager.read_tag_struct(parent.child_map)?;
    let mut counts = BTreeMap::<u32, usize>::new();
    let mut first_previews = BTreeMap::<u32, (TagHash, usize, Vec<u8>)>::new();
    let mut entity_resource_counts = BTreeMap::<u32, usize>::new();
    let mut lookup_candidates = Vec::new();

    for container in &definition.map_resources {
        for &table_tag in &container.data_tables {
            let table: SShadowkeepMapDataTable = manager.read_tag_struct(table_tag)?;
            let table_bytes = manager.read_tag(table_tag)?;
            let mut offsets = table
                .data_entries
                .iter()
                .filter(|entry| entry.data_resource.is_valid)
                .filter_map(|entry| usize::try_from(entry.data_resource.offset).ok())
                .collect::<Vec<_>>();
            offsets.sort_unstable();
            offsets.dedup();

            for entry in &table.data_entries {
                if entry.entity.is_some()
                    && let Ok(entity) = manager.read_tag_struct::<SShadowkeepEntity>(entry.entity)
                {
                    for resource_ref in &entity.entity_resources {
                        let resource = &*resource_ref.resource;
                        if resource.definition.is_valid {
                            *entity_resource_counts
                                .entry(resource.resource.resource_type)
                                .or_default() += 1;
                        }
                    }
                }
                if !entry.data_resource.is_valid {
                    continue;
                }
                let resource_type = entry.data_resource.resource_type;
                *counts.entry(resource_type).or_default() += 1;
                if resource_filter.is_some_and(|filter| filter == resource_type) {
                    println!(
                        "placement table={table_tag} offset=0x{:X} translation={:?} rotation={:?}",
                        entry.data_resource.offset, entry.translation, entry.rotation
                    );
                }
                if resource_type == 0x8080_6B7F {
                    let mut cursor = Cursor::new(table_bytes.as_slice());
                    cursor.seek(SeekFrom::Start(entry.data_resource.offset))?;
                    let cubemap = SShadowkeepCubemapPlacement::read_ds(&mut cursor)?.normalized();
                    println!(
                        "  center={:?} extents={:?} specular={} alpha={} voxel={}",
                        cubemap.volume_center,
                        cubemap.volume_extents,
                        cubemap.texture_cube_specular_ibl,
                        cubemap.texture_cube_alpha,
                        cubemap.texture_voxel_diffuse,
                    );
                }
                let start = usize::try_from(entry.data_resource.offset)
                    .context("resource offset exceeds addressable memory")?;
                let search_end = start.saturating_add(0x400).min(table_bytes.len());
                for offset in (start..search_end).step_by(8) {
                    let lookups = [offset, offset + 0x10, offset + 0x20, offset + 0x30]
                        .map(|candidate| resolve_texture_wide(&table_bytes, candidate));
                    if lookups.iter().all(Option::is_some) {
                        lookup_candidates.push((
                            table_tag,
                            resource_type,
                            start,
                            offset - start,
                            lookups.map(Option::unwrap),
                        ));
                    }
                }
                if resource_filter.is_some_and(|filter| filter != resource_type)
                    || first_previews.contains_key(&resource_type)
                {
                    continue;
                }
                let start = usize::try_from(entry.data_resource.offset)
                    .context("resource offset exceeds addressable memory")?;
                let end = offsets
                    .iter()
                    .copied()
                    .find(|offset| *offset > start)
                    .unwrap_or(table_bytes.len())
                    .min(start.saturating_add(PREVIEW_LIMIT))
                    .min(table_bytes.len());
                if start < end {
                    first_previews.insert(
                        resource_type,
                        (table_tag, start, table_bytes[start..end].to_vec()),
                    );
                }
            }
        }
    }

    println!(
        "bubble={bubble_tag} package={} child={} resource_classes={}",
        manager.package_paths[&bubble_tag.pkg_id()].name,
        parent.child_map,
        counts.len()
    );
    for (resource_type, count) in counts {
        println!("class=0x{resource_type:08X} count={count}");
        let Some((table_tag, start, preview)) = first_previews.get(&resource_type) else {
            continue;
        };
        println!(
            "  table={table_tag} offset=0x{start:X} preview_len=0x{:X}",
            preview.len()
        );
        println!(
            "  hex={}",
            preview
                .iter()
                .map(|byte| format!("{byte:02X}"))
                .collect::<String>()
        );
        for (wide_index, chunk) in preview.chunks_exact(8).enumerate() {
            let hash64 = u64::from_le_bytes(chunk.try_into().unwrap());
            if let Some(wide_entry) = manager.lookup.tag64_entries.get(&hash64)
                && let Some(package_entry) = manager.get_entry(wide_entry.hash32)
            {
                println!(
                    "  wide+0x{:02X}=0x{hash64:016X} -> {} reference=0x{:08X} class={:02X}:{:02X} size={}",
                    wide_index * 8,
                    wide_entry.hash32,
                    package_entry.reference,
                    package_entry.file_type,
                    package_entry.file_subtype,
                    package_entry.file_size,
                );
            }
        }
        for (word_index, chunk) in preview.chunks_exact(4).enumerate() {
            let tag = TagHash(u32::from_le_bytes(chunk.try_into().unwrap()));
            if tag.is_some()
                && let Some(package_entry) = manager.get_entry(tag)
            {
                println!(
                    "  tag+0x{:02X}={} reference=0x{:08X} class={:02X}:{:02X} size={}",
                    word_index * 4,
                    tag,
                    package_entry.reference,
                    package_entry.file_type,
                    package_entry.file_subtype,
                    package_entry.file_size,
                );
                if package_entry.file_type == 0x20
                    && let Ok(texture) = manager.read_tag_struct::<SShadowkeepTextureHeader>(tag)
                {
                    println!(
                        "    texture={}x{}x{} array={} mips={} format={:?}",
                        texture.width,
                        texture.height,
                        texture.depth,
                        texture.array_size,
                        texture.mip_count,
                        texture.format,
                    );
                }
                if let Ok(tag_bytes) = manager.read_tag(tag) {
                    let tag_preview = &tag_bytes[..tag_bytes.len().min(PREVIEW_LIMIT)];
                    println!(
                        "    tag_hex={}",
                        tag_preview
                            .iter()
                            .map(|byte| format!("{byte:02X}"))
                            .collect::<String>()
                    );
                    for (tag_word_index, tag_chunk) in tag_bytes.chunks_exact(4).enumerate() {
                        let nested_tag = TagHash(u32::from_le_bytes(tag_chunk.try_into().unwrap()));
                        if nested_tag.is_some()
                            && let Some(nested_entry) = manager.get_entry(nested_tag)
                        {
                            if nested_entry.file_type != 0x20 {
                                continue;
                            }
                            println!(
                                "    nested_tag+0x{:04X}={} reference=0x{:08X} class={:02X}:{:02X} size={}",
                                tag_word_index * 4,
                                nested_tag,
                                nested_entry.reference,
                                nested_entry.file_type,
                                nested_entry.file_subtype,
                                nested_entry.file_size,
                            );
                            if nested_entry.file_type == 0x20
                                && let Ok(texture) =
                                    manager.read_tag_struct::<SShadowkeepTextureHeader>(nested_tag)
                            {
                                println!(
                                    "      texture={}x{}x{} array={} mips={} format={:?}",
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
                }
            }
        }
    }
    println!("entity resource classes:");
    for (resource_type, count) in entity_resource_counts {
        println!("  class=0x{resource_type:08X} count={count}");
    }

    for (table, resource_type, start, relative, lookups) in lookup_candidates {
        println!(
            "lookup_candidate table={table} type=0x{resource_type:08X} offset=0x{start:X}+0x{relative:X} textures={lookups:?}"
        );
    }
    Ok(())
}

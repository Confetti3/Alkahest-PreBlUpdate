//! Trace preserved ambient scenario layers to activity map data tables.
//!
//! Usage: `cargo run --example shadowkeep_activity_probe -- <packages-dir> <bubble-tag>`.

use std::{
    collections::{BTreeMap, BTreeSet},
    str::FromStr,
};

use alkahest_data::shadowkeep::{
    SShadowkeepEntityResource, SShadowkeepMapDataTable, SShadowkeepTextureHeader,
};
use anyhow::{Context, Result};
use tiger_parse::PackageManagerExt;
use tiger_pkg::{TagHash, package_manager};

fn referenced_tags_with_class(bytes: &[u8], reference: u32) -> BTreeSet<TagHash> {
    let manager = package_manager();
    bytes
        .chunks_exact(4)
        .map(|chunk| TagHash(u32::from_le_bytes(chunk.try_into().unwrap())))
        .filter(|tag| {
            tag.is_some()
                && manager
                    .get_entry(*tag)
                    .is_some_and(|entry| entry.reference == reference)
        })
        .collect()
}
fn resolve_texture_wide(bytes: &[u8], offset: usize) -> Option<TagHash> {
    let hash32 = TagHash(u32::from_le_bytes(
        bytes.get(offset..offset + 4)?.try_into().ok()?,
    ));
    let is_hash32 = u32::from_le_bytes(
        bytes.get(offset + 4..offset + 8)?.try_into().ok()?,
    );
    let tag = if is_hash32 != 0 {
        hash32.is_some().then_some(hash32)?
    } else {
        let hash64 = u64::from_le_bytes(
            bytes.get(offset + 8..offset + 16)?.try_into().ok()?,
        );
        package_manager().lookup.tag64_entries.get(&hash64)?.hash32
    };
    package_manager().get_entry(tag).is_some().then_some(tag)
}

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let packages = args
        .next()
        .context("usage: shadowkeep_activity_probe <packages-dir> <bubble-tag>")?;
    let map_tag = TagHash::from_str(
        &args
            .next()
            .context("usage: shadowkeep_activity_probe <packages-dir> <bubble-tag>")?,
    )?;
    alkahest_core::initialize_package_manager(None, Some(packages.as_str()))?;
    let manager = package_manager();
    let package_name = &manager.package_paths[&map_tag.pkg_id()].name;
    let scenarios = manager.get_named_tags_by_class(0x8080_9994);
    for (name, tag) in scenarios
        .iter()
        .filter(|(name, _)| name.contains(package_name))
    {
        println!("scenario_name={name} tag={tag}");
    }
    let scenario_name = args
        .next()
        .unwrap_or_else(|| format!("{package_name}_freeroam:scenario_client"));
    let scenario_tag = scenarios
        .into_iter()
        .find_map(|(name, tag)| (name == scenario_name).then_some(tag))
        .with_context(|| format!("missing named scenario {scenario_name}"))?;

    let scenario = manager.read_tag(scenario_tag)?;
    let phase_records = referenced_tags_with_class(&scenario, 0x8080_925B);
    let mut phase_roots = BTreeSet::new();
    for record in &phase_records {
        phase_roots.extend(referenced_tags_with_class(
            &manager.read_tag(*record)?,
            0x8080_925E,
        ));
    }
    let mut entity_resources = BTreeSet::new();
    for root in &phase_roots {
        entity_resources.extend(referenced_tags_with_class(
            &manager.read_tag(*root)?,
            0x8080_9462,
        ));
    }
    let mut wrappers = BTreeSet::new();
    for resource in &entity_resources {
        wrappers.extend(referenced_tags_with_class(
            &manager.read_tag(*resource)?,
            0x8080_9468,
        ));
    }
    let mut intermediate_resources = BTreeSet::new();
    let mut data_tables = BTreeSet::new();
    for wrapper in &wrappers {
        let bytes = manager.read_tag(*wrapper)?;
        intermediate_resources.extend(referenced_tags_with_class(&bytes, 0x8080_9B14));
        data_tables.extend(referenced_tags_with_class(&bytes, 0x8080_99D6));
    }
    let mut entity_definitions = BTreeSet::new();
    for intermediate in &intermediate_resources {
        entity_definitions.extend(referenced_tags_with_class(
            &manager.read_tag(*intermediate)?,
            0x8080_9C36,
        ));
    }
    let mut resource_types = BTreeMap::<u32, usize>::new();
    let mut resource_textures = BTreeMap::<u32, BTreeSet<TagHash>>::new();
    let mut resource_samples = BTreeMap::<u32, TagHash>::new();
    let mut entity_lookup_candidates = Vec::new();
    let mut interesting_textures = Vec::new();
    for entity_definition in &entity_definitions {
        let resource: SShadowkeepEntityResource =
            manager.read_tag_struct(*entity_definition)?;
        let resource_type = resource.resource.resource_type;
        *resource_types.entry(resource_type).or_default() += 1;
        resource_samples
            .entry(resource_type)
            .or_insert(*entity_definition);
        let bytes = manager.read_tag(*entity_definition)?;
        let start = usize::try_from(resource.resource.offset).unwrap_or(bytes.len());
        let end = usize::try_from(resource.definition.offset)
            .unwrap_or(bytes.len())
            .min(bytes.len());
        if start < end {
            for chunk in bytes[start..end].chunks_exact(4) {
                let candidate = TagHash(u32::from_le_bytes(chunk.try_into().unwrap()));
                if manager
                    .get_entry(candidate)
                    .is_some_and(|entry| entry.file_type == 0x20)
                {
                    resource_textures
                        .entry(resource_type)
                        .or_default()
                        .insert(candidate);
                    if let Ok(texture) =
                        manager.read_tag_struct::<SShadowkeepTextureHeader>(candidate)
                        && (texture.depth > 1
                            || format!("{:?}", texture.format).contains("Float"))
                    {
                        interesting_textures.push((
                            *entity_definition,
                            resource_type,
                            candidate,
                            texture.width,
                            texture.height,
                            texture.depth,
                            texture.array_size,
                            texture.mip_count,
                            format!("{:?}", texture.format),
                        ));
                    }
                }
            }
            for offset in (start..end).step_by(8) {
                let lookups = [offset, offset + 0x10, offset + 0x20, offset + 0x30]
                    .map(|candidate| resolve_texture_wide(&bytes, candidate));
                if lookups.iter().all(Option::is_some) {
                    entity_lookup_candidates.push((
                        *entity_definition,
                        resource_type,
                        offset - start,
                        lookups.map(Option::unwrap),
                    ));
                }
            }
        }
    }
    let mut map_resource_types = BTreeMap::<u32, usize>::new();
    let mut map_resource_samples = BTreeMap::<u32, (TagHash, usize, Vec<u8>)>::new();
    let mut lookup_candidates = Vec::new();
    for table_tag in &data_tables {
        let table: SShadowkeepMapDataTable = manager.read_tag_struct(*table_tag)?;
        let table_bytes = manager.read_tag(*table_tag)?;
        for entry in table.data_entries {
            let resource_type = entry.data_resource.resource_type;
            *map_resource_types.entry(resource_type).or_default() += 1;
            if !entry.data_resource.is_valid {
                continue;
            }
            let start = usize::try_from(entry.data_resource.offset).unwrap_or(table_bytes.len());
            if start < table_bytes.len() {
                let end = start.saturating_add(0x300).min(table_bytes.len());
                map_resource_samples
                    .entry(resource_type)
                    .or_insert_with(|| (*table_tag, start, table_bytes[start..end].to_vec()));
            }
            let search_end = start.saturating_add(0x400).min(table_bytes.len());
            for offset in (start..search_end).step_by(8) {
                let lookups = [offset, offset + 0x10, offset + 0x20, offset + 0x30]
                    .map(|candidate| resolve_texture_wide(&table_bytes, candidate));
                if lookups.iter().all(Option::is_some) {
                    lookup_candidates.push((
                        *table_tag,
                        resource_type,
                        start,
                        offset - start,
                        lookups.map(Option::unwrap),
                    ));
                }
            }
        }
    }

    println!("map={map_tag} package={package_name} scenario={scenario_tag}");
    println!(
        "phase_records={} phase_roots={} phase_resources={} wrappers={} intermediates={} entity_definitions={} data_tables={}",
        phase_records.len(),
        phase_roots.len(),
        entity_resources.len(),
        wrappers.len(),
        intermediate_resources.len(),
        entity_definitions.len(),
        data_tables.len(),
    );
    for (resource_type, count) in map_resource_types {
        println!("map_resource_type=0x{resource_type:08X} count={count}");
        if count <= 2
            && let Some((table, start, preview)) = map_resource_samples.get(&resource_type)
        {
            println!(
                "  sample table={table} offset=0x{start:X} hex={}",
                preview
                    .iter()
                    .map(|byte| format!("{byte:02X}"))
                    .collect::<String>()
            );
        }
    }
    for (resource_type, count) in resource_types {
        let textures = resource_textures
            .get(&resource_type)
            .map_or(0, BTreeSet::len);
        println!(
            "entity_resource_type=0x{resource_type:08X} count={count} textures={textures} sample={}",
            resource_samples[&resource_type],
        );
    }
    for (table, resource_type, start, relative, lookups) in lookup_candidates {
        println!(
            "lookup_candidate table={table} type=0x{resource_type:08X} offset=0x{start:X}+0x{relative:X} textures={lookups:?}"
        );
    }
    for (resource, resource_type, relative, lookups) in entity_lookup_candidates {
        println!(
            "entity_lookup_candidate resource={resource} type=0x{resource_type:08X} offset=0x{relative:X} textures={lookups:?}"
        );
    }
    for (resource, resource_type, texture, width, height, depth, array, mips, format) in
        interesting_textures
    {
        println!(
            "interesting_texture resource={resource} type=0x{resource_type:08X} texture={texture} shape={width}x{height}x{depth} array={array} mips={mips} format={format}"
        );
    }
    Ok(())
}

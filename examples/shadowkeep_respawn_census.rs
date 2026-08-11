//! One-shot, read-only census of authored Shadowkeep respawn components.
//!
//! Example:
//! `cargo +nightly run --example shadowkeep_respawn_census -- --packages <packages> \
//!     --output artifacts/shadowkeep-respawn-census.json`

use std::{
    collections::{BTreeMap, BTreeSet, HashSet},
    fs,
    io::Cursor,
    path::PathBuf,
};

use alkahest_data::{
    map::{ComponentData, SMapNodeTable, SRespawnPointsComponent},
    shadowkeep::{
        SShadowkeepBubbleDefinition, SShadowkeepBubbleParent, SShadowkeepEntity,
        SShadowkeepMapDataTable,
    },
};
use anyhow::{Context, Result};
use clap::Parser;
use serde::Serialize;
use tiger_parse::{PackageManagerExt, TigerReadable};
use tiger_pkg::{TagHash, package_manager};

const SCENARIO_CLASS: u32 = 0x8080_9994;
const RESPAWN_POINTS_COMPONENT: u32 = 0x8080_8CB3;
const MAP_NODE_TABLE_COMPONENT: u32 = 0x8080_92D8;

#[derive(Parser)]
struct Args {
    /// Shadowkeep package directory.
    #[arg(long)]
    packages: PathBuf,
    /// JSON evidence output.
    #[arg(long)]
    output: PathBuf,
}

#[derive(Serialize)]
struct Census {
    schema: &'static str,
    scanned_bubbles: usize,
    decoded_bubbles: usize,
    bubbles_with_respawns: usize,
    total_direct_points: usize,
    total_map_node_points: usize,
    error_count: usize,
    errors: Vec<String>,
    bubbles: Vec<BubbleRecord>,
}

#[derive(Serialize)]
struct BubbleRecord {
    bubble: String,
    scenario: Option<String>,
    direct_points: usize,
    map_node_points: usize,
    resources: Vec<ResourceRecord>,
}

#[derive(Serialize)]
struct ResourceRecord {
    source: String,
    table: String,
    entity: String,
    resource: String,
    class: String,
    definition_offset: String,
    points: usize,
}

fn referenced_tags_with_class(bytes: &[u8], reference: u32) -> HashSet<TagHash> {
    let manager = package_manager();
    bytes
        .chunks_exact(4)
        .map(|chunk| {
            TagHash(u32::from_le_bytes(
                chunk.try_into().expect("four-byte chunk"),
            ))
        })
        .filter(|tag| {
            tag.is_some()
                && manager
                    .get_entry(*tag)
                    .is_some_and(|entry| entry.reference == reference)
        })
        .collect()
}

fn scenario_tables(map: TagHash) -> Result<(Option<TagHash>, Vec<TagHash>)> {
    let manager = package_manager();
    let package_name = &manager.package_paths[&map.pkg_id()].name;
    let scenario_name = format!("{package_name}_freeroam:scenario_client");
    let Some(scenario) = manager
        .get_named_tags_by_class(SCENARIO_CLASS)
        .into_iter()
        .find_map(|(name, tag)| (name == scenario_name).then_some(tag))
    else {
        return Ok((None, Vec::new()));
    };
    let phase_records = referenced_tags_with_class(&manager.read_tag(scenario)?, 0x8080_925B);
    let mut phase_roots = HashSet::new();
    for tag in phase_records {
        phase_roots.extend(referenced_tags_with_class(
            &manager.read_tag(tag)?,
            0x8080_925E,
        ));
    }
    let mut entity_resources = HashSet::new();
    for tag in phase_roots {
        entity_resources.extend(referenced_tags_with_class(
            &manager.read_tag(tag)?,
            0x8080_9462,
        ));
    }
    let mut wrappers = HashSet::new();
    for tag in entity_resources {
        wrappers.extend(referenced_tags_with_class(
            &manager.read_tag(tag)?,
            0x8080_9468,
        ));
    }
    let mut tables = HashSet::new();
    for tag in wrappers {
        tables.extend(referenced_tags_with_class(
            &manager.read_tag(tag)?,
            0x8080_99D6,
        ));
    }
    let mut tables = tables.into_iter().collect::<Vec<_>>();
    tables.sort_unstable();
    Ok((Some(scenario), tables))
}

fn checked_offset(length: usize, offset: u64, required: usize) -> Result<usize> {
    let offset =
        usize::try_from(offset).context("respawn definition offset exceeds addressable memory")?;
    let end = offset
        .checked_add(required)
        .context("respawn definition offset overflows")?;
    anyhow::ensure!(
        end <= length,
        "respawn definition range {offset:#X}..{end:#X} exceeds tag length {length:#X}"
    );
    Ok(offset)
}

fn table_sources(map: TagHash) -> Result<(Option<TagHash>, BTreeMap<TagHash, BTreeSet<String>>)> {
    let manager = package_manager();
    let parent: SShadowkeepBubbleParent = manager.read_tag_struct(map)?;
    let definition: SShadowkeepBubbleDefinition = manager.read_tag_struct(parent.child_map)?;
    let mut tables = BTreeMap::new();
    for container in definition.map_resources {
        for table in &container.data_tables {
            tables
                .entry(*table)
                .or_insert_with(BTreeSet::new)
                .insert(format!("base:{}", container.1));
        }
    }
    let (scenario, scenario_tables) = scenario_tables(map)?;
    if let Some(scenario_tag) = scenario {
        for table in scenario_tables {
            tables
                .entry(table)
                .or_insert_with(BTreeSet::new)
                .insert(format!("scenario:{scenario_tag}"));
        }
    }
    Ok((scenario, tables))
}

fn direct_points(resource: TagHash, offset: u64) -> Result<usize> {
    let bytes = package_manager().read_tag(resource)?;
    let offset = checked_offset(bytes.len(), offset, SRespawnPointsComponent::SIZE)?;
    let mut cursor = Cursor::new(&bytes[offset..]);
    let component = SRespawnPointsComponent::read_ds(&mut cursor)?;
    Ok(component.tag.as_ref().map_or(0, |points| points.unk8.len()))
}

fn map_node_points(resource: TagHash, offset: u64) -> Result<Option<usize>> {
    let bytes = package_manager().read_tag(resource)?;
    let offset = checked_offset(
        bytes.len(),
        offset
            .checked_add(0x84)
            .context("map-node tag offset overflow")?,
        TagHash::SIZE,
    )?;
    let mut cursor = Cursor::new(&bytes[offset..]);
    let node_table_tag = TagHash::read_ds(&mut cursor)?;
    if node_table_tag.is_none() {
        return Ok(None);
    }
    let table: SMapNodeTable = package_manager().read_tag_struct(node_table_tag)?;
    Ok(Some(
        table
            .nodes
            .iter()
            .flat_map(|node| node.component_data.iter())
            .filter_map(|component| match component {
                ComponentData::SRespawnPointsComponent(component) => component.tag.as_ref(),
                _ => None,
            })
            .map(|points| points.unk8.len())
            .sum(),
    ))
}

fn record_error(census: &mut Census, error: String) {
    census.error_count += 1;
    if census.errors.len() < 32 {
        census.errors.push(error);
    }
}

fn main() -> Result<()> {
    let args = Args::parse();
    alkahest_core::initialize_package_manager(
        None,
        Some(args.packages.to_string_lossy().as_ref()),
    )?;
    let manager = package_manager();
    let bubble_tags = manager
        .get_all_by_reference(SShadowkeepBubbleParent::ID.unwrap())
        .into_iter()
        .map(|(tag, _)| tag)
        .collect::<Vec<_>>();
    drop(manager);

    let mut census = Census {
        schema: "alkahest-shadowkeep-respawn-census/v1",
        scanned_bubbles: bubble_tags.len(),
        decoded_bubbles: 0,
        bubbles_with_respawns: 0,
        total_direct_points: 0,
        total_map_node_points: 0,
        error_count: 0,
        errors: Vec::new(),
        bubbles: Vec::new(),
    };

    for bubble in bubble_tags {
        let (scenario, tables) = match table_sources(bubble) {
            Ok(value) => value,
            Err(error) => {
                record_error(&mut census, format!("{bubble}: table discovery: {error:#}"));
                continue;
            }
        };
        census.decoded_bubbles += 1;
        let mut record = BubbleRecord {
            bubble: bubble.to_string(),
            scenario: scenario.map(|tag| tag.to_string()),
            direct_points: 0,
            map_node_points: 0,
            resources: Vec::new(),
        };
        for (table_tag, sources) in tables {
            let table: SShadowkeepMapDataTable = match package_manager().read_tag_struct(table_tag)
            {
                Ok(table) => table,
                Err(error) => {
                    record_error(
                        &mut census,
                        format!("{bubble}/{table_tag}: table decode: {error:#}"),
                    );
                    continue;
                }
            };
            for entry in table.data_entries {
                if entry.entity.is_none() {
                    continue;
                }
                let entity: SShadowkeepEntity =
                    match package_manager().read_tag_struct(entry.entity) {
                        Ok(entity) => entity,
                        Err(error) => {
                            record_error(
                                &mut census,
                                format!(
                                    "{bubble}/{table_tag}/{}: entity decode: {error:#}",
                                    entry.entity
                                ),
                            );
                            continue;
                        }
                    };
                for resource_ref in entity.entity_resources {
                    let resource = &*resource_ref.resource;
                    if !resource.resource.is_valid || !resource.definition.is_valid {
                        continue;
                    }
                    let class = resource.resource.resource_type;
                    if !matches!(class, RESPAWN_POINTS_COMPONENT | MAP_NODE_TABLE_COMPONENT) {
                        continue;
                    }
                    let resource_tag = resource_ref.resource.taghash();
                    let points = match class {
                        RESPAWN_POINTS_COMPONENT => {
                            direct_points(resource_tag, resource.definition.offset).map(Some)
                        }
                        MAP_NODE_TABLE_COMPONENT => {
                            map_node_points(resource_tag, resource.definition.offset)
                        }
                        _ => unreachable!(),
                    };
                    let points = match points {
                        Ok(Some(points)) => points,
                        Ok(None) => continue,
                        Err(error) => {
                            record_error(
                                &mut census,
                                format!(
                                    "{bubble}/{table_tag}/{resource_tag}: respawn decode: \
                                     {error:#}"
                                ),
                            );
                            continue;
                        }
                    };
                    let source = sources.iter().cloned().collect::<Vec<_>>().join(",");
                    record.resources.push(ResourceRecord {
                        source,
                        table: table_tag.to_string(),
                        entity: entry.entity.to_string(),
                        resource: resource_tag.to_string(),
                        class: format!("{class:08X}"),
                        definition_offset: format!("0x{:X}", resource.definition.offset),
                        points,
                    });
                    if class == RESPAWN_POINTS_COMPONENT {
                        record.direct_points += points;
                    } else {
                        record.map_node_points += points;
                    }
                }
            }
        }
        if record.direct_points + record.map_node_points != 0 {
            census.bubbles_with_respawns += 1;
            census.total_direct_points += record.direct_points;
            census.total_map_node_points += record.map_node_points;
            census.bubbles.push(record);
        }
    }
    if let Some(parent) = args.output.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&args.output, serde_json::to_vec_pretty(&census)?)
        .with_context(|| format!("writing {}", args.output.display()))?;
    Ok(())
}

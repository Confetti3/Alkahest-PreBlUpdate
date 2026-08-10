//! Shadowkeep map-chain decoding and scene normalization.
//!
//! The reader intentionally treats each table entry as an isolation boundary:
//! corrupt or unrecognised data is recorded in the report and does not prevent
//! the rest of a bubble from becoming viewable.

use std::{
    collections::{BTreeMap, HashSet},
    io::{Cursor, Seek, SeekFrom},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

use alkahest_core::ConVars;
use alkahest_data::{
    shadowkeep::{
        SShadowkeepBubbleDefinition, SShadowkeepBubbleParent, SShadowkeepCubemapPlacement,
        SShadowkeepEntity, SShadowkeepLightCollection, SShadowkeepMapDataTable,
        SShadowkeepOcclusionBounds, SShadowkeepRigidModelComponent, SShadowkeepShadowingLight,
        SShadowkeepSkyObjectCollection, SShadowkeepStaticPlacement, SShadowkeepTerrainPlacement,
        SShadowkeepTextureHeader,
    },
    tfx::{
        RenderStage, TfxFeatureRenderer, atmosphere::SShadowkeepAtmospherePlacement,
        common::AxisAlignedBBox, features::dynamic::RenderStageSubscription,
    },
};
use anyhow::Context;
use glam::{Mat4, Vec3, Vec4Swizzles};
use parking_lot::Mutex;
use tiger_parse::{PackageManagerExt, TigerReadable};
use tiger_pkg::{TagHash, package_manager};

use crate::world::{
    render_objects::{DynamicRenderObject, StaticRenderObject},
    transform::Transform,
};
use alkahest_render::{
    Renderer,
    asset::texture::Texture,
    feature::{
        cubemap::CubemapRenderer, light::LightRenderer, rigid_model::DynamicModel,
        static_geometry::StaticInstancesRenderer, terrain_patches::TerrainPatchesRenderer,
    },
    object::RenderObject,
    renderer::submit::atmosphere::AtmosphereData,
};

const STATIC_PLACEMENT: u32 = 0x8080_71B3;
const TERRAIN_PLACEMENT: u32 = 0x8080_714B;
const LIGHT_COLLECTION: u32 = 0x8080_6F5A;
const SHADOWING_LIGHT: u32 = 0x8080_7133;
const CUBEMAP_VOLUME: u32 = 0x8080_6B7F;
const ATMOSPHERE_PLACEMENT: u32 = 0x8080_7086;
const SKY_OBJECT_PLACEMENT: u32 = 0x8080_6F91;
const SKY_OBJECT_COLLECTION: u32 = 0x8080_6F95;
const RIGID_MODEL_COMPONENT: u32 = 0x8080_72B8;
const SHADOWKEEP_LOOKUP_TABLE_BYTES: usize = 64 * 64 * 4;
const ENTITY_RESOURCE_EXAMPLE_LIMIT: usize = 5;

fn bounded_offset(table_len: usize, offset: u64, required: usize) -> anyhow::Result<usize> {
    let offset = usize::try_from(offset).context("resource offset exceeds addressable memory")?;
    let end = offset
        .checked_add(required)
        .context("resource offset overflows while checking table bounds")?;
    anyhow::ensure!(
        end <= table_len,
        "resource range {offset:#X}..{end:#X} exceeds table length {table_len:#X}"
    );
    Ok(offset)
}
fn referenced_tags_with_class(bytes: &[u8], reference: u32) -> HashSet<TagHash> {
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

fn shadowkeep_scenario_tables(map: TagHash) -> anyhow::Result<(Option<TagHash>, Vec<TagHash>)> {
    let manager = package_manager();
    let package_name = &manager.package_paths[&map.pkg_id()].name;
    let scenario_name = format!("{package_name}_freeroam:scenario_client");
    let Some(scenario) = manager
        .get_named_tags_by_class(0x8080_9994)
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

/// Thread-safe map-load counters, updated by both decoding and asset setup.
#[derive(Clone, Default)]
pub struct MapLoadProgress {
    total_tables: Arc<AtomicUsize>,
    tables_seen: Arc<AtomicUsize>,
    entries_seen: Arc<AtomicUsize>,
    visual_resources_loaded: Arc<AtomicUsize>,
    gpu_assets_requested: Arc<AtomicUsize>,
    diagnostics: Arc<Mutex<Vec<String>>>,
    cancelled: Arc<AtomicBool>,
}

impl MapLoadProgress {
    pub fn snapshot(&self) -> MapLoadProgressSnapshot {
        MapLoadProgressSnapshot {
            total_tables: self.total_tables.load(Ordering::Relaxed),
            tables_seen: self.tables_seen.load(Ordering::Relaxed),
            entries_seen: self.entries_seen.load(Ordering::Relaxed),
            visual_resources_loaded: self.visual_resources_loaded.load(Ordering::Relaxed),
            gpu_assets_requested: self.gpu_assets_requested.load(Ordering::Relaxed),
            diagnostics: self.diagnostics.lock().len(),
            cancelled: self.cancelled.load(Ordering::Relaxed),
        }
    }

    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }

    fn diagnostic(&self, message: String) {
        self.diagnostics.lock().push(message);
    }
}

#[derive(Debug, Clone, Default)]
pub struct MapLoadProgressSnapshot {
    pub total_tables: usize,
    pub tables_seen: usize,
    pub entries_seen: usize,
    pub visual_resources_loaded: usize,
    pub gpu_assets_requested: usize,
    pub diagnostics: usize,
    pub cancelled: bool,
}

#[derive(Debug, Clone)]
pub struct MapLoadDiagnostic {
    pub table: TagHash,
    pub entry_offset: u64,
    pub resource_class: u32,
    pub error: String,
}

#[derive(Debug, Clone)]
pub struct EntityResourceExample {
    pub table: TagHash,
    pub entity: TagHash,
    pub resource: TagHash,
    pub definition_offset: u64,
    pub translation: Vec3,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShadowkeepTableOrigin {
    BaseBubble,
    FreeroamScenario,
}

impl ShadowkeepTableOrigin {
    fn label(self) -> &'static str {
        match self {
            Self::BaseBubble => "base bubble",
            Self::FreeroamScenario => "freeroam scenario",
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct SkyObjectPlacementCandidate {
    collection: TagHash,
    table: TagHash,
    entry_offset: u64,
    origin: ShadowkeepTableOrigin,
}

#[derive(Debug, Clone, Default)]
pub struct MapLoadReport {
    pub map: TagHash,
    pub containers: usize,
    pub scenario: Option<TagHash>,
    pub activity_tables: usize,
    pub tables: usize,
    pub table_origins: BTreeMap<TagHash, ShadowkeepTableOrigin>,
    pub entries: usize,
    pub static_placements: usize,
    pub terrain_placements: usize,
    pub entity_entries: usize,
    pub entity_read_failures: usize,
    pub entries_without_table_resource: usize,
    pub rigid_entities: usize,
    pub light_collections: usize,
    pub light_render_objects: usize,
    pub shadowing_lights: usize,
    pub cubemap_volumes: usize,
    pub atmosphere_placements: usize,
    pub sky_object_placements: usize,
    pub sky_object_collections: usize,
    pub sky_object_records: usize,
    pub sky_object_render_objects: usize,
    pub sky_object_skipped_kind_5: usize,
    pub sky_object_collection_tags: Vec<TagHash>,
    pub sky_object_model_tags: Vec<TagHash>,
    pub deferred_sky_object_collections: Vec<TagHash>,
    pub sky_object_stage_subscriptions: Vec<(TagHash, u32)>,
    pub static_render_objects: usize,
    pub terrain_render_objects: usize,
    pub rigid_render_objects: usize,
    pub cubemap_render_objects: usize,
    pub deduplicated_resources: usize,
    pub deferred_resources: usize,
    pub skipped_resources: usize,
    pub entity_resource_class_counts: BTreeMap<u32, usize>,
    pub valid_entity_resource_definitions: BTreeMap<u32, usize>,
    pub invalid_entity_resource_definitions: BTreeMap<u32, usize>,
    pub loaded_entity_resource_classes: BTreeMap<u32, usize>,
    pub deferred_entity_resource_classes: BTreeMap<u32, usize>,
    pub failed_entity_resource_classes: BTreeMap<u32, usize>,
    pub entity_resource_examples: BTreeMap<u32, Vec<EntityResourceExample>>,
    pub resource_class_counts: BTreeMap<u32, usize>,
    pub deferred_resource_classes: BTreeMap<u32, usize>,
    pub world_bounds: Option<AxisAlignedBBox>,
    /// Bounds from table placement transforms, retained separately from the
    /// reconstructed renderable bounds for a stable initial map frame.
    pub placement_bounds: Option<AxisAlignedBBox>,
    /// Bounds from placements that admitted at least one entity visual.
    pub entity_placement_bounds: Option<AxisAlignedBBox>,
    pub spawn_points: Vec<Vec3>,
    pub diagnostics: Vec<MapLoadDiagnostic>,
    pub cancelled: bool,
    pub elapsed: Duration,
}

impl MapLoadReport {
    pub fn is_degraded(&self) -> bool {
        self.deferred_resources != 0 || !self.diagnostics.is_empty()
    }

    fn defer_entity_resource(&mut self, class: u32, example: EntityResourceExample) {
        self.deferred_resources += 1;
        *self
            .deferred_entity_resource_classes
            .entry(class)
            .or_default() += 1;
        let examples = self.entity_resource_examples.entry(class).or_default();
        if examples.len() < ENTITY_RESOURCE_EXAMPLE_LIMIT {
            examples.push(example);
        }
    }

    fn entity_resource_accounting_is_complete(&self) -> bool {
        self.entity_resource_class_counts
            .iter()
            .all(|(class, total)| {
                let loaded = self
                    .loaded_entity_resource_classes
                    .get(class)
                    .copied()
                    .unwrap_or_default();
                let deferred = self
                    .deferred_entity_resource_classes
                    .get(class)
                    .copied()
                    .unwrap_or_default();
                let failed = self
                    .failed_entity_resource_classes
                    .get(class)
                    .copied()
                    .unwrap_or_default();
                *total == loaded + deferred + failed
            })
    }

    fn diagnostic(&mut self, progress: &MapLoadProgress, diagnostic: MapLoadDiagnostic) {
        progress.diagnostic(format!(
            "table {} offset {:#X} class {:08X}: {}",
            diagnostic.table, diagnostic.entry_offset, diagnostic.resource_class, diagnostic.error
        ));
        self.diagnostics.push(diagnostic);
    }
}

fn load_shadowkeep_rigid_entity(
    renderer: &Arc<Renderer>,
    progress: &MapLoadProgress,
    world: &mut hecs::World,
    transform: Transform,
    resource_tag: TagHash,
    definition_offset: u64,
) -> anyhow::Result<()> {
    let bytes = package_manager().read_tag(resource_tag)?;
    let mut cursor = Cursor::new(bytes);
    cursor.seek(SeekFrom::Start(definition_offset))?;
    let component = SShadowkeepRigidModelComponent::read_ds(&mut cursor)?;
    let model = DynamicModel::load_shadowkeep(
        component.model,
        component.material_variants,
        component.techniques,
    )?;
    progress
        .gpu_assets_requested
        .fetch_add(1, Ordering::Relaxed);
    world.spawn((
        transform,
        DynamicRenderObject::new(
            renderer.add_object(RenderObject::new(TfxFeatureRenderer::DynamicObjects, model)),
        ),
    ));
    progress
        .visual_resources_loaded
        .fetch_add(1, Ordering::Relaxed);
    Ok(())
}

/// Decode a Shadowkeep bubble into the modern world. Static geometry is
/// immediately normalized into render objects; unimplemented visual families
/// remain explicit report diagnostics so they cannot silently disappear.
pub fn load_shadowkeep_map_into_world(
    tag: TagHash,
    renderer: &Arc<Renderer>,
    progress: &MapLoadProgress,
) -> anyhow::Result<(hecs::World, MapLoadReport)> {
    let started = Instant::now();
    let parent: SShadowkeepBubbleParent = package_manager()
        .read_tag_struct(tag)
        .context("Failed to read Shadowkeep bubble parent")?;
    let definition: SShadowkeepBubbleDefinition = package_manager()
        .read_tag_struct(parent.child_map)
        .context("Failed to read Shadowkeep bubble definition")?;
    let mut report = MapLoadReport {
        map: tag,
        ..Default::default()
    };
    report.containers = definition.map_resources.len();
    let mut table_records = definition
        .map_resources
        .iter()
        .flat_map(|container| {
            container
                .data_tables
                .iter()
                .copied()
                .map(|table| (table, ShadowkeepTableOrigin::BaseBubble))
        })
        .collect::<Vec<_>>();
    match shadowkeep_scenario_tables(tag) {
        Ok((scenario, tables)) => {
            report.scenario = scenario;
            report.activity_tables = tables.len();
            table_records.extend(
                tables
                    .into_iter()
                    .map(|table| (table, ShadowkeepTableOrigin::FreeroamScenario)),
            );
        }
        Err(error) => report.diagnostic(
            progress,
            MapLoadDiagnostic {
                table: tag,
                entry_offset: 0,
                resource_class: 0x8080_9994,
                error: format!("could not decode freeroam scenario layers: {error:#}"),
            },
        ),
    }
    let mut unique_tables = HashSet::new();
    table_records.retain(|(table, _)| unique_tables.insert(*table));
    report.table_origins.extend(table_records.iter().copied());
    progress
        .total_tables
        .store(table_records.len(), Ordering::Relaxed);
    let mut world = hecs::World::new();
    let mut bound_points = Vec::new();
    let mut entity_bound_points = Vec::new();
    let mut loaded_static_collections = HashSet::new();
    let mut loaded_terrain_resources = HashSet::new();
    let mut visual_bounds: Option<AxisAlignedBBox> = None;
    let mut sky_object_candidates = Vec::new();

    for (table_hash, table_origin) in table_records {
        if progress.is_cancelled() {
            report.cancelled = true;
            break;
        }
        let table: SShadowkeepMapDataTable = match package_manager().read_tag_struct(table_hash) {
            Ok(table) => table,
            Err(error) => {
                report.skipped_resources += 1;
                report.diagnostic(
                    progress,
                    MapLoadDiagnostic {
                        table: table_hash,
                        entry_offset: 0,
                        resource_class: 0,
                        error: format!("could not decode map data table: {error:#}"),
                    },
                );
                continue;
            }
        };
        let table_bytes = match package_manager().read_tag(table_hash) {
            Ok(bytes) => bytes,
            Err(error) => {
                report.skipped_resources += table.data_entries.len();
                report.diagnostic(
                    progress,
                    MapLoadDiagnostic {
                        table: table_hash,
                        entry_offset: 0,
                        resource_class: 0,
                        error: format!("could not read map data table bytes: {error:#}"),
                    },
                );
                continue;
            }
        };
        report.tables += 1;
        progress.tables_seen.fetch_add(1, Ordering::Relaxed);
        for entry in &table.data_entries {
            if progress.is_cancelled() {
                report.cancelled = true;
                break;
            }
            report.entries += 1;
            progress.entries_seen.fetch_add(1, Ordering::Relaxed);
            let transform = Transform::new(
                entry.translation.xyz(),
                entry.rotation,
                Vec3::splat(entry.translation.w),
            );
            let mut loaded_entity_visual = false;
            // Entity payloads are independent of the optional table-local resource pointer.
            // Process them first so entity-only entries are not discarded by the guard below.
            if !entry.entity.is_none() {
                report.entity_entries += 1;
                match package_manager().read_tag_struct::<SShadowkeepEntity>(entry.entity) {
                    Ok(entity) => {
                        for resource_ref in &entity.entity_resources {
                            if progress.is_cancelled() {
                                report.cancelled = true;
                                break;
                            }

                            let resource_tag = resource_ref.resource.taghash();
                            let resource = &*resource_ref.resource;
                            let class = resource.resource.resource_type;
                            *report
                                .entity_resource_class_counts
                                .entry(class)
                                .or_default() += 1;
                            if resource.definition.is_valid {
                                *report
                                    .valid_entity_resource_definitions
                                    .entry(class)
                                    .or_default() += 1;
                            } else {
                                *report
                                    .invalid_entity_resource_definitions
                                    .entry(class)
                                    .or_default() += 1;
                            }
                            let example = EntityResourceExample {
                                table: table_hash,
                                entity: entry.entity,
                                resource: resource_tag,
                                definition_offset: resource.definition.offset,
                                translation: entry.translation.xyz(),
                            };

                            if !resource.resource.is_valid || !resource.definition.is_valid {
                                report.defer_entity_resource(class, example);
                                continue;
                            }

                            match class {
                                RIGID_MODEL_COMPONENT => {
                                    report.rigid_entities += 1;
                                    if let Err(error) = load_shadowkeep_rigid_entity(
                                        renderer,
                                        progress,
                                        &mut world,
                                        transform,
                                        resource_tag,
                                        resource.definition.offset,
                                    ) {
                                        report.skipped_resources += 1;
                                        *report
                                            .failed_entity_resource_classes
                                            .entry(class)
                                            .or_default() += 1;
                                        report.diagnostic(
                                            progress,
                                            MapLoadDiagnostic {
                                                table: table_hash,
                                                entry_offset: resource.definition.offset,
                                                resource_class: class,
                                                error: format!(
                                                    "entity {} resource {}: rigid model: {error:#}",
                                                    entry.entity, resource_tag,
                                                ),
                                            },
                                        );
                                    } else {
                                        report.rigid_render_objects += 1;
                                        *report
                                            .loaded_entity_resource_classes
                                            .entry(class)
                                            .or_default() += 1;
                                        loaded_entity_visual = true;
                                    }
                                }
                                other => report.defer_entity_resource(other, example),
                            }
                        }
                    }
                    Err(error) => {
                        report.entity_read_failures += 1;
                        report.skipped_resources += 1;
                        report.diagnostic(
                            progress,
                            MapLoadDiagnostic {
                                table: table_hash,
                                entry_offset: 0,
                                resource_class: package_manager()
                                    .get_entry(entry.entity)
                                    .map_or(0, |package_entry| package_entry.reference),
                                error: format!(
                                    "entity {} could not be decoded: {error:#}",
                                    entry.entity,
                                ),
                            },
                        );
                    }
                }
            }
            if loaded_entity_visual {
                entity_bound_points.push(entry.translation.xyz());
            }
            if !entry.data_resource.is_valid {
                report.entries_without_table_resource += 1;
                continue;
            }
            if entry.data_resource.resource_type != SKY_OBJECT_PLACEMENT {
                bound_points.push(entry.translation.xyz());
            }
            *report
                .resource_class_counts
                .entry(entry.data_resource.resource_type)
                .or_default() += 1;

            match entry.data_resource.resource_type {
                SKY_OBJECT_PLACEMENT => {
                    report.sky_object_placements += 1;
                    let result = (|| -> anyhow::Result<()> {
                        bounded_offset(table_bytes.len(), entry.data_resource.offset, 0x14)?;
                        let mut cursor = Cursor::new(&table_bytes[..]);
                        cursor.seek(SeekFrom::Start(entry.data_resource.offset + 0x10))?;
                        let collection_tag = TagHash::read_ds(&mut cursor)?;
                        anyhow::ensure!(
                            collection_tag.is_some(),
                            "sky-object collection tag is absent"
                        );
                        let package_entry = package_manager()
                            .get_entry(collection_tag)
                            .context("sky-object collection package entry is missing")?;
                        anyhow::ensure!(
                            package_entry.reference == SKY_OBJECT_COLLECTION,
                            "sky-object placement references class 0x{:08X}, expected 0x{:08X}",
                            package_entry.reference,
                            SKY_OBJECT_COLLECTION,
                        );
                        sky_object_candidates.push(SkyObjectPlacementCandidate {
                            collection: collection_tag,
                            table: table_hash,
                            entry_offset: entry.data_resource.offset,
                            origin: table_origin,
                        });
                        tracing::info!(
                            table = %table_hash,
                            origin = table_origin.label(),
                            collection = %collection_tag,
                            "discovered Shadowkeep sky-object collection"
                        );
                        Ok(())
                    })();
                    if let Err(error) = result {
                        report.skipped_resources += 1;
                        report.diagnostic(
                            progress,
                            MapLoadDiagnostic {
                                table: table_hash,
                                entry_offset: entry.data_resource.offset,
                                resource_class: SKY_OBJECT_PLACEMENT,
                                error: format!("sky-object placement: {error:#}"),
                            },
                        );
                    }
                }
                ATMOSPHERE_PLACEMENT => {
                    report.atmosphere_placements += 1;
                    if world.query::<&AtmosphereData>().iter().next().is_some() {
                        report.deduplicated_resources += 1;
                        continue;
                    }
                    let result = (|| -> anyhow::Result<()> {
                        bounded_offset(table_bytes.len(), entry.data_resource.offset, 0xF8)?;
                        let mut cursor = Cursor::new(&table_bytes[..]);
                        cursor.seek(SeekFrom::Start(entry.data_resource.offset))?;
                        let placement = SShadowkeepAtmospherePlacement::read_ds(&mut cursor)?;
                        for (name, tag) in [
                            ("lookup_volume_0", placement.lookup_volume_0),
                            ("lookup_volume_1", placement.lookup_volume_1),
                            ("lookup_vertical", placement.lookup_vertical),
                            ("lookup_table", placement.lookup_table),
                        ] {
                            anyhow::ensure!(tag.is_some(), "{name} tag is absent");
                            anyhow::ensure!(
                                package_manager().get_entry(tag).is_some(),
                                "{name} tag {tag} is not present in the package set"
                            );
                        }

                        let read_lookup_header = |tag| {
                            package_manager()
                                .read_tag_struct::<SShadowkeepTextureHeader>(tag)
                                .with_context(|| {
                                    format!("failed to read atmosphere texture header {tag}")
                                })
                        };
                        let lookup_volume_0 = read_lookup_header(placement.lookup_volume_0)?;
                        let lookup_volume_1 = read_lookup_header(placement.lookup_volume_1)?;
                        let lookup_vertical = read_lookup_header(placement.lookup_vertical)?;
                        let lookup_table_entry = package_manager()
                            .get_entry(placement.lookup_table)
                            .context("lookup table package entry disappeared")?;
                        let lookup_table_bytes =
                            package_manager().read_tag(placement.lookup_table)?;
                        anyhow::ensure!(
                            lookup_table_bytes.len() == SHADOWKEEP_LOOKUP_TABLE_BYTES,
                            "lookup table {} has {} bytes; expected {}",
                            placement.lookup_table,
                            lookup_table_bytes.len(),
                            SHADOWKEEP_LOOKUP_TABLE_BYTES,
                        );
                        // This raw table has no texture header. Its companion
                        // vertical lookup is authored as sRGB, and frozen A/B
                        // captures reject the large color shift from linear
                        // UNORM sampling.
                        let lookup_table = Texture::load_2d_raw(
                            &renderer.gpu.device,
                            64,
                            64,
                            &lookup_table_bytes,
                            d3d11::dxgi::Format::R8g8b8a8UnormSrgb,
                            Some("Shadowkeep atmosphere lookup table"),
                            false,
                        )?;
                        let lookup_table_min =
                            lookup_table_bytes.iter().copied().min().unwrap_or(0);
                        let lookup_table_max =
                            lookup_table_bytes.iter().copied().max().unwrap_or(0);
                        let lookup_table_mean = lookup_table_bytes
                            .iter()
                            .map(|&value| f64::from(value))
                            .sum::<f64>()
                            / lookup_table_bytes.len() as f64;
                        tracing::info!(
                            table = %table_hash,
                            entry_offset = entry.data_resource.offset,
                            lookup_table = %placement.lookup_table,
                            lookup_table_class = format_args!("0x{:08X}", lookup_table_entry.reference),
                            lookup_table_format = "R8G8B8A8_UNORM_SRGB",
                            lookup_table_min,
                            lookup_table_max,
                            lookup_table_mean,
                            lookup_volume_0 = ?(
                                lookup_volume_0.width,
                                lookup_volume_0.height,
                                lookup_volume_0.depth,
                                lookup_volume_0.format,
                            ),
                            lookup_volume_1 = ?(
                                lookup_volume_1.width,
                                lookup_volume_1.height,
                                lookup_volume_1.depth,
                                lookup_volume_1.format,
                            ),
                            lookup_vertical = ?(
                                lookup_vertical.width,
                                lookup_vertical.height,
                                lookup_vertical.depth,
                                lookup_vertical.format,
                            ),
                            lookup_parameters = ?placement.lookup_parameters,
                            "loaded authored Shadowkeep atmosphere inputs"
                        );
                        let atmosphere = AtmosphereData {
                            shadowkeep_lookup_volume_0: renderer
                                .asset_manager
                                .load(placement.lookup_volume_0),
                            shadowkeep_lookup_volume_1: renderer
                                .asset_manager
                                .load(placement.lookup_volume_1),
                            shadowkeep_lookup_vertical: renderer
                                .asset_manager
                                .load(placement.lookup_vertical),
                            shadowkeep_lookup_table: Some(lookup_table),
                            shadowkeep_lookup_parameters: placement.lookup_parameters,
                            ..Default::default()
                        };
                        world.spawn((atmosphere,));
                        progress
                            .gpu_assets_requested
                            .fetch_add(4, Ordering::Relaxed);
                        progress
                            .visual_resources_loaded
                            .fetch_add(1, Ordering::Relaxed);
                        Ok(())
                    })();
                    if let Err(error) = result {
                        report.skipped_resources += 1;
                        report.diagnostic(
                            progress,
                            MapLoadDiagnostic {
                                table: table_hash,
                                entry_offset: entry.data_resource.offset,
                                resource_class: ATMOSPHERE_PLACEMENT,
                                error: format!("atmosphere placement: {error:#}"),
                            },
                        );
                    }
                }
                CUBEMAP_VOLUME => {
                    report.cubemap_volumes += 1;
                    let result = (|| -> anyhow::Result<()> {
                        bounded_offset(table_bytes.len(), entry.data_resource.offset, 0x1A4)?;
                        let mut cursor = Cursor::new(&table_bytes[..]);
                        cursor.seek(SeekFrom::Start(entry.data_resource.offset))?;
                        let placement = SShadowkeepCubemapPlacement::read_ds(&mut cursor)?;
                        let component = placement.normalized();
                        let cubemap = CubemapRenderer::load(&renderer.gpu, &component)?;
                        progress
                            .gpu_assets_requested
                            .fetch_add(1, Ordering::Relaxed);
                        let (volume_scale, volume_rotation, volume_translation) =
                            component.unkb0.to_scale_rotation_translation();
                        world.spawn((
                            Transform::new(volume_translation, volume_rotation, volume_scale),
                            DynamicRenderObject::new(renderer.add_object(RenderObject::new(
                                TfxFeatureRenderer::Cubemaps,
                                Box::new(cubemap),
                            ))),
                        ));
                        progress
                            .visual_resources_loaded
                            .fetch_add(1, Ordering::Relaxed);
                        report.cubemap_render_objects += 1;
                        Ok(())
                    })();
                    if let Err(error) = result {
                        report.skipped_resources += 1;
                        report.diagnostic(
                            progress,
                            MapLoadDiagnostic {
                                table: table_hash,
                                entry_offset: entry.data_resource.offset,
                                resource_class: CUBEMAP_VOLUME,
                                error: format!("cubemap volume: {error:#}"),
                            },
                        );
                    }
                }
                LIGHT_COLLECTION => {
                    report.light_collections += 1;
                    let result = (|| -> anyhow::Result<()> {
                        bounded_offset(table_bytes.len(), entry.data_resource.offset, 20)?;
                        let mut cursor = Cursor::new(&table_bytes[..]);
                        cursor.seek(SeekFrom::Start(entry.data_resource.offset + 16))?;
                        let light_tag = TagHash::read_ds(&mut cursor)?;
                        anyhow::ensure!(light_tag.is_some(), "light collection tag is absent");
                        tracing::warn!(table = %table_hash, tag = %light_tag, "reading Shadowkeep light collection");
                        let collection: SShadowkeepLightCollection =
                            package_manager().read_tag_struct(light_tag)?;
                        tracing::warn!(
                            table = %table_hash,
                            tag = %light_tag,
                            lights = collection.lights.len(),
                            transforms = collection.transforms.len(),
                            "decoded Shadowkeep light collection"
                        );
                        let bounds = package_manager()
                            .read_tag_struct::<SShadowkeepOcclusionBounds>(
                                collection.occlusion_bounds.taghash(),
                            )
                            .ok();
                        for (index, light) in collection.lights.iter().enumerate() {
                            if progress.is_cancelled() {
                                report.cancelled = true;
                                break;
                            }
                            let transform = collection.transforms.get(index).copied().unwrap_or(
                                alkahest_data::shadowkeep::SShadowkeepRotationTranslation {
                                    rotation: glam::Quat::IDENTITY,
                                    translation: glam::Vec4::ZERO,
                                },
                            );
                            let _bounds = bounds
                                .as_ref()
                                .and_then(|value| value.bounds.get(index))
                                .map(|value| value.bb);
                            tracing::debug!(
                                table = %table_hash,
                                light = index,
                                technique = %light.technique_shading,
                                volumetrics = %light.unk84,
                                "decoded Shadowkeep light"
                            );
                            let renderer_object = LightRenderer::new_shadowkeep(
                                renderer,
                                light.technique_shading,
                                light.unk84,
                                light.light_to_world,
                                // The preserved occlusion boxes are in
                                // collection-local space.  Applying them
                                // before `prepare` would cull every light
                                // against the world camera; LightRenderer
                                // derives the world-space volume on its
                                // first prepare instead.
                                None,
                            )?;
                            world.spawn((
                                Transform::new(
                                    transform.translation.xyz(),
                                    transform.rotation,
                                    Vec3::ONE,
                                ),
                                DynamicRenderObject::new(renderer.add_object(RenderObject::new(
                                    TfxFeatureRenderer::DeferredLights,
                                    renderer_object,
                                ))),
                            ));
                            report.light_render_objects += 1;
                        }
                        Ok(())
                    })();
                    if let Err(error) = result {
                        tracing::warn!(table = %table_hash, class = %format_args!("{LIGHT_COLLECTION:08X}"), error = %format_args!("{error:#}"), "Shadowkeep light collection skipped");
                        report.skipped_resources += 1;
                        report.diagnostic(
                            progress,
                            MapLoadDiagnostic {
                                table: table_hash,
                                entry_offset: entry.data_resource.offset,
                                resource_class: LIGHT_COLLECTION,
                                error: format!("light collection: {error:#}"),
                            },
                        );
                    }
                }
                SHADOWING_LIGHT => {
                    report.shadowing_lights += 1;
                    let result = (|| -> anyhow::Result<()> {
                        bounded_offset(table_bytes.len(), entry.data_resource.offset, 20)?;
                        let mut cursor = Cursor::new(&table_bytes[..]);
                        cursor.seek(SeekFrom::Start(entry.data_resource.offset + 16))?;
                        let light_tag = TagHash::read_ds(&mut cursor)?;
                        anyhow::ensure!(light_tag.is_some(), "shadowing light tag is absent");
                        let light: SShadowkeepShadowingLight =
                            package_manager().read_tag_struct(light_tag)?;
                        let renderer_object = LightRenderer::new_shadowkeep_shadowing(
                            renderer,
                            light.technique_shading,
                            light.technique_shading_shadowing,
                            light.technique_volumetrics,
                            light.technique_volumetrics_shadowing,
                            light.light_to_world,
                            glam::Mat4::IDENTITY,
                        )?;
                        world.spawn((
                            Transform::new(
                                entry.translation.xyz(),
                                entry.rotation,
                                Vec3::splat(entry.translation.w),
                            ),
                            DynamicRenderObject::new(renderer.add_object(RenderObject::new(
                                TfxFeatureRenderer::DeferredLights,
                                renderer_object,
                            ))),
                        ));
                        Ok(())
                    })();
                    if let Err(error) = result {
                        tracing::warn!(table = %table_hash, class = %format_args!("{SHADOWING_LIGHT:08X}"), error = %format_args!("{error:#}"), "Shadowkeep shadowing light skipped");
                        report.skipped_resources += 1;
                        report.diagnostic(
                            progress,
                            MapLoadDiagnostic {
                                table: table_hash,
                                entry_offset: entry.data_resource.offset,
                                resource_class: SHADOWING_LIGHT,
                                error: format!("shadowing light: {error:#}"),
                            },
                        );
                    }
                }
                STATIC_PLACEMENT => {
                    report.static_placements += 1;
                    let result = (|| -> anyhow::Result<()> {
                        bounded_offset(table_bytes.len(), entry.data_resource.offset, 20)?;
                        let mut cursor = Cursor::new(&table_bytes[..]);
                        cursor.seek(SeekFrom::Start(entry.data_resource.offset + 16))?;
                        let header_tag = TagHash::read_ds(&mut cursor)?;
                        let placement: SShadowkeepStaticPlacement =
                            package_manager().read_tag_struct(header_tag)?;
                        let instances_hash = placement.instances.taghash();
                        if !loaded_static_collections.insert(instances_hash) {
                            report.deduplicated_resources += 1;
                            return Ok(());
                        }
                        let feature = StaticInstancesRenderer::load_shadowkeep(
                            &renderer.gpu,
                            instances_hash,
                        )?;
                        let feature_bounds = feature.bounds();
                        if feature_bounds.is_valid() {
                            visual_bounds = Some(match visual_bounds {
                                Some(bounds) => bounds.union(&feature_bounds),
                                None => feature_bounds,
                            });
                        }
                        progress
                            .gpu_assets_requested
                            .fetch_add(1, Ordering::Relaxed);
                        world.spawn((StaticRenderObject::new(renderer.add_object(
                            RenderObject::new(
                                TfxFeatureRenderer::ChunkedInstanceObjects,
                                Box::new(feature),
                            ),
                        )),));
                        progress
                            .visual_resources_loaded
                            .fetch_add(1, Ordering::Relaxed);
                        report.static_render_objects += 1;
                        Ok(())
                    })();
                    if let Err(error) = result {
                        report.skipped_resources += 1;
                        report.diagnostic(
                            progress,
                            MapLoadDiagnostic {
                                table: table_hash,
                                entry_offset: entry.data_resource.offset,
                                resource_class: STATIC_PLACEMENT,
                                error: format!("static placement: {error:#}"),
                            },
                        );
                    }
                }
                TERRAIN_PLACEMENT => {
                    report.terrain_placements += 1;
                    let result = (|| -> anyhow::Result<()> {
                        bounded_offset(table_bytes.len(), entry.data_resource.offset, 0x20)?;
                        let mut cursor = Cursor::new(&table_bytes[..]);
                        cursor.seek(SeekFrom::Start(entry.data_resource.offset))?;
                        let placement = SShadowkeepTerrainPlacement::read_ds(&mut cursor)?;
                        if !loaded_terrain_resources.insert(placement.terrain) {
                            report.deduplicated_resources += 1;
                            return Ok(());
                        }
                        let terrain = TerrainPatchesRenderer::load_shadowkeep(
                            &renderer.gpu,
                            placement.terrain,
                            placement.identifier as u64,
                        )?;
                        progress
                            .gpu_assets_requested
                            .fetch_add(1, Ordering::Relaxed);
                        world.spawn((StaticRenderObject::new(renderer.add_object(
                            RenderObject::new(TfxFeatureRenderer::TerrainPatch, terrain),
                        )),));
                        progress
                            .visual_resources_loaded
                            .fetch_add(1, Ordering::Relaxed);
                        report.terrain_render_objects += 1;
                        Ok(())
                    })();
                    if let Err(error) = result {
                        report.skipped_resources += 1;
                        report.diagnostic(
                            progress,
                            MapLoadDiagnostic {
                                table: table_hash,
                                entry_offset: entry.data_resource.offset,
                                resource_class: TERRAIN_PLACEMENT,
                                error: format!("terrain placement: {error:#}"),
                            },
                        );
                    }
                }
                resource_class => {
                    report.deferred_resources += 1;
                    *report
                        .deferred_resource_classes
                        .entry(resource_class)
                        .or_default() += 1;
                }
            }
        }
    }
    let mut candidates_by_collection = BTreeMap::new();
    for candidate in sky_object_candidates {
        let occupied = candidates_by_collection.get(&candidate.collection).copied();
        if occupied.is_some() {
            report.deduplicated_resources += 1;
        }
        if occupied.is_none_or(|existing: SkyObjectPlacementCandidate| {
            candidate.origin == ShadowkeepTableOrigin::BaseBubble
                && existing.origin == ShadowkeepTableOrigin::FreeroamScenario
        }) {
            candidates_by_collection.insert(candidate.collection, candidate);
        }
    }
    report.sky_object_collections = candidates_by_collection.len();

    let base_collections = candidates_by_collection
        .values()
        .filter(|candidate| candidate.origin == ShadowkeepTableOrigin::BaseBubble)
        .map(|candidate| candidate.collection)
        .collect::<Vec<_>>();
    let selector =
        ConVars::get::<u32>("render.shadowkeep_sky_object_collection").unwrap_or_default();
    let selected_collections = if !base_collections.is_empty() {
        report.deferred_sky_object_collections = candidates_by_collection
            .keys()
            .copied()
            .filter(|collection| !base_collections.contains(collection))
            .collect();
        base_collections
    } else if candidates_by_collection.len() == 1 {
        candidates_by_collection.keys().copied().collect()
    } else if candidates_by_collection.len() > 1 {
        let selected = TagHash(selector);
        if selector != 0 && candidates_by_collection.contains_key(&selected) {
            report.deferred_sky_object_collections = candidates_by_collection
                .keys()
                .copied()
                .filter(|collection| *collection != selected)
                .collect();
            vec![selected]
        } else {
            report.deferred_sky_object_collections =
                candidates_by_collection.keys().copied().collect();
            let candidate_tags = report
                .deferred_sky_object_collections
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(", ");
            let candidate = candidates_by_collection
                .values()
                .next()
                .expect("non-empty candidate set");
            report.diagnostic(
                progress,
                MapLoadDiagnostic {
                    table: candidate.table,
                    entry_offset: candidate.entry_offset,
                    resource_class: SKY_OBJECT_PLACEMENT,
                    error: format!(
                        "multiple scenario sky-object collections ({candidate_tags}); set \
                         render.shadowkeep_sky_object_collection to one exact tag"
                    ),
                },
            );
            Vec::new()
        }
    } else {
        Vec::new()
    };
    report.sky_object_collection_tags = selected_collections.clone();

    for collection_tag in selected_collections {
        if progress.is_cancelled() {
            report.cancelled = true;
            break;
        }
        let candidate = candidates_by_collection[&collection_tag];
        let collection: SShadowkeepSkyObjectCollection =
            match package_manager().read_tag_struct(collection_tag) {
                Ok(collection) => collection,
                Err(error) => {
                    report.skipped_resources += 1;
                    report.diagnostic(
                        progress,
                        MapLoadDiagnostic {
                            table: candidate.table,
                            entry_offset: candidate.entry_offset,
                            resource_class: SKY_OBJECT_COLLECTION,
                            error: format!(
                                "sky-object collection {collection_tag}: could not decode: \
                                 {error:#}"
                            ),
                        },
                    );
                    continue;
                }
            };
        let object_count = collection.objects.len();
        let occlusion_count = collection.occlusion_bounds.len();
        let identifier_count = collection.identifiers.len();
        let common_count = object_count.min(occlusion_count).min(identifier_count);
        report.sky_object_records += object_count;
        tracing::info!(
            collection = %collection_tag,
            origin = candidate.origin.label(),
            objects = object_count,
            occlusion_bounds = occlusion_count,
            identifiers = identifier_count,
            "decoded Shadowkeep sky-object collection"
        );
        if object_count != occlusion_count || object_count != identifier_count {
            report.diagnostic(
                progress,
                MapLoadDiagnostic {
                    table: candidate.table,
                    entry_offset: candidate.entry_offset,
                    resource_class: SKY_OBJECT_COLLECTION,
                    error: format!(
                        "sky-object collection {collection_tag} parallel-array length mismatch: \
                         objects={object_count}, occlusion_bounds={occlusion_count}, \
                         identifiers={identifier_count}; processing common minimum {common_count}"
                    ),
                },
            );
        }

        for index in 0..common_count {
            if progress.is_cancelled() {
                report.cancelled = true;
                break;
            }
            let object = &collection.objects[index];
            let parallel_bound = &collection.occlusion_bounds[index].bb;
            let identifier = collection.identifiers[index];
            if object.bounds.min != parallel_bound.min || object.bounds.max != parallel_bound.max {
                report.diagnostic(
                    progress,
                    MapLoadDiagnostic {
                        table: candidate.table,
                        entry_offset: candidate.entry_offset,
                        resource_class: SKY_OBJECT_COLLECTION,
                        error: format!(
                            "sky-object collection {collection_tag} record {index} identifier \
                             {identifier} bounds differ from the parallel occlusion bound"
                        ),
                    },
                );
            }
            if object.unk70 == 5 {
                report.sky_object_skipped_kind_5 += 1;
                continue;
            }

            let model_tag = object.model.entity_model;
            if model_tag.is_some() {
                report.sky_object_model_tags.push(model_tag);
            }
            let result = (|| -> anyhow::Result<()> {
                anyhow::ensure!(model_tag.is_some(), "sky object has no entity model");
                package_manager()
                    .get_entry(model_tag)
                    .context("sky-object entity model package entry is missing")?;
                anyhow::ensure!(
                    object.bounds.min.is_finite()
                        && object.bounds.max.is_finite()
                        && parallel_bound.min.is_finite()
                        && parallel_bound.max.is_finite(),
                    "sky-object bounds contain non-finite values"
                );
                let matrix = Mat4::from_cols_array(&object.transform);
                anyhow::ensure!(
                    matrix.is_finite(),
                    "sky-object transform contains non-finite values"
                );
                let (scale, rotation, translation) = matrix.to_scale_rotation_translation();
                let transform = Transform::new(translation, rotation, scale);
                let model = DynamicModel::load_shadowkeep(model_tag, Vec::new(), Vec::new())?;
                let (model_center, model_radius) = model.model.bounding_sphere();
                let mut render_object =
                    RenderObject::new(TfxFeatureRenderer::SkyTransparent, model);
                let authored_stages = render_object.stages;
                report
                    .sky_object_stage_subscriptions
                    .push((model_tag, authored_stages.bits()));
                let permitted_stages = RenderStageSubscription::DECALS_ADDITIVE
                    | RenderStageSubscription::TRANSPARENTS;
                let rejected_stages = authored_stages & !permitted_stages;
                if !rejected_stages.is_empty() {
                    tracing::warn!(
                        collection = %collection_tag,
                        model = %model_tag,
                        authored_stage_mask = format_args!("0x{:08X}", authored_stages.bits()),
                        rejected_stage_mask = format_args!("0x{:08X}", rejected_stages.bits()),
                        "removed non-transparent stages from Shadowkeep sky object"
                    );
                    report.diagnostic(
                        progress,
                        MapLoadDiagnostic {
                            table: candidate.table,
                            entry_offset: candidate.entry_offset,
                            resource_class: SKY_OBJECT_COLLECTION,
                            error: format!(
                                "sky-object model {model_tag} subscribed to rejected stage mask \
                                 0x{:08X}; retained only DecalsAdditive/Transparents",
                                rejected_stages.bits(),
                            ),
                        },
                    );
                }
                render_object.stages &= permitted_stages;
                anyhow::ensure!(
                    !render_object.stages.is_empty(),
                    "sky-object model subscribes to no DecalsAdditive or Transparents stage"
                );
                let admitted_stage_mask = render_object.stages.bits();
                world.spawn((
                    transform,
                    DynamicRenderObject::new(renderer.add_object(render_object)),
                ));
                report.sky_object_render_objects += 1;
                progress
                    .gpu_assets_requested
                    .fetch_add(1, Ordering::Relaxed);
                progress
                    .visual_resources_loaded
                    .fetch_add(1, Ordering::Relaxed);
                tracing::info!(
                    collection = %collection_tag,
                    origin = candidate.origin.label(),
                    index,
                    identifier,
                    model = %model_tag,
                    translation = ?translation,
                    scale = ?scale,
                    model_center = ?model_center,
                    model_radius,
                    authored_stage_mask = format_args!("0x{:08X}", authored_stages.bits()),
                    decals_additive = authored_stages.is_subscribed(RenderStage::DecalsAdditive),
                    transparents = authored_stages.is_subscribed(RenderStage::Transparents),
                    admitted_stage_mask = format_args!("0x{admitted_stage_mask:08X}"),
                    "loaded Shadowkeep sky object"
                );
                Ok(())
            })();
            if let Err(error) = result {
                report.skipped_resources += 1;
                report.diagnostic(
                    progress,
                    MapLoadDiagnostic {
                        table: candidate.table,
                        entry_offset: candidate.entry_offset,
                        resource_class: SKY_OBJECT_COLLECTION,
                        error: format!(
                            "sky-object collection {collection_tag} record {index} model \
                             {model_tag}: {error:#}"
                        ),
                    },
                );
            }
        }
    }
    report.placement_bounds =
        (!bound_points.is_empty()).then(|| AxisAlignedBBox::from_points(&bound_points));
    report.entity_placement_bounds = (!entity_bound_points.is_empty())
        .then(|| AxisAlignedBBox::from_points(&entity_bound_points));
    report.world_bounds = [
        visual_bounds,
        report.placement_bounds,
        report.entity_placement_bounds,
    ]
    .into_iter()
    .flatten()
    .reduce(|left, right| left.union(&right));
    debug_assert!(report.entity_resource_accounting_is_complete());
    tracing::info!(
        map = %tag,
        scenario = ?report.scenario,
        activity_tables = report.activity_tables,
        static_placements = report.static_placements,
        static_render_objects = report.static_render_objects,
        terrain_placements = report.terrain_placements,
        terrain_render_objects = report.terrain_render_objects,
        entity_entries = report.entity_entries,
        rigid_entities = report.rigid_entities,
        rigid_render_objects = report.rigid_render_objects,
        entity_read_failures = report.entity_read_failures,
        entries_without_table_resource = report.entries_without_table_resource,
        light_collections = report.light_collections,
        light_render_objects = report.light_render_objects,
        cubemap_volumes = report.cubemap_volumes,
        atmosphere_placements = report.atmosphere_placements,
        sky_object_placements = report.sky_object_placements,
        sky_object_collections = report.sky_object_collections,
        sky_object_records = report.sky_object_records,
        sky_object_render_objects = report.sky_object_render_objects,
        sky_object_skipped_kind_5 = report.sky_object_skipped_kind_5,
        sky_object_collection_tags = ?report.sky_object_collection_tags,
        deferred_sky_object_collections = ?report.deferred_sky_object_collections,
        table_origins = ?report.table_origins,
        cubemap_render_objects = report.cubemap_render_objects,
        shadowing_lights = report.shadowing_lights,
        skipped_resources = report.skipped_resources,
        deferred_resources = report.deferred_resources,
        deferred_resource_classes = ?report.deferred_resource_classes,
        entity_resource_class_counts = ?report.entity_resource_class_counts,
        deferred_entity_resource_classes = ?report.deferred_entity_resource_classes,
        ?report.world_bounds,
        "completed Shadowkeep map normalization"
    );
    report.elapsed = started.elapsed();
    Ok((world, report))
}

#[cfg(test)]
mod tests {
    use glam::Vec3;
    use tiger_pkg::TagHash;

    use super::{EntityResourceExample, MapLoadProgress, MapLoadReport, bounded_offset};

    #[test]
    fn bounded_resource_offsets_reject_overflow_and_truncation() {
        assert_eq!(bounded_offset(0x40, 0x20, 0x10).unwrap(), 0x20);
        assert!(bounded_offset(0x40, 0x31, 0x10).is_err());
        assert!(bounded_offset(0x40, u64::MAX, 1).is_err());
    }

    #[test]
    fn cancellation_is_shared_with_progress_observers() {
        let progress = MapLoadProgress::default();
        let worker_view = progress.clone();
        assert!(!worker_view.is_cancelled());
        progress.cancel();
        assert!(worker_view.is_cancelled());
        assert!(worker_view.snapshot().cancelled);
    }

    #[test]
    fn deferred_entity_accounting_is_complete_and_examples_are_bounded() {
        let class = 0x8080_1234;
        let mut report = MapLoadReport::default();
        report.entity_resource_class_counts.insert(class, 7);

        for offset in 0..7 {
            report.defer_entity_resource(
                class,
                EntityResourceExample {
                    table: TagHash(1),
                    entity: TagHash(2),
                    resource: TagHash(3),
                    definition_offset: offset,
                    translation: Vec3::ZERO,
                },
            );
        }

        assert_eq!(report.deferred_resources, 7);
        assert_eq!(report.deferred_entity_resource_classes[&class], 7);
        assert_eq!(report.entity_resource_examples[&class].len(), 5);
        assert!(report.entity_resource_accounting_is_complete());
    }
}

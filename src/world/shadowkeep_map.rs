//! Shadowkeep map-chain decoding and scene normalization.
//!
//! The reader intentionally treats each table entry as an isolation boundary:
//! corrupt or unrecognised data is recorded in the report and does not prevent
//! the rest of a bubble from becoming viewable.

use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    fs,
    io::{Cursor, Seek, SeekFrom},
    sync::{
        Arc, OnceLock,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

use alkahest_core::ConVars;
use alkahest_data::{
    map::{ComponentData, SMapNodeTable, SRespawnPointsComponent},
    shadowkeep::{
        SShadowkeepBubbleDefinition, SShadowkeepBubbleParent, SShadowkeepCubemapPlacement,
        SShadowkeepDynamicModel, SShadowkeepEntity, SShadowkeepLightCollection,
        SShadowkeepMapDataTable, SShadowkeepOcclusionBounds, SShadowkeepRigidModelComponent,
        SShadowkeepShadowingLight, SShadowkeepSkyObjectCollection, SShadowkeepStaticPlacement,
        SShadowkeepTerrainPlacement, SShadowkeepTextureHeader,
    },
    tfx::{
        RenderStage, TfxFeatureRenderer, atmosphere::SShadowkeepAtmospherePlacement,
        common::AxisAlignedBBox, features::dynamic::RenderStageSubscription,
    },
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
    tfx::{
        packet::ShadowkeepSkyOrder,
        view::{View, ViewKind},
    },
};
use anyhow::Context;
use glam::{Mat4, Vec3, Vec4, Vec4Swizzles};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use tiger_parse::{PackageManagerExt, TigerReadable};
use tiger_pkg::{TagHash, package_manager};

use crate::world::{
    render_objects::{DynamicRenderObject, StaticRenderObject},
    shadowkeep_inspection::{
        MapEntityVisibility, MapInspectionBinding, MapInspectionGraphBuilder, MapInspectionNode,
        MapInspectionNodeId, MapInspectionNodeKind, MapInspectionSourceGroup,
    },
    shadowmap::ShadowMap,
    transform::Transform,
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

/// Resource dispatch class. The `SRespawnPointsComponent` payload type is
/// 0x8080_8CB5, but map component dispatch uses its 0x8080_8CB3 wrapper.
const RESPAWN_POINTS_COMPONENT: u32 = 0x8080_8CB3;
const MAP_NODE_TABLE_COMPONENT: u32 = 0x8080_92D8;
pub use crate::world::shadowkeep_inspection::{
    MapInspectionDisposition, ShadowkeepMapInspection, ShadowkeepTableSources,
};

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

fn transform_bounds(bounds: AxisAlignedBBox, matrix: Mat4) -> AxisAlignedBBox {
    let mut corners = [Vec3::ZERO; 8];
    for (index, corner) in corners.iter_mut().enumerate() {
        *corner = matrix.transform_point3(Vec3::new(
            if index & 1 == 0 {
                bounds.min.x
            } else {
                bounds.max.x
            },
            if index & 2 == 0 {
                bounds.min.y
            } else {
                bounds.max.y
            },
            if index & 4 == 0 {
                bounds.min.z
            } else {
                bounds.max.z
            },
        ));
    }
    AxisAlignedBBox::from_points(&corners)
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

pub fn shadowkeep_scenario_tables(map: TagHash) -> anyhow::Result<(Option<TagHash>, Vec<TagHash>)> {
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

/// A process-wide catalog discovered exactly once during application startup.
#[derive(Debug, Clone)]
pub struct ShadowkeepBubbleCatalogEntry {
    pub tag: TagHash,
    pub child_map: Option<TagHash>,
    pub package_name: String,
    pub package_path: String,
    pub map_name_hash: Option<u32>,
    pub display_name: String,
    pub search_text: String,
    pub container_count: usize,
    pub table_count: usize,
    pub scenario: Option<TagHash>,
    pub readable: bool,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct ShadowkeepBubbleCatalog {
    pub entries: Vec<ShadowkeepBubbleCatalogEntry>,
    pub package_names: Vec<String>,
}

static SHADOWKEEP_BUBBLE_CATALOG: OnceLock<ShadowkeepBubbleCatalog> = OnceLock::new();

pub fn initialize_shadowkeep_bubble_catalog(
    resolve_name: impl Fn(u32) -> Option<String>,
) -> anyhow::Result<&'static ShadowkeepBubbleCatalog> {
    if let Some(catalog) = SHADOWKEEP_BUBBLE_CATALOG.get() {
        return Ok(catalog);
    }
    let catalog = {
        let manager = package_manager();
        let mut entries = Vec::new();
        for (tag, _) in manager.get_all_by_reference(SShadowkeepBubbleParent::ID.unwrap()) {
            let package_name = manager.package_paths[&tag.pkg_id()].name.clone();
            let mut entry = ShadowkeepBubbleCatalogEntry {
                tag,
                child_map: None,
                package_path: package_name.clone(),
                package_name,
                map_name_hash: None,
                display_name: format!("Bubble {tag}"),
                search_text: String::new(),
                container_count: 0,
                table_count: 0,
                scenario: None,
                readable: false,
                error: None,
            };
            match (|| -> anyhow::Result<(SShadowkeepBubbleParent, SShadowkeepBubbleDefinition)> {
                let parent: SShadowkeepBubbleParent = manager.read_tag_struct(tag)?;
                let definition: SShadowkeepBubbleDefinition =
                    manager.read_tag_struct(parent.child_map)?;
                Ok((parent, definition))
            })() {
                Ok((parent, definition)) => {
                    entry.child_map = Some(parent.child_map);
                    entry.map_name_hash = Some(parent.map_name);
                    entry.display_name =
                        resolve_name(parent.map_name).unwrap_or_else(|| format!("Bubble {tag}"));
                    entry.container_count = definition.map_resources.len();
                    let mut tables = BTreeSet::new();
                    for container in &definition.map_resources {
                        tables.extend(container.data_tables.iter().copied());
                    }
                    match shadowkeep_scenario_tables(tag) {
                        Ok((scenario, scenario_tables)) => {
                            entry.scenario = scenario;
                            tables.extend(scenario_tables);
                        }
                        Err(error) => entry.error = Some(format!("freeroam scenario: {error:#}")),
                    }
                    entry.table_count = tables.len();
                    entry.readable = entry.error.is_none();
                }
                Err(error) => entry.error = Some(format!("{error:#}")),
            }
            entry.search_text = format!(
                "{} {} 0x{:08X} {} {}",
                entry.display_name, entry.tag, entry.tag.0, entry.package_name, entry.package_path
            )
            .to_ascii_lowercase();
            entries.push(entry);
        }
        entries.sort_unstable_by(|left, right| {
            left.package_name
                .cmp(&right.package_name)
                .then_with(|| left.tag.cmp(&right.tag))
        });
        let mut package_names = entries
            .iter()
            .map(|entry| entry.package_name.clone())
            .collect::<Vec<_>>();
        package_names.sort();
        package_names.dedup();
        ShadowkeepBubbleCatalog {
            entries,
            package_names,
        }
    };
    let _ = SHADOWKEEP_BUBBLE_CATALOG.set(catalog);
    Ok(SHADOWKEEP_BUBBLE_CATALOG
        .get()
        .expect("catalog was initialized immediately before lookup"))
}

pub fn shadowkeep_bubble_catalog() -> &'static ShadowkeepBubbleCatalog {
    SHADOWKEEP_BUBBLE_CATALOG
        .get()
        .expect("Shadowkeep bubble catalog must be initialized during App::new")
}

pub(crate) fn shadowkeep_bubble_catalog_matches(
    entry: &ShadowkeepBubbleCatalogEntry,
    query: &str,
) -> bool {
    query.trim().is_empty()
        || entry
            .search_text
            .contains(&query.trim().to_ascii_lowercase())
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

#[derive(Debug, Clone)]
struct SkyObjectPlacementCandidate {
    node: MapInspectionNodeId,
    collection: TagHash,
    table: TagHash,
    entry_offset: u64,
    sources: ShadowkeepTableSources,
    entity: TagHash,
    world_id: u64,
    entry_translation: Vec3,
}

#[derive(Debug, Clone, Default)]
struct SkyObjectCollectionEvidence {
    placements: Vec<SkyObjectPlacementCandidate>,
    object_count: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ShadowkeepEnvironmentSelectionReason {
    ExplicitSkyOverride,
    SharedSourceTable,
    SharedWorldId,
    DestinationPackage,
    SoleCandidates,
    RequestedCollectionMissing,
    RequestedCollectionEmpty,
    Ambiguous,
}

impl ShadowkeepEnvironmentSelectionReason {
    fn diagnostic(self) -> Option<&'static str> {
        match self {
            Self::ExplicitSkyOverride
            | Self::SharedSourceTable
            | Self::SharedWorldId
            | Self::DestinationPackage
            | Self::SoleCandidates => None,
            Self::RequestedCollectionMissing => {
                Some("requested sky-object collection was not discovered")
            }
            Self::RequestedCollectionEmpty => {
                Some("requested sky-object collection is empty and cannot define an environment")
            }
            Self::Ambiguous => {
                Some("could not prove one Shadowkeep sky/atmosphere environment pair")
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ShadowkeepEnvironmentSelection {
    sky_collection: Option<TagHash>,
    atmosphere_index: Option<usize>,
    reason: ShadowkeepEnvironmentSelectionReason,
    deferred_sky_collections: Vec<TagHash>,
    deferred_atmospheres: Vec<usize>,
}

fn select_shadowkeep_environment(
    candidates: &BTreeMap<TagHash, SkyObjectCollectionEvidence>,
    atmospheres: &[AtmospherePlacementCandidate],
    selector: u32,
    map_package_name: Option<&str>,
    collection_package_names: &BTreeMap<TagHash, String>,
) -> ShadowkeepEnvironmentSelection {
    let non_empty = candidates
        .iter()
        .filter(|(_, candidate)| candidate.object_count != 0)
        .map(|(collection, _)| *collection)
        .collect::<Vec<_>>();
    let all_deferred_sky = candidates.keys().copied().collect::<Vec<_>>();
    let all_deferred_atmospheres = (0..atmospheres.len()).collect::<Vec<_>>();
    let selected = |sky_collection, atmosphere_index, reason| ShadowkeepEnvironmentSelection {
        sky_collection: Some(sky_collection),
        atmosphere_index: Some(atmosphere_index),
        reason,
        deferred_sky_collections: candidates
            .keys()
            .copied()
            .filter(|candidate| *candidate != sky_collection)
            .collect(),
        deferred_atmospheres: (0..atmospheres.len())
            .filter(|candidate| *candidate != atmosphere_index)
            .collect(),
    };
    let unresolved = |reason| ShadowkeepEnvironmentSelection {
        sky_collection: None,
        atmosphere_index: None,
        reason,
        deferred_sky_collections: all_deferred_sky.clone(),
        deferred_atmospheres: all_deferred_atmospheres.clone(),
    };
    let unique_pair = |pairs: Vec<(TagHash, usize)>| {
        let pairs = pairs.into_iter().collect::<BTreeSet<_>>();
        (pairs.len() == 1).then(|| *pairs.first().expect("checked pair count"))
    };
    let pairs_for_sky = |sky_collection: TagHash,
                         predicate: &dyn Fn(
        &SkyObjectPlacementCandidate,
        &AtmospherePlacementCandidate,
    ) -> bool| {
        let mut pairs = Vec::new();
        for placement in &candidates[&sky_collection].placements {
            for (index, atmosphere) in atmospheres.iter().enumerate() {
                if predicate(placement, atmosphere) {
                    pairs.push((sky_collection, index));
                }
            }
        }
        unique_pair(pairs).map(|(_, atmosphere)| atmosphere)
    };
    let same_table = |placement: &SkyObjectPlacementCandidate,
                      atmosphere: &AtmospherePlacementCandidate| {
        placement.table == atmosphere.table
    };
    let same_meaningful_world =
        |placement: &SkyObjectPlacementCandidate, atmosphere: &AtmospherePlacementCandidate| {
            placement.world_id != 0
                && placement.world_id != u64::MAX
                && placement.world_id == atmosphere.world_id
        };
    let same_source_set = |placement: &SkyObjectPlacementCandidate,
                           atmosphere: &AtmospherePlacementCandidate| {
        placement.sources == atmosphere.sources
    };

    if selector != 0 {
        let requested = TagHash(selector);
        if !candidates.contains_key(&requested) {
            return unresolved(ShadowkeepEnvironmentSelectionReason::RequestedCollectionMissing);
        }
        if !non_empty.contains(&requested) {
            return unresolved(ShadowkeepEnvironmentSelectionReason::RequestedCollectionEmpty);
        }
        let atmosphere = pairs_for_sky(requested, &same_table)
            .or_else(|| pairs_for_sky(requested, &same_meaningful_world))
            .or_else(|| pairs_for_sky(requested, &same_source_set))
            .or_else(|| (atmospheres.len() == 1).then_some(0));
        return atmosphere.map_or_else(
            || unresolved(ShadowkeepEnvironmentSelectionReason::Ambiguous),
            |atmosphere| {
                selected(
                    requested,
                    atmosphere,
                    ShadowkeepEnvironmentSelectionReason::ExplicitSkyOverride,
                )
            },
        );
    }

    let pairs_with = |predicate: &dyn Fn(
        &SkyObjectPlacementCandidate,
        &AtmospherePlacementCandidate,
    ) -> bool| {
        let mut pairs = Vec::new();
        for sky_collection in &non_empty {
            for placement in &candidates[sky_collection].placements {
                for (index, atmosphere) in atmospheres.iter().enumerate() {
                    if predicate(placement, atmosphere) {
                        pairs.push((*sky_collection, index));
                    }
                }
            }
        }
        unique_pair(pairs)
    };
    if let Some((sky, atmosphere)) = pairs_with(&same_table) {
        return selected(
            sky,
            atmosphere,
            ShadowkeepEnvironmentSelectionReason::SharedSourceTable,
        );
    }
    if let Some((sky, atmosphere)) = pairs_with(&same_meaningful_world) {
        return selected(
            sky,
            atmosphere,
            ShadowkeepEnvironmentSelectionReason::SharedWorldId,
        );
    }

    let destination_collections = non_empty
        .iter()
        .copied()
        .filter(|collection| {
            map_package_name.is_some_and(|map_package| {
                collection_package_names
                    .get(collection)
                    .is_some_and(|package| package == map_package)
            })
        })
        .collect::<Vec<_>>();
    if let [sky] = destination_collections.as_slice() {
        if let Some(atmosphere) =
            pairs_for_sky(*sky, &same_source_set).or_else(|| (atmospheres.len() == 1).then_some(0))
        {
            return selected(
                *sky,
                atmosphere,
                ShadowkeepEnvironmentSelectionReason::DestinationPackage,
            );
        }
    }
    if let ([sky], [_]) = (non_empty.as_slice(), atmospheres) {
        return selected(
            *sky,
            0,
            ShadowkeepEnvironmentSelectionReason::SoleCandidates,
        );
    }
    unresolved(ShadowkeepEnvironmentSelectionReason::Ambiguous)
}

#[derive(Debug, Clone)]
struct AtmospherePlacementCandidate {
    node: MapInspectionNodeId,
    table: TagHash,
    entry_offset: u64,
    sources: ShadowkeepTableSources,
    world_id: u64,
    lookup_volume_0: TagHash,
    lookup_volume_1: TagHash,
    lookup_vertical: TagHash,
    lookup_table: TagHash,
    lookup_parameters: [Vec4; 4],
}

#[derive(Debug, Serialize)]
struct EnvironmentPlacementManifest {
    source_table: String,
    entry_offset: u64,
    base_containers: Vec<String>,
    referenced_by_freeroam_scenario: bool,
    world_id: u64,
    entity: String,
    entry_translation: [f32; 3],
}

#[derive(Debug, Serialize)]
struct SkyCollectionManifest {
    collection: String,
    occlusion_bound_count: usize,
    identifier_count: usize,
    object_count: usize,
    model_tags: Vec<String>,
    source_tables: Vec<String>,
    base_containers: Vec<String>,
    base_container_package_names: Vec<String>,
    referenced_by_freeroam_scenario: bool,
    placements: Vec<EnvironmentPlacementManifest>,
    object_records: Vec<SkyObjectRecordManifest>,
    identifiers: Vec<u32>,
    authored_aggregate_stage_mask: u32,
    package_name: Option<String>,
    package_path: Option<String>,
}

#[derive(Debug, Serialize)]
struct SkyObjectRecordManifest {
    index: usize,
    identifier: Option<u32>,
    transform: [f32; 16],
    bounds_min: [f32; 4],
    bounds_max: [f32; 4],
    parallel_occlusion_bounds_min: Option<[f32; 4]>,
    parallel_occlusion_bounds_max: Option<[f32; 4]>,
    model_wrapper: String,
    entity_model: String,
    authored_stage_mask: u32,
    unk64: f32,
    unk68: u32,
    unk6c: i16,
    unk6e: u16,
    unk70: u32,
    unk74: f32,
    unk78: u32,
    unk7c: String,
}

#[derive(Debug, Serialize)]
struct AtmosphereCandidateManifest {
    source_table: String,
    entry_offset: u64,
    base_containers: Vec<String>,
    referenced_by_freeroam_scenario: bool,
    world_id: u64,
    lookup_volume_0: String,
    lookup_volume_1: String,
    lookup_vertical: String,
    lookup_table: String,
    lookup_parameters: [[f32; 4]; 4],
}

#[derive(Debug, Serialize)]
struct EnvironmentPairingManifest {
    collection: String,
    atmosphere_table: String,
    same_source_table: bool,
    shared_base_containers: Vec<String>,
    same_world_id: bool,
    same_source_set: bool,
}

#[derive(Debug, Serialize)]
struct ShadowkeepEnvironmentCensusManifest {
    schema: &'static str,
    map: String,
    scenario: Option<String>,
    map_package_name: Option<String>,
    map_package_path: Option<String>,
    selection_reason: String,
    selected_sky_collection: Option<String>,
    deferred_sky_collections: Vec<String>,
    selected_atmosphere_table: Option<String>,
    deferred_atmosphere_tables: Vec<String>,
    sky_collections: Vec<SkyCollectionManifest>,
    atmosphere_candidates: Vec<AtmosphereCandidateManifest>,
    possible_pairings: Vec<EnvironmentPairingManifest>,
}

fn tag_package_metadata(tag: TagHash) -> (Option<String>, Option<String>) {
    package_manager()
        .package_paths
        .get(&tag.pkg_id())
        .map(|package| (Some(package.name.clone()), Some(package.path.clone())))
        .unwrap_or_default()
}

fn shadowkeep_model_stage_mask(model_tag: TagHash) -> Option<u32> {
    let model = package_manager()
        .read_tag_struct::<SShadowkeepDynamicModel>(model_tag)
        .ok()?;
    Some(
        model
            .meshes
            .iter()
            .fold(RenderStageSubscription::empty(), |mask, mesh| {
                mask | RenderStageSubscription::from_partrange_list(
                    &mesh.part_range_per_render_stage,
                )
            })
            .bits(),
    )
}

fn sky_collection_stage_mask(collection: &SShadowkeepSkyObjectCollection) -> u32 {
    collection
        .objects
        .iter()
        .map(|object| object.model.entity_model)
        .filter(|tag| tag.is_some())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .filter_map(shadowkeep_model_stage_mask)
        .fold(0, |mask, stages| mask | stages)
}

fn write_environment_census(
    map: TagHash,
    scenario: Option<TagHash>,
    candidates: &BTreeMap<TagHash, SkyObjectCollectionEvidence>,
    decoded_collections: &BTreeMap<TagHash, SShadowkeepSkyObjectCollection>,
    atmospheres: &[AtmospherePlacementCandidate],
    selection: &ShadowkeepEnvironmentSelection,
) -> anyhow::Result<()> {
    if !ConVars::get_flag("render.shadowkeep_environment_census") {
        return Ok(());
    }

    let (map_package_name, map_package_path) = tag_package_metadata(map);
    let sky_collections = candidates
        .iter()
        .map(|(collection_tag, evidence)| {
            let collection = decoded_collections.get(collection_tag);
            let model_tags = collection
                .into_iter()
                .flat_map(|collection| &collection.objects)
                .map(|object| object.model.entity_model)
                .filter(|tag| tag.is_some())
                .collect::<BTreeSet<_>>();
            let model_stage_masks = model_tags
                .iter()
                .filter_map(|model| {
                    shadowkeep_model_stage_mask(*model).map(|stages| (*model, stages))
                })
                .collect::<BTreeMap<_, _>>();
            let object_records = collection
                .into_iter()
                .flat_map(|collection| collection.objects.iter().enumerate())
                .map(|(index, object)| {
                    let parallel_bounds =
                        collection.and_then(|collection| collection.occlusion_bounds.get(index));
                    SkyObjectRecordManifest {
                        index,
                        identifier: collection
                            .and_then(|collection| collection.identifiers.get(index).copied()),
                        transform: object.transform,
                        bounds_min: object.bounds.min.to_array(),
                        bounds_max: object.bounds.max.to_array(),
                        parallel_occlusion_bounds_min: parallel_bounds
                            .map(|bounds| bounds.bb.min.to_array()),
                        parallel_occlusion_bounds_max: parallel_bounds
                            .map(|bounds| bounds.bb.max.to_array()),
                        model_wrapper: object.model.taghash().to_string(),
                        entity_model: object.model.entity_model.to_string(),
                        authored_stage_mask: model_stage_masks
                            .get(&object.model.entity_model)
                            .copied()
                            .unwrap_or_default(),
                        unk64: object.unk64,
                        unk68: object.unk68,
                        unk6c: object.unk6c,
                        unk6e: object.unk6e,
                        unk70: object.unk70,
                        unk74: object.unk74,
                        unk78: object.unk78,
                        unk7c: object.unk7c.to_string(),
                    }
                })
                .collect();
            let source_tables = evidence
                .placements
                .iter()
                .map(|placement| placement.table)
                .collect::<BTreeSet<_>>();
            let base_containers = evidence
                .placements
                .iter()
                .flat_map(|placement| placement.sources.base_containers.iter().copied())
                .collect::<BTreeSet<_>>();
            let (package_name, package_path) = tag_package_metadata(*collection_tag);
            SkyCollectionManifest {
                collection: collection_tag.to_string(),
                occlusion_bound_count: collection
                    .map_or(0, |collection| collection.occlusion_bounds.len()),
                identifier_count: collection.map_or(0, |collection| collection.identifiers.len()),
                object_count: collection.map_or(0, |collection| collection.objects.len()),
                model_tags: model_tags.iter().map(ToString::to_string).collect(),
                source_tables: source_tables.iter().map(ToString::to_string).collect(),
                base_containers: base_containers.iter().map(ToString::to_string).collect(),
                base_container_package_names: base_containers
                    .iter()
                    .filter_map(|tag| tag_package_metadata(*tag).0)
                    .collect::<BTreeSet<_>>()
                    .into_iter()
                    .collect(),
                referenced_by_freeroam_scenario: evidence
                    .placements
                    .iter()
                    .any(|placement| placement.sources.referenced_by_freeroam_scenario),
                placements: evidence
                    .placements
                    .iter()
                    .map(|placement| EnvironmentPlacementManifest {
                        source_table: placement.table.to_string(),
                        entry_offset: placement.entry_offset,
                        base_containers: placement
                            .sources
                            .base_containers
                            .iter()
                            .map(ToString::to_string)
                            .collect(),
                        referenced_by_freeroam_scenario: placement
                            .sources
                            .referenced_by_freeroam_scenario,
                        world_id: placement.world_id,
                        entity: placement.entity.to_string(),
                        entry_translation: placement.entry_translation.to_array(),
                    })
                    .collect(),
                object_records,
                identifiers: collection
                    .map(|collection| collection.identifiers.clone())
                    .unwrap_or_default(),
                authored_aggregate_stage_mask: collection
                    .map(sky_collection_stage_mask)
                    .unwrap_or_default(),
                package_name,
                package_path,
            }
        })
        .collect();
    let atmosphere_candidates = atmospheres
        .iter()
        .map(|candidate| AtmosphereCandidateManifest {
            source_table: candidate.table.to_string(),
            entry_offset: candidate.entry_offset,
            base_containers: candidate
                .sources
                .base_containers
                .iter()
                .map(ToString::to_string)
                .collect(),
            referenced_by_freeroam_scenario: candidate.sources.referenced_by_freeroam_scenario,
            world_id: candidate.world_id,
            lookup_volume_0: candidate.lookup_volume_0.to_string(),
            lookup_volume_1: candidate.lookup_volume_1.to_string(),
            lookup_vertical: candidate.lookup_vertical.to_string(),
            lookup_table: candidate.lookup_table.to_string(),
            lookup_parameters: candidate.lookup_parameters.map(|value| value.to_array()),
        })
        .collect();
    let possible_pairings = candidates
        .iter()
        .flat_map(|(collection, evidence)| {
            atmospheres.iter().map(move |atmosphere| {
                let shared_base_containers = evidence
                    .placements
                    .iter()
                    .flat_map(|placement| {
                        placement
                            .sources
                            .base_containers
                            .intersection(&atmosphere.sources.base_containers)
                            .copied()
                    })
                    .collect::<BTreeSet<_>>();
                EnvironmentPairingManifest {
                    collection: collection.to_string(),
                    atmosphere_table: atmosphere.table.to_string(),
                    same_source_table: evidence
                        .placements
                        .iter()
                        .any(|placement| placement.table == atmosphere.table),
                    shared_base_containers: shared_base_containers
                        .iter()
                        .map(ToString::to_string)
                        .collect(),
                    same_world_id: evidence
                        .placements
                        .iter()
                        .any(|placement| placement.world_id == atmosphere.world_id),
                    same_source_set: evidence
                        .placements
                        .iter()
                        .any(|placement| placement.sources == atmosphere.sources),
                }
            })
        })
        .collect();
    let manifest = ShadowkeepEnvironmentCensusManifest {
        schema: "alkahest-shadowkeep-environment-census/v2",
        map: map.to_string(),
        scenario: scenario.map(|tag| tag.to_string()),
        map_package_name,
        map_package_path,
        selection_reason: format!("{:?}", selection.reason),
        selected_sky_collection: selection.sky_collection.map(|tag| tag.to_string()),
        deferred_sky_collections: selection
            .deferred_sky_collections
            .iter()
            .map(ToString::to_string)
            .collect(),
        selected_atmosphere_table: selection
            .atmosphere_index
            .and_then(|index| atmospheres.get(index))
            .map(|candidate| candidate.table.to_string()),
        deferred_atmosphere_tables: selection
            .deferred_atmospheres
            .iter()
            .filter_map(|index| atmospheres.get(*index))
            .map(|candidate| candidate.table.to_string())
            .collect(),
        sky_collections,
        atmosphere_candidates,
        possible_pairings,
    };
    fs::create_dir_all("artifacts")?;
    let path = format!("artifacts/shadowkeep-environment-census-{map}.json");
    fs::write(&path, serde_json::to_vec_pretty(&manifest)?)?;
    tracing::info!(%path, "wrote Shadowkeep environment census");
    Ok(())
}
fn load_shadowkeep_atmosphere(
    renderer: &Renderer,
    candidate: &AtmospherePlacementCandidate,
) -> anyhow::Result<AtmosphereData> {
    let read_lookup_header = |tag| {
        package_manager()
            .read_tag_struct::<SShadowkeepTextureHeader>(tag)
            .with_context(|| format!("failed to read atmosphere texture header {tag}"))
    };
    let lookup_volume_0 = read_lookup_header(candidate.lookup_volume_0)?;
    let lookup_volume_1 = read_lookup_header(candidate.lookup_volume_1)?;
    let lookup_vertical = read_lookup_header(candidate.lookup_vertical)?;
    let lookup_table_entry = package_manager()
        .get_entry(candidate.lookup_table)
        .context("lookup table package entry disappeared")?;
    let lookup_table_bytes = package_manager().read_tag(candidate.lookup_table)?;
    anyhow::ensure!(
        lookup_table_bytes.len() == SHADOWKEEP_LOOKUP_TABLE_BYTES,
        "lookup table {} has {} bytes; expected {}",
        candidate.lookup_table,
        lookup_table_bytes.len(),
        SHADOWKEEP_LOOKUP_TABLE_BYTES,
    );
    let lookup_table = Texture::load_2d_raw(
        &renderer.gpu.device,
        64,
        64,
        &lookup_table_bytes,
        d3d11::dxgi::Format::R8g8b8a8UnormSrgb,
        Some("Shadowkeep atmosphere lookup table"),
        false,
    )?;
    let lookup_table_min = lookup_table_bytes.iter().copied().min().unwrap_or(0);
    let lookup_table_max = lookup_table_bytes.iter().copied().max().unwrap_or(0);
    let lookup_table_mean = lookup_table_bytes
        .iter()
        .map(|&value| f64::from(value))
        .sum::<f64>()
        / lookup_table_bytes.len() as f64;
    tracing::info!(
        table = %candidate.table,
        entry_offset = candidate.entry_offset,
        lookup_table = %candidate.lookup_table,
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
        lookup_parameters = ?candidate.lookup_parameters,
        "loaded authored Shadowkeep atmosphere inputs"
    );
    Ok(AtmosphereData {
        shadowkeep_lookup_volume_0: renderer.asset_manager.load(candidate.lookup_volume_0),
        shadowkeep_lookup_volume_1: renderer.asset_manager.load(candidate.lookup_volume_1),
        shadowkeep_lookup_vertical: renderer.asset_manager.load(candidate.lookup_vertical),
        shadowkeep_lookup_table: Some(lookup_table),
        shadowkeep_lookup_parameters: candidate.lookup_parameters,
        ..Default::default()
    })
}

#[derive(Debug, Clone, Default)]
pub struct MapLoadReport {
    pub map: TagHash,
    pub inspection: ShadowkeepMapInspection,
    pub containers: usize,
    pub scenario: Option<TagHash>,
    pub activity_tables: usize,
    pub tables: usize,
    pub table_sources: BTreeMap<TagHash, ShadowkeepTableSources>,
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
    pub environment_selection_reason: Option<String>,
    pub selected_atmosphere_table: Option<TagHash>,
    pub sky_object_stage_subscriptions: Vec<(TagHash, u32)>,
    /// Authored stages that contribute sky color in the dedicated pass.
    pub sky_object_color_stage_mask: u32,
    /// Authored stages intentionally deferred because the legacy target is not restored.
    pub sky_object_deferred_stage_mask: u32,
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

/// Opt-in corpus evidence collected from completed map normalizations.
///
/// This is intentionally CPU-only and written only when the explicit matrix
/// diagnostic is armed; ordinary launches must not create artifacts.
#[derive(Debug, Default, Serialize, Deserialize)]
struct ShadowkeepProductionFeatureMatrix {
    schema: String,
    maps: BTreeMap<String, ShadowkeepProductionFeatureMap>,
}

#[derive(Debug, Serialize, Deserialize)]
struct ShadowkeepProductionFeatureMap {
    table_resource_classes: BTreeMap<String, usize>,
    entity_resource_classes: BTreeMap<String, usize>,
    loaded_entity_resource_classes: BTreeMap<String, usize>,
    deferred_entity_resource_classes: BTreeMap<String, usize>,
    feature_renderers: BTreeMap<String, usize>,
    #[serde(default)]
    feature_families: BTreeMap<String, ShadowkeepFeatureFamilyMatrix>,
}

#[derive(Debug, Serialize, Deserialize)]
struct ShadowkeepFeatureFamilyMatrix {
    present_in_map: bool,
    loader_implemented: bool,
    render_pass_implemented: bool,
    production_status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<String>,
}

fn class_counts(counts: &BTreeMap<u32, usize>) -> BTreeMap<String, usize> {
    counts
        .iter()
        .map(|(class, count)| (format!("{class:08X}"), *count))
        .collect()
}

fn has_resource_class(report: &MapLoadReport, class: u32) -> bool {
    report.resource_class_counts.contains_key(&class)
        || report.entity_resource_class_counts.contains_key(&class)
}

fn feature_family(
    present_in_map: bool,
    loader_implemented: bool,
    render_pass_implemented: bool,
) -> ShadowkeepFeatureFamilyMatrix {
    ShadowkeepFeatureFamilyMatrix {
        present_in_map,
        loader_implemented,
        render_pass_implemented,
        production_status: if !present_in_map {
            "AbsentCorpus"
        } else if loader_implemented && render_pass_implemented {
            "Ready"
        } else {
            "Deferred"
        }
        .to_owned(),
        reason: None,
    }
}

fn degraded_feature_family(
    present_in_map: bool,
    loader_implemented: bool,
    render_pass_implemented: bool,
    reason: &'static str,
) -> ShadowkeepFeatureFamilyMatrix {
    ShadowkeepFeatureFamilyMatrix {
        present_in_map,
        loader_implemented,
        render_pass_implemented,
        production_status: "Degraded".to_owned(),
        reason: Some(reason.to_owned()),
    }
}

fn write_shadowkeep_production_feature_matrix(report: &MapLoadReport) -> anyhow::Result<()> {
    if !ConVars::get_flag("render.shadowkeep_feature_matrix") {
        return Ok(());
    }

    let path = std::path::Path::new("artifacts/shadowkeep-production-feature-matrix.json");
    let mut matrix: ShadowkeepProductionFeatureMatrix = fs::read(path)
        .ok()
        .map(|bytes| serde_json::from_slice(&bytes))
        .transpose()?
        .unwrap_or_default();
    matrix.schema = "alkahest-shadowkeep-production-feature-matrix/v1".to_owned();
    matrix.maps.insert(
        report.map.to_string(),
        ShadowkeepProductionFeatureMap {
            table_resource_classes: class_counts(&report.resource_class_counts),
            entity_resource_classes: class_counts(&report.entity_resource_class_counts),
            loaded_entity_resource_classes: class_counts(&report.loaded_entity_resource_classes),
            deferred_entity_resource_classes: class_counts(
                &report.deferred_entity_resource_classes,
            ),
            feature_renderers: BTreeMap::from([
                (
                    "ChunkedInstanceObjects".to_owned(),
                    report.static_render_objects,
                ),
                ("TerrainPatch".to_owned(), report.terrain_render_objects),
                ("DeferredLights".to_owned(), report.light_render_objects),
                ("Cubemaps".to_owned(), report.cubemap_render_objects),
                (
                    "SkyTransparent".to_owned(),
                    report.sky_object_render_objects,
                ),
                ("RigidObject".to_owned(), report.rigid_render_objects),
            ]),
            feature_families: BTreeMap::from([
                (
                    "static_geometry".to_owned(),
                    feature_family(
                        has_resource_class(report, STATIC_PLACEMENT),
                        true,
                        report.static_render_objects != 0,
                    ),
                ),
                (
                    "terrain".to_owned(),
                    feature_family(
                        has_resource_class(report, TERRAIN_PLACEMENT),
                        true,
                        report.terrain_render_objects != 0,
                    ),
                ),
                (
                    "local_lights".to_owned(),
                    feature_family(
                        has_resource_class(report, LIGHT_COLLECTION),
                        true,
                        report.light_render_objects != 0,
                    ),
                ),
                (
                    "shadowing_lights".to_owned(),
                    degraded_feature_family(
                        has_resource_class(report, SHADOWING_LIGHT),
                        true,
                        true,
                        "Shadow producers and consumers are wired; non-clear depth and visible \
                         attenuation require one-shot provenance capture.",
                    ),
                ),
                (
                    "cubemap_ibl".to_owned(),
                    feature_family(
                        has_resource_class(report, CUBEMAP_VOLUME),
                        true,
                        report.cubemap_render_objects != 0,
                    ),
                ),
                (
                    "atmosphere".to_owned(),
                    feature_family(
                        has_resource_class(report, ATMOSPHERE_PLACEMENT),
                        true,
                        report.atmosphere_placements != 0,
                    ),
                ),
                (
                    "sky_objects".to_owned(),
                    feature_family(
                        has_resource_class(report, SKY_OBJECT_PLACEMENT),
                        true,
                        report.sky_object_render_objects != 0,
                    ),
                ),
                (
                    "rigid_models".to_owned(),
                    feature_family(
                        has_resource_class(report, RIGID_MODEL_COMPONENT),
                        true,
                        report.rigid_render_objects != 0,
                    ),
                ),
            ]),
        },
    );
    fs::create_dir_all("artifacts")?;
    fs::write(path, serde_json::to_vec_pretty(&matrix)?)?;
    Ok(())
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
    inspection: &mut MapInspectionGraphBuilder,
    node: MapInspectionNodeId,
    transform: Transform,
    resource_tag: TagHash,
    definition_offset: u64,
) -> anyhow::Result<hecs::Entity> {
    let bytes = package_manager().read_tag(resource_tag)?;
    let mut cursor = Cursor::new(bytes);
    cursor.seek(SeekFrom::Start(definition_offset))?;
    let component = SShadowkeepRigidModelComponent::read_ds(&mut cursor)?;
    let model = DynamicModel::load_shadowkeep(
        component.model,
        component.material_variants,
        component.techniques,
    )?;
    let (center, radius) = model.model.bounding_sphere();
    let radius = radius * transform.scale.abs().max_element();
    inspection.node_mut(node).bounds = Some(AxisAlignedBBox::from_center_extents(
        transform.translation + transform.rotation * (center * transform.scale),
        Vec3::splat(radius),
    ));
    progress
        .gpu_assets_requested
        .fetch_add(1, Ordering::Relaxed);
    let entity = world.spawn((
        transform,
        DynamicRenderObject::new(
            renderer.add_object(RenderObject::new(TfxFeatureRenderer::DynamicObjects, model)),
        ),
        MapInspectionBinding { node },
        MapEntityVisibility::default(),
    ));
    inspection.bind_world_entity(node, entity)?;
    progress
        .visual_resources_loaded
        .fetch_add(1, Ordering::Relaxed);
    Ok(entity)
}

fn collect_shadowkeep_respawn_points(
    report: &mut MapLoadReport,
    world: &mut hecs::World,
    inspection: &mut MapInspectionGraphBuilder,
    parent: MapInspectionNodeId,
    resource: TagHash,
    definition_offset: u64,
    source: ShadowkeepTableSources,
) -> anyhow::Result<()> {
    let bytes = package_manager().read_tag(resource)?;
    let offset = bounded_offset(
        bytes.len(),
        definition_offset,
        SRespawnPointsComponent::SIZE,
    )?;
    let mut cursor = Cursor::new(&bytes[offset..]);
    let component = SRespawnPointsComponent::read_ds(&mut cursor)?;
    let Some(respawns) = &*component.tag else {
        anyhow::bail!("respawn point collection tag is absent");
    };
    for (index, spawn) in respawns.unk8.iter().enumerate() {
        let transform = Transform::new(spawn.translation.xyz(), spawn.rotation, Vec3::ONE);
        let mut node = MapInspectionNode::new(
            MapInspectionNodeKind::SpawnPoint,
            MapInspectionDisposition::NonRendering,
            format!("Spawn {:08X}", spawn.unk20),
            source.clone(),
        );
        node.tag = Some(component.tag.taghash());
        node.definition_offset = Some(definition_offset);
        node.element_index = Some(index);
        node.transform = Some(transform);
        node.name_hash = Some(spawn.unk20);
        let node_id = inspection.add_node(Some(parent), node);
        let entity = world.spawn((transform, MapInspectionBinding { node: node_id }));
        inspection.bind_world_entity(node_id, entity)?;
        report.spawn_points.push(transform.translation);
    }
    Ok(())
}

fn collect_shadowkeep_map_node_respawns(
    report: &mut MapLoadReport,
    world: &mut hecs::World,
    inspection: &mut MapInspectionGraphBuilder,
    parent: MapInspectionNodeId,
    loaded_node_tables: &mut HashSet<TagHash>,
    resource: TagHash,
    definition_offset: u64,
    source: ShadowkeepTableSources,
) -> anyhow::Result<()> {
    let bytes = package_manager().read_tag(resource)?;
    let offset = bounded_offset(bytes.len(), definition_offset + 0x84, TagHash::SIZE)?;
    let mut cursor = Cursor::new(&bytes[offset..]);
    let node_table_tag = TagHash::read_ds(&mut cursor)?;
    anyhow::ensure!(node_table_tag.is_some(), "map-node table tag is absent");
    if !loaded_node_tables.insert(node_table_tag) {
        return Ok(());
    }
    let node_table: SMapNodeTable = package_manager().read_tag_struct(node_table_tag)?;
    for (node_index, node) in node_table.nodes.iter().enumerate() {
        for (component_index, component) in node.component_data.iter().enumerate() {
            let ComponentData::SRespawnPointsComponent(component) = component else {
                continue;
            };
            let Some(points) = &*component.tag else {
                continue;
            };
            for (point_index, point) in points.unk8.iter().enumerate() {
                let transform = Transform::new(point.translation.xyz(), point.rotation, Vec3::ONE);
                let mut inspection_node = MapInspectionNode::new(
                    MapInspectionNodeKind::SpawnPoint,
                    MapInspectionDisposition::NonRendering,
                    format!(
                        "Spawn {:08X} · node table {node_table_tag} / \
                         {node_index}:{component_index}:{point_index}",
                        point.unk20
                    ),
                    source.clone(),
                );
                inspection_node.tag = Some(component.tag.taghash());
                inspection_node.definition_offset = Some(definition_offset);
                // Stable authored path metadata is represented without inventing
                // an unproven transform composition: node/component/point order
                // remains encoded in this unique element ordinal.
                inspection_node.element_index = Some(
                    node_index
                        .checked_mul(1_000_000)
                        .and_then(|value| value.checked_add(component_index * 1_000))
                        .and_then(|value| value.checked_add(point_index))
                        .context("map-node spawn path index overflow")?,
                );
                inspection_node.transform = Some(transform);
                inspection_node.name_hash = Some(point.unk20);
                let node_id = inspection.add_node(Some(parent), inspection_node);
                let entity = world.spawn((transform, MapInspectionBinding { node: node_id }));
                inspection.bind_world_entity(node_id, entity)?;
                report.spawn_points.push(transform.translation);
            }
        }
    }
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
        inspection: ShadowkeepMapInspection {
            bubble: tag,
            child_map: parent.child_map,
            ..Default::default()
        },
        ..Default::default()
    };
    report.containers = definition.map_resources.len();
    let mut table_sources = BTreeMap::<TagHash, ShadowkeepTableSources>::new();
    for container in &definition.map_resources {
        let container_tag = container.1;
        for table in &container.data_tables {
            table_sources
                .entry(*table)
                .or_default()
                .base_containers
                .insert(container_tag);
        }
    }
    match shadowkeep_scenario_tables(tag) {
        Ok((scenario, tables)) => {
            report.scenario = scenario;
            report.activity_tables = tables.len();
            for table in tables {
                let sources = table_sources.entry(table).or_default();
                sources.referenced_by_freeroam_scenario = true;
                sources.scenario = scenario;
            }
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
    report.table_sources = table_sources.clone();
    progress
        .total_tables
        .store(table_sources.len(), Ordering::Relaxed);
    let mut inspection = MapInspectionGraphBuilder::new(tag, parent.child_map);
    let mut source_groups = BTreeSet::new();
    for sources in table_sources.values() {
        source_groups.extend(
            sources
                .base_containers
                .iter()
                .copied()
                .map(MapInspectionSourceGroup::BaseContainer),
        );
        if let Some(scenario) = sources.scenario {
            source_groups.insert(MapInspectionSourceGroup::Scenario(scenario));
        }
    }
    for group in source_groups {
        inspection.add_source_group(group);
    }
    let mut world = hecs::World::new();
    let mut bound_points = Vec::new();
    let mut entity_bound_points = Vec::new();
    let mut loaded_static_collections = HashMap::new();
    let mut loaded_terrain_resources = HashMap::new();
    let mut loaded_node_tables = HashSet::new();
    let mut visual_bounds: Option<AxisAlignedBBox> = None;
    let mut sky_object_candidates = Vec::new();
    let mut atmosphere_candidates = Vec::new();

    for (table_hash, table_sources) in table_sources {
        if progress.is_cancelled() {
            report.cancelled = true;
            break;
        }
        let mut table_node = MapInspectionNode::new(
            MapInspectionNodeKind::Table,
            MapInspectionDisposition::NonRendering,
            format!("Table {table_hash}"),
            table_sources.clone(),
        );
        table_node.tag = Some(table_hash);
        let table_id = inspection.add_node(Some(inspection.root()), table_node);
        for container in &table_sources.base_containers {
            inspection.reference_table(
                &MapInspectionSourceGroup::BaseContainer(*container),
                table_id,
            );
        }
        if let Some(scenario) = table_sources.scenario {
            inspection.reference_table(&MapInspectionSourceGroup::Scenario(scenario), table_id);
        }
        let table: SShadowkeepMapDataTable = match package_manager().read_tag_struct(table_hash) {
            Ok(table) => table,
            Err(error) => {
                let error = format!("could not decode map data table: {error:#}");
                report.skipped_resources += 1;
                report.diagnostic(
                    progress,
                    MapLoadDiagnostic {
                        table: table_hash,
                        entry_offset: 0,
                        resource_class: 0,
                        error: error.clone(),
                    },
                );
                let node = inspection.node_mut(table_id);
                node.disposition = MapInspectionDisposition::Failed;
                node.error = Some(error);
                continue;
            }
        };
        let table_bytes = match package_manager().read_tag(table_hash) {
            Ok(bytes) => bytes,
            Err(error) => {
                let error = format!("could not read map data table bytes: {error:#}");
                report.skipped_resources += table.data_entries.len();
                report.diagnostic(
                    progress,
                    MapLoadDiagnostic {
                        table: table_hash,
                        entry_offset: 0,
                        resource_class: 0,
                        error: error.clone(),
                    },
                );
                let node = inspection.node_mut(table_id);
                node.disposition = MapInspectionDisposition::Failed;
                node.error = Some(error);
                continue;
            }
        };
        report.tables += 1;
        let mut entry_index = 0usize;
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
            let table_resource_class = entry
                .data_resource
                .is_valid
                .then_some(entry.data_resource.resource_type);
            let mut entry_node = MapInspectionNode::new(
                MapInspectionNodeKind::Entry,
                match table_resource_class {
                    Some(
                        STATIC_PLACEMENT | TERRAIN_PLACEMENT | LIGHT_COLLECTION | SHADOWING_LIGHT
                        | CUBEMAP_VOLUME | ATMOSPHERE_PLACEMENT | SKY_OBJECT_PLACEMENT,
                    ) => MapInspectionDisposition::Rendering,
                    Some(_) => MapInspectionDisposition::Deferred,
                    None => MapInspectionDisposition::NonRendering,
                },
                format!("Entry {entry_index}"),
                table_sources.clone(),
            );
            entry_node.tag = (!entry.entity.is_none()).then_some(entry.entity);
            entry_node.entry_index = Some(entry_index);
            entry_node.world_id = Some(entry.world_id);
            entry_node.transform = Some(transform);
            let entry_id = inspection.add_node(Some(table_id), entry_node);
            let table_semantic_id = entry.data_resource.is_valid.then(|| {
                let mut table_resource_node = MapInspectionNode::new(
                    MapInspectionNodeKind::TableResource,
                    MapInspectionDisposition::NonRendering,
                    format!("Table Resource {:08X}", entry.data_resource.resource_type),
                    table_sources.clone(),
                );
                table_resource_node.tag = Some(table_hash);
                table_resource_node.class = Some(entry.data_resource.resource_type);
                table_resource_node.entry_index = Some(entry_index);
                table_resource_node.definition_offset = Some(entry.data_resource.offset);
                let table_resource_id = inspection.add_node(Some(entry_id), table_resource_node);
                let semantic_kind = match entry.data_resource.resource_type {
                    STATIC_PLACEMENT => MapInspectionNodeKind::StaticGeometry,
                    TERRAIN_PLACEMENT => MapInspectionNodeKind::Terrain,
                    LIGHT_COLLECTION => MapInspectionNodeKind::LightCollection,
                    SHADOWING_LIGHT => MapInspectionNodeKind::ShadowingLight,
                    CUBEMAP_VOLUME => MapInspectionNodeKind::Cubemap,
                    ATMOSPHERE_PLACEMENT => MapInspectionNodeKind::Atmosphere,
                    SKY_OBJECT_PLACEMENT => MapInspectionNodeKind::SkyCollection,
                    _ => MapInspectionNodeKind::DeferredResource,
                };
                let mut semantic_node = MapInspectionNode::new(
                    semantic_kind,
                    if semantic_kind == MapInspectionNodeKind::DeferredResource {
                        MapInspectionDisposition::Deferred
                    } else {
                        MapInspectionDisposition::Rendering
                    },
                    format!("{semantic_kind:?}"),
                    table_sources.clone(),
                );
                semantic_node.class = Some(entry.data_resource.resource_type);
                semantic_node.entry_index = Some(entry_index);
                semantic_node.definition_offset = Some(entry.data_resource.offset);
                semantic_node.transform = Some(transform);
                inspection.add_node(Some(table_resource_id), semantic_node)
            });
            entry_index += 1;
            let mut loaded_entity_visual = false;
            // Entity payloads are independent of the optional table-local resource pointer.
            // Process them first so entity-only entries are not discarded by the guard below.
            if !entry.entity.is_none() {
                report.entity_entries += 1;
                match package_manager().read_tag_struct::<SShadowkeepEntity>(entry.entity) {
                    Ok(entity) => {
                        for (resource_index, resource_ref) in
                            entity.entity_resources.iter().enumerate()
                        {
                            if progress.is_cancelled() {
                                report.cancelled = true;
                                break;
                            }

                            let resource_tag = resource_ref.resource.taghash();
                            let resource = &*resource_ref.resource;
                            let class = resource.resource.resource_type;
                            let valid = resource.resource.is_valid && resource.definition.is_valid;
                            let mut resource_node = MapInspectionNode::new(
                                MapInspectionNodeKind::EntityResource,
                                if valid {
                                    MapInspectionDisposition::NonRendering
                                } else {
                                    MapInspectionDisposition::Failed
                                },
                                format!("Entity Resource {resource_tag}"),
                                table_sources.clone(),
                            );
                            resource_node.tag = Some(resource_tag);
                            resource_node.entry_index = Some(entry_index - 1);
                            resource_node.element_index = Some(resource_index);
                            resource_node.definition_offset = Some(resource.definition.offset);
                            if !valid {
                                resource_node.error =
                                    Some("resource or definition pointer is invalid".to_owned());
                            }
                            let resource_id = inspection.add_node(Some(entry_id), resource_node);
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

                            if !valid {
                                report.defer_entity_resource(class, example);
                                continue;
                            }
                            let semantic_node = match class {
                                RIGID_MODEL_COMPONENT => Some(MapInspectionNodeKind::RigidModel),
                                RESPAWN_POINTS_COMPONENT | MAP_NODE_TABLE_COMPONENT => None,
                                _ => Some(MapInspectionNodeKind::DeferredResource),
                            };
                            let semantic_id = semantic_node.map(|kind| {
                                let mut node = MapInspectionNode::new(
                                    kind,
                                    if kind == MapInspectionNodeKind::DeferredResource {
                                        MapInspectionDisposition::Deferred
                                    } else {
                                        MapInspectionDisposition::Rendering
                                    },
                                    format!("{kind:?} {resource_tag}"),
                                    table_sources.clone(),
                                );
                                node.tag = Some(resource_tag);
                                node.class = Some(class);
                                node.definition_offset = Some(resource.definition.offset);
                                inspection.add_node(Some(resource_id), node)
                            });

                            match class {
                                RIGID_MODEL_COMPONENT => {
                                    report.rigid_entities += 1;
                                    if let Err(error) = load_shadowkeep_rigid_entity(
                                        renderer,
                                        progress,
                                        &mut world,
                                        &mut inspection,
                                        semantic_id.expect("rigid model has semantic node"),
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
                                        let node = inspection.node_mut(
                                            semantic_id.expect("rigid model has semantic node"),
                                        );
                                        node.disposition = MapInspectionDisposition::Failed;
                                        node.error = Some(format!("rigid model: {error:#}"));
                                    } else {
                                        report.rigid_render_objects += 1;
                                        *report
                                            .loaded_entity_resource_classes
                                            .entry(class)
                                            .or_default() += 1;
                                        loaded_entity_visual = true;
                                    }
                                }
                                RESPAWN_POINTS_COMPONENT => {
                                    match collect_shadowkeep_respawn_points(
                                        &mut report,
                                        &mut world,
                                        &mut inspection,
                                        resource_id,
                                        resource_tag,
                                        resource.definition.offset,
                                        table_sources.clone(),
                                    ) {
                                        Ok(()) => {
                                            *report
                                                .loaded_entity_resource_classes
                                                .entry(class)
                                                .or_default() += 1;
                                        }
                                        Err(error) => {
                                            report.skipped_resources += 1;
                                            *report
                                                .failed_entity_resource_classes
                                                .entry(class)
                                                .or_default() += 1;
                                            let node = inspection.node_mut(resource_id);
                                            node.disposition = MapInspectionDisposition::Failed;
                                            node.error = Some(format!("respawn points: {error:#}"));
                                            report.diagnostic(
                                                progress,
                                                MapLoadDiagnostic {
                                                    table: table_hash,
                                                    entry_offset: resource.definition.offset,
                                                    resource_class: class,
                                                    error: format!(
                                                        "entity {} resource {}: respawn points: \
                                                         {error:#}",
                                                        entry.entity, resource_tag,
                                                    ),
                                                },
                                            );
                                        }
                                    }
                                }
                                MAP_NODE_TABLE_COMPONENT => {
                                    match collect_shadowkeep_map_node_respawns(
                                        &mut report,
                                        &mut world,
                                        &mut inspection,
                                        resource_id,
                                        &mut loaded_node_tables,
                                        resource_tag,
                                        resource.definition.offset,
                                        table_sources.clone(),
                                    ) {
                                        Ok(()) => {
                                            *report
                                                .loaded_entity_resource_classes
                                                .entry(class)
                                                .or_default() += 1;
                                        }
                                        Err(error) => {
                                            report.skipped_resources += 1;
                                            *report
                                                .failed_entity_resource_classes
                                                .entry(class)
                                                .or_default() += 1;
                                            let node = inspection.node_mut(resource_id);
                                            node.disposition = MapInspectionDisposition::Failed;
                                            node.error =
                                                Some(format!("map-node respawn points: {error:#}"));
                                            report.diagnostic(
                                                progress,
                                                MapLoadDiagnostic {
                                                    table: table_hash,
                                                    entry_offset: resource.definition.offset,
                                                    resource_class: class,
                                                    error: format!(
                                                        "entity {} resource {}: map-node respawn \
                                                         points: {error:#}",
                                                        entry.entity, resource_tag,
                                                    ),
                                                },
                                            );
                                        }
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
                        let node = inspection.node_mut(entry_id);
                        node.disposition = MapInspectionDisposition::Failed;
                        node.error = Some(format!("entity decode: {error:#}"));
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
            let table_semantic_id =
                table_semantic_id.expect("valid table resource allocated before entity resources");

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
                            node: table_semantic_id,
                            collection: collection_tag,
                            table: table_hash,
                            entry_offset: entry.data_resource.offset,
                            sources: table_sources.clone(),
                            entity: entry.entity,
                            world_id: entry.world_id,
                            entry_translation: entry.translation.xyz(),
                        });
                        tracing::info!(
                            table = %table_hash,
                            origin = table_sources.label(),
                            base_containers = ?table_sources.base_containers,
                            scenario_referenced = table_sources.referenced_by_freeroam_scenario,
                            collection = %collection_tag,
                            world_id = entry.world_id,
                            entity = %entry.entity,
                            "discovered Shadowkeep sky-object collection"
                        );
                        Ok(())
                    })();
                    if let Err(error) = result {
                        report.skipped_resources += 1;
                        let diagnostic = format!("sky-object placement: {error:#}");
                        inspection.node_mut(table_semantic_id).disposition =
                            MapInspectionDisposition::Failed;
                        inspection.node_mut(table_semantic_id).error = Some(diagnostic.clone());
                        report.diagnostic(
                            progress,
                            MapLoadDiagnostic {
                                table: table_hash,
                                entry_offset: entry.data_resource.offset,
                                resource_class: SKY_OBJECT_PLACEMENT,
                                error: diagnostic,
                            },
                        );
                    }
                }
                ATMOSPHERE_PLACEMENT => {
                    report.atmosphere_placements += 1;
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
                        atmosphere_candidates.push(AtmospherePlacementCandidate {
                            node: table_semantic_id,
                            table: table_hash,
                            entry_offset: entry.data_resource.offset,
                            sources: table_sources.clone(),
                            world_id: entry.world_id,
                            lookup_volume_0: placement.lookup_volume_0,
                            lookup_volume_1: placement.lookup_volume_1,
                            lookup_vertical: placement.lookup_vertical,
                            lookup_table: placement.lookup_table,
                            lookup_parameters: placement.lookup_parameters,
                        });
                        Ok(())
                    })();
                    if let Err(error) = result {
                        report.skipped_resources += 1;
                        let diagnostic = format!("atmosphere placement: {error:#}");
                        inspection.node_mut(table_semantic_id).disposition =
                            MapInspectionDisposition::Failed;
                        inspection.node_mut(table_semantic_id).error = Some(diagnostic.clone());
                        report.diagnostic(
                            progress,
                            MapLoadDiagnostic {
                                table: table_hash,
                                entry_offset: entry.data_resource.offset,
                                resource_class: ATMOSPHERE_PLACEMENT,
                                error: diagnostic,
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
                        inspection.node_mut(table_semantic_id).bounds = Some(transform_bounds(
                            AxisAlignedBBox::from_center_extents(Vec3::ZERO, Vec3::ONE),
                            component.unkb0,
                        ));
                        let entity = world.spawn((
                            Transform::new(volume_translation, volume_rotation, volume_scale),
                            DynamicRenderObject::new(renderer.add_object(RenderObject::new(
                                TfxFeatureRenderer::Cubemaps,
                                Box::new(cubemap),
                            ))),
                            MapInspectionBinding {
                                node: table_semantic_id,
                            },
                            MapEntityVisibility::default(),
                        ));
                        inspection.bind_world_entity(table_semantic_id, entity)?;
                        progress
                            .visual_resources_loaded
                            .fetch_add(1, Ordering::Relaxed);
                        report.cubemap_render_objects += 1;
                        Ok(())
                    })();
                    if let Err(error) = result {
                        report.skipped_resources += 1;
                        let diagnostic = format!("cubemap volume: {error:#}");
                        inspection.node_mut(table_semantic_id).disposition =
                            MapInspectionDisposition::Failed;
                        inspection.node_mut(table_semantic_id).error = Some(diagnostic.clone());
                        report.diagnostic(
                            progress,
                            MapLoadDiagnostic {
                                table: table_hash,
                                entry_offset: entry.data_resource.offset,
                                resource_class: CUBEMAP_VOLUME,
                                error: diagnostic,
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
                            let world_transform = Transform::new(
                                transform.translation.xyz(),
                                transform.rotation,
                                Vec3::ONE,
                            );
                            let mut light_node = MapInspectionNode::new(
                                MapInspectionNodeKind::Light,
                                MapInspectionDisposition::Rendering,
                                format!("Light {index}"),
                                table_sources.clone(),
                            );
                            light_node.element_index = Some(index);
                            light_node.transform = Some(world_transform);
                            let light_node_id =
                                inspection.add_node(Some(table_semantic_id), light_node);
                            let entity = world.spawn((
                                world_transform,
                                DynamicRenderObject::new(renderer.add_object(RenderObject::new(
                                    TfxFeatureRenderer::DeferredLights,
                                    renderer_object,
                                ))),
                                MapInspectionBinding {
                                    node: light_node_id,
                                },
                                MapEntityVisibility::default(),
                            ));
                            inspection.bind_world_entity(light_node_id, entity)?;
                            report.light_render_objects += 1;
                        }
                        Ok(())
                    })();
                    if let Err(error) = result {
                        tracing::warn!(table = %table_hash, class = %format_args!("{LIGHT_COLLECTION:08X}"), error = %format_args!("{error:#}"), "Shadowkeep light collection skipped");
                        report.skipped_resources += 1;
                        let diagnostic = format!("light collection: {error:#}");
                        inspection.node_mut(table_semantic_id).disposition =
                            MapInspectionDisposition::Failed;
                        inspection.node_mut(table_semantic_id).error = Some(diagnostic.clone());
                        report.diagnostic(
                            progress,
                            MapLoadDiagnostic {
                                table: table_hash,
                                entry_offset: entry.data_resource.offset,
                                resource_class: LIGHT_COLLECTION,
                                error: diagnostic,
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
                        let light_transform = Transform::new(
                            entry.translation.xyz(),
                            entry.rotation,
                            Vec3::splat(entry.translation.w),
                        );
                        let fov_degrees = (light.half_fov * 2.0).to_degrees();
                        anyhow::ensure!(
                            fov_degrees.is_finite() && fov_degrees > 0.0 && fov_degrees < 180.0,
                            "invalid local shadow FOV {fov_degrees}"
                        );
                        anyhow::ensure!(
                            light.far_plane.is_finite() && light.far_plane > 0.5,
                            "invalid local shadow far plane {}",
                            light.far_plane
                        );
                        let shadowmap =
                            ShadowMap::create(light_transform, fov_degrees, 0.5, light.far_plane);
                        let mut shadow_view = View::new_shadow(
                            format!("shadow_{light_tag}"),
                            &renderer.gpu,
                            (1024, 1024),
                        )?;
                        shadow_view.disable_culling = true;
                        let ViewKind::Shadow(shadow_view_data) = &shadow_view.kind else {
                            anyhow::bail!("local shadow view has unexpected kind");
                        };
                        let shadow_texture = shadow_view_data.shadow_map.texture.clone();
                        let shadow_srv = shadow_view_data
                            .shadow_map
                            .srv(0)
                            .context("local shadow map has no shader resource view")?
                            .clone();
                        let mut renderer_object = LightRenderer::new_shadowkeep_shadowing(
                            renderer,
                            light.technique_shading,
                            light.technique_shading_shadowing,
                            light.technique_volumetrics,
                            light.technique_volumetrics_shadowing,
                            light.light_to_world,
                            shadowmap.camera_to_projective,
                        )?;
                        renderer_object.shadow_view = Some((shadow_texture, shadow_srv));
                        let entity = world.spawn((
                            light_transform,
                            DynamicRenderObject::new(renderer.add_object(RenderObject::new(
                                TfxFeatureRenderer::DeferredLights,
                                renderer_object,
                            ))),
                            shadowmap,
                            shadow_view,
                            MapInspectionBinding {
                                node: table_semantic_id,
                            },
                            MapEntityVisibility::default(),
                        ));
                        inspection.bind_world_entity(table_semantic_id, entity)?;
                        Ok(())
                    })();
                    if let Err(error) = result {
                        tracing::warn!(table = %table_hash, class = %format_args!("{SHADOWING_LIGHT:08X}"), error = %format_args!("{error:#}"), "Shadowkeep shadowing light skipped");
                        report.skipped_resources += 1;
                        let diagnostic = format!("shadowing light: {error:#}");
                        inspection.node_mut(table_semantic_id).disposition =
                            MapInspectionDisposition::Failed;
                        inspection.node_mut(table_semantic_id).error = Some(diagnostic.clone());
                        report.diagnostic(
                            progress,
                            MapLoadDiagnostic {
                                table: table_hash,
                                entry_offset: entry.data_resource.offset,
                                resource_class: SHADOWING_LIGHT,
                                error: diagnostic,
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
                        if let Some(canonical) = loaded_static_collections.get(&instances_hash) {
                            let node = inspection.node_mut(table_semantic_id);
                            node.disposition = MapInspectionDisposition::NonRendering;
                            node.linked_node = Some(*canonical);
                            node.error = Some(format!(
                                "deduplicated; rendered by inspection node {}",
                                canonical.0
                            ));
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
                        inspection.node_mut(table_semantic_id).bounds = Some(feature_bounds);
                        let entity = world.spawn((
                            StaticRenderObject::new(renderer.add_object(RenderObject::new(
                                TfxFeatureRenderer::ChunkedInstanceObjects,
                                Box::new(feature),
                            ))),
                            MapInspectionBinding {
                                node: table_semantic_id,
                            },
                            MapEntityVisibility::default(),
                        ));
                        inspection.bind_world_entity(table_semantic_id, entity)?;
                        loaded_static_collections.insert(instances_hash, table_semantic_id);
                        progress
                            .visual_resources_loaded
                            .fetch_add(1, Ordering::Relaxed);
                        report.static_render_objects += 1;
                        Ok(())
                    })();
                    if let Err(error) = result {
                        report.skipped_resources += 1;
                        let diagnostic = format!("static placement: {error:#}");
                        inspection.node_mut(table_semantic_id).disposition =
                            MapInspectionDisposition::Failed;
                        inspection.node_mut(table_semantic_id).error = Some(diagnostic.clone());
                        report.diagnostic(
                            progress,
                            MapLoadDiagnostic {
                                table: table_hash,
                                entry_offset: entry.data_resource.offset,
                                resource_class: STATIC_PLACEMENT,
                                error: diagnostic,
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
                        if let Some(canonical) = loaded_terrain_resources.get(&placement.terrain) {
                            let node = inspection.node_mut(table_semantic_id);
                            node.disposition = MapInspectionDisposition::NonRendering;
                            node.linked_node = Some(*canonical);
                            node.error = Some(format!(
                                "deduplicated; rendered by inspection node {}",
                                canonical.0
                            ));
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
                        let entity = world.spawn((
                            StaticRenderObject::new(renderer.add_object(RenderObject::new(
                                TfxFeatureRenderer::TerrainPatch,
                                terrain,
                            ))),
                            MapInspectionBinding {
                                node: table_semantic_id,
                            },
                            MapEntityVisibility::default(),
                        ));
                        inspection.bind_world_entity(table_semantic_id, entity)?;
                        loaded_terrain_resources.insert(placement.terrain, table_semantic_id);
                        progress
                            .visual_resources_loaded
                            .fetch_add(1, Ordering::Relaxed);
                        report.terrain_render_objects += 1;
                        Ok(())
                    })();
                    if let Err(error) = result {
                        report.skipped_resources += 1;
                        let diagnostic = format!("terrain placement: {error:#}");
                        inspection.node_mut(table_semantic_id).disposition =
                            MapInspectionDisposition::Failed;
                        inspection.node_mut(table_semantic_id).error = Some(diagnostic.clone());
                        report.diagnostic(
                            progress,
                            MapLoadDiagnostic {
                                table: table_hash,
                                entry_offset: entry.data_resource.offset,
                                resource_class: TERRAIN_PLACEMENT,
                                error: diagnostic,
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
    let mut candidates_by_collection = BTreeMap::<TagHash, SkyObjectCollectionEvidence>::new();
    for candidate in sky_object_candidates {
        candidates_by_collection
            .entry(candidate.collection)
            .or_default()
            .placements
            .push(candidate);
    }
    report.sky_object_collections = candidates_by_collection.len();

    let mut decoded_sky_collections = BTreeMap::new();
    for (collection_tag, evidence) in &mut candidates_by_collection {
        match package_manager().read_tag_struct::<SShadowkeepSkyObjectCollection>(*collection_tag) {
            Ok(collection) => {
                evidence.object_count = collection.objects.len();
                decoded_sky_collections.insert(*collection_tag, collection);
            }
            Err(error) => {
                report.skipped_resources += 1;
                let candidate = evidence
                    .placements
                    .first()
                    .expect("every collection has at least one placement");
                report.diagnostic(
                    progress,
                    MapLoadDiagnostic {
                        table: candidate.table,
                        entry_offset: candidate.entry_offset,
                        resource_class: SKY_OBJECT_COLLECTION,
                        error: format!(
                            "sky-object collection {collection_tag}: could not decode: {error:#}"
                        ),
                    },
                );
            }
        }
    }

    let selector =
        ConVars::get::<u32>("render.shadowkeep_sky_object_collection").unwrap_or_default();
    let (map_package_name, _) = tag_package_metadata(tag);
    let collection_package_names = candidates_by_collection
        .keys()
        .filter_map(|collection| {
            tag_package_metadata(*collection)
                .0
                .map(|package| (*collection, package))
        })
        .collect();
    let selection = select_shadowkeep_environment(
        &candidates_by_collection,
        &atmosphere_candidates,
        selector,
        map_package_name.as_deref(),
        &collection_package_names,
    );
    report.deferred_sky_object_collections = selection.deferred_sky_collections.clone();
    report.environment_selection_reason = Some(format!("{:?}", selection.reason));
    report.selected_atmosphere_table = selection
        .atmosphere_index
        .map(|index| atmosphere_candidates[index].table);
    if let Some(error) = selection.reason.diagnostic() {
        report.diagnostic(
            progress,
            MapLoadDiagnostic {
                table: tag,
                entry_offset: 0,
                resource_class: SKY_OBJECT_PLACEMENT,
                error: error.to_owned(),
            },
        );
    }
    report.sky_object_collection_tags = selection.sky_collection.iter().copied().collect();
    let selected_atmosphere = selection.atmosphere_index;
    if let Some(index) = selected_atmosphere {
        let candidate = &atmosphere_candidates[index];
        match load_shadowkeep_atmosphere(renderer, candidate) {
            Ok(atmosphere) => {
                let entity = world.spawn((
                    atmosphere,
                    MapInspectionBinding {
                        node: candidate.node,
                    },
                    MapEntityVisibility::default(),
                ));
                inspection.bind_world_entity(candidate.node, entity)?;
                progress
                    .gpu_assets_requested
                    .fetch_add(4, Ordering::Relaxed);
                progress
                    .visual_resources_loaded
                    .fetch_add(1, Ordering::Relaxed);
            }
            Err(error) => {
                report.skipped_resources += 1;
                let diagnostic = format!("atmosphere placement: {error:#}");
                inspection.node_mut(candidate.node).disposition = MapInspectionDisposition::Failed;
                inspection.node_mut(candidate.node).error = Some(diagnostic.clone());
                report.diagnostic(
                    progress,
                    MapLoadDiagnostic {
                        table: candidate.table,
                        entry_offset: candidate.entry_offset,
                        resource_class: ATMOSPHERE_PLACEMENT,
                        error: diagnostic,
                    },
                );
            }
        }
    }
    if let Err(error) = write_environment_census(
        tag,
        report.scenario,
        &candidates_by_collection,
        &decoded_sky_collections,
        &atmosphere_candidates,
        &selection,
    ) {
        tracing::error!(error = ?error, "failed to write Shadowkeep environment census");
    }

    for (collection_order, collection_tag) in selection.sky_collection.into_iter().enumerate() {
        let collection_order = collection_order as u16;
        if progress.is_cancelled() {
            report.cancelled = true;
            break;
        }
        let evidence = &candidates_by_collection[&collection_tag];
        let candidate = evidence
            .placements
            .first()
            .expect("every collection has at least one placement");
        let Some(collection) = decoded_sky_collections.get(&collection_tag) else {
            continue;
        };
        let object_count = collection.objects.len();
        let occlusion_count = collection.occlusion_bounds.len();
        let identifier_count = collection.identifiers.len();
        let common_count = object_count.min(occlusion_count).min(identifier_count);
        report.sky_object_records += object_count;
        tracing::info!(
            collection = %collection_tag,
            origin = candidate.sources.label(),
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
            let mut sky_node = MapInspectionNode::new(
                MapInspectionNodeKind::SkyObject,
                if object.unk70 == 5 {
                    MapInspectionDisposition::NonRendering
                } else {
                    MapInspectionDisposition::Rendering
                },
                format!("Sky Object {index}"),
                candidate.sources.clone(),
            );
            sky_node.element_index = Some(index);
            sky_node.tag = object
                .model
                .entity_model
                .is_some()
                .then_some(object.model.entity_model);
            let sky_node_id = inspection.add_node(Some(candidate.node), sky_node);
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
                inspection.node_mut(sky_node_id).bounds =
                    Some(transform_bounds(object.bounds, matrix));
                let (scale, rotation, translation) = matrix.to_scale_rotation_translation();
                let transform = Transform::new(translation, rotation, scale);
                inspection.node_mut(sky_node_id).transform = Some(transform);
                let mut model = DynamicModel::load_shadowkeep(model_tag, Vec::new(), Vec::new())?;
                model.set_sky_owner(tag, collection_tag);
                let (model_center, model_radius) = model.model.bounding_sphere();
                let mut render_object =
                    RenderObject::new(TfxFeatureRenderer::SkyTransparent, model);
                let authored_stages = render_object.stages;
                report
                    .sky_object_stage_subscriptions
                    .push((model_tag, authored_stages.bits()));
                let color_stages = authored_stages
                    & (RenderStageSubscription::DECALS_ADDITIVE
                        | RenderStageSubscription::TRANSPARENTS);
                let deferred_stages =
                    authored_stages & RenderStageSubscription::LIGHT_SHAFT_OCCLUSION;
                let unsupported_stages = authored_stages & !(color_stages | deferred_stages);
                report.sky_object_color_stage_mask |= color_stages.bits();
                report.sky_object_deferred_stage_mask |= deferred_stages.bits();
                if !deferred_stages.is_empty() {
                    tracing::info!(
                        collection = %collection_tag,
                        model = %model_tag,
                        deferred_stage_mask = format_args!("0x{:08X}", deferred_stages.bits()),
                        "deferred Shadowkeep sky-object stages whose legacy target is not restored"
                    );
                }
                if !unsupported_stages.is_empty() {
                    tracing::warn!(
                        collection = %collection_tag,
                        model = %model_tag,
                        authored_stage_mask = format_args!("0x{:08X}", authored_stages.bits()),
                        unsupported_stage_mask = format_args!("0x{:08X}", unsupported_stages.bits()),
                        "removed unsupported stages from Shadowkeep sky object"
                    );
                    report.diagnostic(
                        progress,
                        MapLoadDiagnostic {
                            table: candidate.table,
                            entry_offset: candidate.entry_offset,
                            resource_class: SKY_OBJECT_COLLECTION,
                            error: format!(
                                "sky-object model {model_tag} subscribed to unsupported stage \
                                 mask 0x{:08X}; retained color stages and deferred \
                                 LightShaftOcclusion",
                                unsupported_stages.bits(),
                            ),
                        },
                    );
                }
                render_object.stages = color_stages;
                anyhow::ensure!(
                    !render_object.stages.is_empty(),
                    "sky-object model subscribes to no DecalsAdditive or Transparents stage"
                );
                let admitted_stage_mask = render_object.stages.bits();
                let entity = world.spawn((
                    transform,
                    ShadowkeepSkyOrder {
                        collection_order,
                        object_index: u16::try_from(index)
                            .context("sky-object record index exceeds authored order range")?,
                    },
                    DynamicRenderObject::new(renderer.add_object(render_object)),
                    MapInspectionBinding { node: sky_node_id },
                    MapEntityVisibility::default(),
                ));
                inspection.bind_world_entity(sky_node_id, entity)?;
                report.sky_object_render_objects += 1;
                progress
                    .gpu_assets_requested
                    .fetch_add(1, Ordering::Relaxed);
                progress
                    .visual_resources_loaded
                    .fetch_add(1, Ordering::Relaxed);
                tracing::info!(
                    collection = %collection_tag,
                    origin = candidate.sources.label(),
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
                    light_shaft_occlusion_deferred = deferred_stages
                        .is_subscribed(RenderStage::LightShaftOcclusion),
                    admitted_stage_mask = format_args!("0x{admitted_stage_mask:08X}"),
                    "loaded Shadowkeep sky object"
                );
                Ok(())
            })();
            if let Err(error) = result {
                report.skipped_resources += 1;
                let diagnostic = format!(
                    "sky-object collection {collection_tag} record {index} model {model_tag}: \
                     {error:#}"
                );
                inspection.node_mut(sky_node_id).disposition = MapInspectionDisposition::Failed;
                inspection.node_mut(sky_node_id).error = Some(diagnostic.clone());
                report.diagnostic(
                    progress,
                    MapLoadDiagnostic {
                        table: candidate.table,
                        entry_offset: candidate.entry_offset,
                        resource_class: SKY_OBJECT_COLLECTION,
                        error: diagnostic,
                    },
                );
            }
        }
    }
    tracing::info!(
        map = %tag,
        reason = ?report.environment_selection_reason,
        selected_sky_collection = ?report.sky_object_collection_tags,
        selected_atmosphere_table = ?report.selected_atmosphere_table,
        sky_object_render_objects = report.sky_object_render_objects,
        "completed Shadowkeep environment selection"
    );
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
    report.inspection = inspection.finalize();
    report.inspection.validate(&world)?;
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
        environment_selection_reason = ?report.environment_selection_reason,
        selected_atmosphere_table = ?report.selected_atmosphere_table,
        sky_collection_package_names = ?collection_package_names,
        table_sources = ?report.table_sources,
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
    if let Err(error) = write_shadowkeep_production_feature_matrix(&report) {
        tracing::error!(error = ?error, "failed to write Shadowkeep production feature matrix");
    }
    Ok((world, report))
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};

    use alkahest_data::map::SRespawnPointsComponent;
    use glam::{Vec3, Vec4};
    use tiger_parse::TigerReadable;
    use tiger_pkg::TagHash;

    use super::{
        AtmospherePlacementCandidate, EntityResourceExample, MapInspectionNodeId, MapLoadProgress,
        MapLoadReport, RESPAWN_POINTS_COMPONENT, ShadowkeepBubbleCatalogEntry,
        ShadowkeepEnvironmentSelectionReason, ShadowkeepTableSources, SkyObjectCollectionEvidence,
        SkyObjectPlacementCandidate, bounded_offset, select_shadowkeep_environment,
        shadowkeep_bubble_catalog_matches,
    };

    fn collection_candidate(
        collection: TagHash,
        object_count: usize,
        base_container: Option<TagHash>,
        scenario: bool,
    ) -> SkyObjectCollectionEvidence {
        let mut base_containers = BTreeSet::new();
        if let Some(container) = base_container {
            base_containers.insert(container);
        }
        SkyObjectCollectionEvidence {
            placements: vec![SkyObjectPlacementCandidate {
                node: MapInspectionNodeId(0),
                collection,
                table: TagHash(0x8080_1000),
                entry_offset: 0x20,
                sources: ShadowkeepTableSources {
                    base_containers,
                    referenced_by_freeroam_scenario: scenario,
                    scenario: None,
                },
                entity: TagHash::NONE,
                world_id: 7,
                entry_translation: Vec3::ZERO,
            }],
            object_count,
        }
    }

    fn atmosphere_candidate(
        table: TagHash,
        world_id: u64,
        sources: ShadowkeepTableSources,
    ) -> AtmospherePlacementCandidate {
        AtmospherePlacementCandidate {
            node: MapInspectionNodeId(0),
            table,
            entry_offset: 0x40,
            sources,
            world_id,
            lookup_volume_0: TagHash::NONE,
            lookup_volume_1: TagHash::NONE,
            lookup_vertical: TagHash::NONE,
            lookup_table: TagHash::NONE,
            lookup_parameters: [Vec4::ZERO; 4],
        }
    }

    #[test]
    fn bounded_resource_offsets_reject_overflow_and_truncation() {
        assert_eq!(bounded_offset(0x40, 0x20, 0x10).unwrap(), 0x20);
        assert!(bounded_offset(0x40, 0x31, 0x10).is_err());
        assert!(bounded_offset(0x40, u64::MAX, 1).is_err());
    }

    #[test]
    fn respawn_dispatch_class_is_distinct_from_payload_type() {
        assert_eq!(RESPAWN_POINTS_COMPONENT, 0x8080_8CB3);
        assert_eq!(SRespawnPointsComponent::ID, Some(0x8080_8CB5));
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
    #[test]
    fn explicit_sky_override_selects_one_complete_environment() {
        let selected = TagHash(0x8080_2002);
        let candidates = BTreeMap::from([(
            selected,
            collection_candidate(selected, 3, Some(TagHash(0x8080_3002)), false),
        )]);
        let atmospheres = vec![atmosphere_candidate(
            TagHash(0x8080_1000),
            7,
            ShadowkeepTableSources {
                base_containers: BTreeSet::from([TagHash(0x8080_3002)]),
                referenced_by_freeroam_scenario: false,
                scenario: None,
            },
        )];

        let result = select_shadowkeep_environment(
            &candidates,
            &atmospheres,
            selected.0,
            None,
            &BTreeMap::new(),
        );

        assert_eq!(result.sky_collection, Some(selected));
        assert_eq!(result.atmosphere_index, Some(0));
        assert_eq!(
            result.reason,
            ShadowkeepEnvironmentSelectionReason::ExplicitSkyOverride
        );
    }

    #[test]
    fn same_source_table_beats_destination_package() {
        let shared_table = TagHash(0x8080_1000);
        let matching = TagHash(0x8080_2001);
        let destination = TagHash(0x8080_2002);
        let mut destination_candidate =
            collection_candidate(destination, 3, Some(TagHash(0x8080_3002)), false);
        destination_candidate.placements[0].table = TagHash(0x8080_1001);
        let candidates = BTreeMap::from([
            (
                matching,
                collection_candidate(matching, 2, Some(TagHash(0x8080_3001)), false),
            ),
            (destination, destination_candidate),
        ]);
        let atmospheres = vec![atmosphere_candidate(
            shared_table,
            7,
            ShadowkeepTableSources {
                base_containers: BTreeSet::from([TagHash(0x8080_3001)]),
                referenced_by_freeroam_scenario: false,
                scenario: None,
            },
        )];
        let packages = BTreeMap::from([(destination, "edz".to_owned())]);

        let result =
            select_shadowkeep_environment(&candidates, &atmospheres, 0, Some("edz"), &packages);

        assert_eq!(result.sky_collection, Some(matching));
        assert_eq!(result.atmosphere_index, Some(0));
        assert_eq!(
            result.reason,
            ShadowkeepEnvironmentSelectionReason::SharedSourceTable
        );
    }

    #[test]
    fn sentinel_empty_collection_never_makes_environment_ambiguous() {
        let empty = TagHash(0x80C7_54AB);
        let non_empty = TagHash(0x8080_2002);
        let candidates = BTreeMap::from([
            (
                empty,
                collection_candidate(empty, 0, Some(TagHash(0x8080_3001)), false),
            ),
            (
                non_empty,
                collection_candidate(non_empty, 3, Some(TagHash(0x8080_3002)), false),
            ),
        ]);
        let atmospheres = vec![atmosphere_candidate(
            TagHash(0x8080_1000),
            7,
            ShadowkeepTableSources {
                base_containers: BTreeSet::from([TagHash(0x8080_3002)]),
                referenced_by_freeroam_scenario: false,
                scenario: None,
            },
        )];

        let result =
            select_shadowkeep_environment(&candidates, &atmospheres, 0, None, &BTreeMap::new());

        assert_eq!(result.sky_collection, Some(non_empty));
        assert_eq!(result.atmosphere_index, Some(0));
        assert_eq!(result.deferred_sky_collections, vec![empty]);
    }

    #[test]
    fn destination_package_needs_exactly_one_compatible_atmosphere() {
        let common = TagHash(0x8080_2001);
        let destination = TagHash(0x8080_2002);
        let source = ShadowkeepTableSources {
            base_containers: BTreeSet::from([TagHash(0x8080_3002)]),
            referenced_by_freeroam_scenario: false,
            scenario: None,
        };
        let candidates = BTreeMap::from([
            (
                common,
                collection_candidate(common, 2, Some(TagHash(0x8080_3001)), true),
            ),
            (
                destination,
                collection_candidate(destination, 3, Some(TagHash(0x8080_3002)), false),
            ),
        ]);
        let atmospheres = vec![
            atmosphere_candidate(TagHash(0x8080_4001), 0, ShadowkeepTableSources::default()),
            atmosphere_candidate(TagHash(0x8080_4002), 0, source),
        ];
        let packages = BTreeMap::from([(destination, "edz".to_owned())]);

        let result =
            select_shadowkeep_environment(&candidates, &atmospheres, 0, Some("edz"), &packages);

        assert_eq!(result.sky_collection, Some(destination));
        assert_eq!(result.atmosphere_index, Some(1));
        assert_eq!(
            result.reason,
            ShadowkeepEnvironmentSelectionReason::DestinationPackage
        );
    }

    #[test]
    fn destination_package_uses_the_only_atmosphere_when_sources_do_not_overlap() {
        let common = TagHash(0x8080_2001);
        let destination = TagHash(0x8080_2002);
        let candidates = BTreeMap::from([
            (
                common,
                collection_candidate(common, 2, Some(TagHash(0x8080_3001)), false),
            ),
            (
                destination,
                collection_candidate(destination, 3, Some(TagHash(0x8080_3002)), false),
            ),
        ]);
        let atmospheres = vec![atmosphere_candidate(
            TagHash(0x8080_4001),
            0,
            ShadowkeepTableSources::default(),
        )];
        let packages = BTreeMap::from([(destination, "edz".to_owned())]);

        let result =
            select_shadowkeep_environment(&candidates, &atmospheres, 0, Some("edz"), &packages);

        assert_eq!(result.sky_collection, Some(destination));
        assert_eq!(result.atmosphere_index, Some(0));
        assert_eq!(
            result.reason,
            ShadowkeepEnvironmentSelectionReason::DestinationPackage
        );
    }

    #[test]
    fn unresolved_environment_selects_neither_resource_family() {
        let first = TagHash(0x8080_2001);
        let second = TagHash(0x8080_2002);
        let candidates = BTreeMap::from([
            (
                first,
                collection_candidate(first, 2, Some(TagHash(0x8080_3001)), false),
            ),
            (
                second,
                collection_candidate(second, 3, Some(TagHash(0x8080_3002)), true),
            ),
        ]);
        let atmospheres = vec![
            atmosphere_candidate(TagHash(0x8080_4001), 0, ShadowkeepTableSources::default()),
            atmosphere_candidate(TagHash(0x8080_4002), 0, ShadowkeepTableSources::default()),
        ];

        let result =
            select_shadowkeep_environment(&candidates, &atmospheres, 0, None, &BTreeMap::new());

        assert_eq!(result.sky_collection, None);
        assert_eq!(result.atmosphere_index, None);
        assert_eq!(
            result.reason,
            ShadowkeepEnvironmentSelectionReason::Ambiguous
        );
        assert_eq!(result.deferred_sky_collections, vec![first, second]);
        assert_eq!(result.deferred_atmospheres, vec![0, 1]);
    }

    #[test]
    fn catalog_search_matches_resolved_names_tags_and_packages_case_insensitively() {
        let entry = ShadowkeepBubbleCatalogEntry {
            tag: TagHash(0x8080_1234),
            child_map: None,
            package_name: "sandbox_01".to_owned(),
            package_path: "sandbox_01.pkg".to_owned(),
            map_name_hash: None,
            display_name: "European Dead Zone".to_owned(),
            search_text: "european dead zone 80801234 sandbox_01".to_owned(),
            container_count: 0,
            table_count: 0,
            scenario: None,
            readable: true,
            error: None,
        };

        assert!(shadowkeep_bubble_catalog_matches(&entry, ""));
        assert!(shadowkeep_bubble_catalog_matches(&entry, "DEAD ZONE"));
        assert!(shadowkeep_bubble_catalog_matches(&entry, "sandbox_01"));
        assert!(!shadowkeep_bubble_catalog_matches(&entry, "europa"));
    }
}

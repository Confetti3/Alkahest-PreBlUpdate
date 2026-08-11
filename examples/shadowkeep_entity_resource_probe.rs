//! Read-only structural dossier for Shadowkeep entity-resource classes.
//!
//! Example:
//! `cargo run --example shadowkeep_entity_resource_probe -- --packages <packages> \
//!  --map 81547A30 --map 80ED6027 --class 808084D7 --max-examples 32 \
//!  --output artifacts/shadowkeep-entity-class-808084D7/`

use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    fs,
    io::{Cursor, Seek, SeekFrom},
    path::{Path, PathBuf},
    str::FromStr,
};

use alkahest_data::shadowkeep::{
    SShadowkeepBubbleDefinition, SShadowkeepBubbleParent, SShadowkeepEntity,
    SShadowkeepEntityResource, SShadowkeepMapDataTable, SShadowkeepRigidModelComponent,
};
use anyhow::{Context, Result, bail};
use clap::Parser;
use glam::{Mat4, Vec4, Vec4Swizzles};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tiger_parse::{PackageManagerExt, TigerReadable};
use tiger_pkg::{TagHash, package_manager};

const RIGID_MODEL_COMPONENT: u32 = 0x8080_72B8;
const DEFINITION_PREFIX_BYTES: usize = 0x40;
const DEFINITION_DUMP_BYTES: usize = 0x200;
const SCENARIO_CLASS: u32 = 0x8080_9994;
const PAIRED_CANDIDATE_CLASSES: [u32; 2] = [0x8080_6730, 0x80C7_0EDC];

#[derive(Debug, Parser)]
struct Args {
    /// Shadowkeep package directory.
    #[arg(long)]
    packages: PathBuf,
    /// Bubble-parent map tag. May be repeated.
    #[arg(long = "map", required = true)]
    maps: Vec<String>,
    /// Entity resource class in hexadecimal.
    #[arg(long = "class")]
    class: String,
    /// Maximum detailed examples retained across all maps.
    #[arg(long, default_value_t = 32)]
    max_examples: usize,
    /// Dossier output directory.
    #[arg(long)]
    output: PathBuf,
    /// Optional frozen camera/depth manifest. May be repeated.
    #[arg(long = "capture")]
    captures: Vec<PathBuf>,
}

#[derive(Debug, Clone, Serialize)]
struct PackageRecord {
    tag: String,
    reference: String,
    file_type: u8,
    file_subtype: u8,
    declared_size: u32,
    byte_length: Option<usize>,
}

#[derive(Debug, Clone, Serialize)]
struct PointerRecord {
    valid: bool,
    resource_type: String,
    offset: u64,
}

#[derive(Debug, Clone, Serialize)]
struct NestedPointerRecord {
    valid: bool,
    resource_type: String,
    class_type: String,
    parent_tag: String,
    offset: u64,
}

#[derive(Debug, Clone, Serialize)]
struct ChildTagRecord {
    definition_offset: usize,
    package: PackageRecord,
    context_hex_16_before_after: String,
}

#[derive(Debug, Clone, Serialize)]
struct PlacementRecord {
    translation: [f32; 3],
    rotation: [f32; 4],
    scale: [f32; 3],
}

#[derive(Debug, Clone, Serialize)]
struct GapEvidence {
    capture_available: bool,
    in_front_of_camera: Option<bool>,
    inside_viewport: Option<bool>,
    screen_pixel: Option<[f32; 2]>,
    projected_distance: Option<f32>,
    clear_depth_percentage: Option<f64>,
    nearest_non_clear_depth: Option<f32>,
    note: String,
}

impl GapEvidence {
    fn unavailable() -> Self {
        Self {
            capture_available: false,
            in_front_of_camera: None,
            inside_viewport: None,
            screen_pixel: None,
            projected_distance: None,
            clear_depth_percentage: None,
            nearest_non_clear_depth: None,
            note: "No matching frozen camera/depth capture was supplied; no spatial semantic \
                   claim was made."
                .into(),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
struct ExampleRecord {
    map: String,
    scenario: Option<String>,
    table_origin: Vec<String>,
    table: String,
    entity: String,
    entity_resource: PackageRecord,
    resource_pointer: PointerRecord,
    definition_pointer: PointerRecord,
    placement: PlacementRecord,
    sibling_entity_resource_classes: Vec<String>,
    has_loaded_rigid_component: bool,
    nested_resource_table: Vec<NestedPointerRecord>,
    unk80: String,
    unk80_package: Option<PackageRecord>,
    unk84: String,
    unk84_package: Option<PackageRecord>,
    definition_tag_values: Vec<ChildTagRecord>,
    likely_definition_span_length: usize,
    definition_sha256: String,
    definition_prefix_sha256: String,
    definition_hex_0x200: String,
    gap_evidence: GapEvidence,
}

#[derive(Debug, Clone)]
struct DefinitionAnalysis {
    span_length: usize,
    sha256: String,
    prefix_sha256: String,
    hex_dump: String,
    child_tags: Vec<ChildTagRecord>,
    child_class_signature: Vec<String>,
}

#[derive(Debug, Default)]
struct ClassStats {
    occurrences: usize,
    valid_definitions: usize,
    invalid_definitions: usize,
    entities: BTreeSet<String>,
    maps: BTreeSet<String>,
    on_entities_with_rigid: usize,
    on_entities_without_loaded_rigid_component: usize,
    capture_unavailable: usize,
    projected_over_mostly_clear_depth: usize,
    projected_over_existing_geometry: usize,
    off_screen: usize,
    complete_hash_counts: BTreeMap<String, usize>,
    definition_lengths: BTreeMap<usize, usize>,
    child_class_counts: BTreeMap<String, usize>,
    nested_signatures: BTreeMap<String, usize>,
    sibling_signatures: BTreeMap<String, usize>,
    reference_clusters: BTreeMap<usize, ReferenceClusterAccumulator>,
}

#[derive(Debug, Clone, Serialize)]
struct DefinitionCluster {
    entity_resource_package_class: String,
    definition_length: usize,
    definition_prefix_sha256: String,
    definition_sha256: String,
    nested_resource_table_signature: Vec<String>,
    resolved_child_class_signature: Vec<String>,
    sibling_entity_resource_class_signature: Vec<String>,
    classification: &'static str,
    count: usize,
    maps: BTreeMap<String, usize>,
    representative_entity_resource: String,
}

#[derive(Debug, Default)]
struct ClusterAccumulator {
    count: usize,
    maps: BTreeMap<String, usize>,
    representative: String,
}

#[derive(Debug, Default)]
struct ReferenceClusterAccumulator {
    occurrences: usize,
    references: BTreeMap<(usize, String), StableReferenceAccumulator>,
}

#[derive(Debug, Default)]
struct StableReferenceAccumulator {
    occurrences_at_offset: usize,
    tags: BTreeSet<String>,
    contexts: BTreeSet<String>,
}

#[derive(Debug, Clone, Serialize)]
struct StableReferenceOffset {
    definition_length: usize,
    cluster: String,
    offset: usize,
    package_class: String,
    cluster_occurrences: usize,
    occurrences_at_offset: usize,
    coverage_percentage: f64,
    distinct_tags: usize,
    example_tags: Vec<String>,
    surrounding_bytes: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
struct RigidBaseline {
    occurrences: usize,
    parsed_definitions: usize,
    proven_struct_size: usize,
    definition_lengths: BTreeMap<usize, usize>,
    model_package_classes: BTreeMap<String, usize>,
    technique_package_classes: BTreeMap<String, usize>,
    material_variant_record_counts: BTreeMap<usize, usize>,
    technique_reference_counts: BTreeMap<usize, usize>,
    child_tag_reference_pattern: BTreeMap<String, usize>,
}

impl Default for RigidBaseline {
    fn default() -> Self {
        Self {
            occurrences: 0,
            proven_struct_size: 0x320,
            parsed_definitions: 0,
            definition_lengths: BTreeMap::new(),
            model_package_classes: BTreeMap::new(),
            technique_package_classes: BTreeMap::new(),
            material_variant_record_counts: BTreeMap::new(),
            technique_reference_counts: BTreeMap::new(),
            child_tag_reference_pattern: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Serialize)]
struct CooccurrenceReport {
    target_class: String,
    occurrences: usize,
    on_entities_with_loaded_rigid_component: usize,
    on_entities_without_loaded_rigid_component: usize,
    sibling_class_counts: BTreeMap<String, usize>,
    sibling_signatures: BTreeMap<String, usize>,
    projected_over_mostly_clear_depth: usize,
    projected_over_existing_geometry: usize,
    off_screen: usize,
    capture_unavailable: usize,
}

#[derive(Debug, Clone, Serialize)]
struct ClassPairEvidence {
    class_a: String,
    class_b: String,
    entities_with_a: usize,
    entities_with_b: usize,
    entities_with_both: usize,
    probability_b_given_a: f64,
    probability_a_given_b: f64,
    both_without_loaded_rigid_component: usize,
}

#[derive(Debug, Default)]
struct ClassPairAccumulator {
    entities_with_both: usize,
    both_without_loaded_rigid_component: usize,
}

#[derive(Debug, Serialize)]
struct ChildClassReport {
    target_class: String,
    resolved_values: usize,
    classes: BTreeMap<String, ChildClassSummary>,
}

#[derive(Debug, Default, Serialize)]
struct ChildClassSummary {
    occurrences: usize,
    file_types: BTreeMap<String, usize>,
    examples: Vec<String>,
}

#[derive(Debug, Serialize)]
struct ClassificationReport {
    label: String,
    evidence: Vec<String>,
    unresolved_questions: Vec<String>,
}

#[derive(Debug, Serialize)]
struct RendererAdmissionReport {
    stable_visual_reference_contract: bool,
    consistent_typed_definition_layout: bool,
    substantial_occurrence_without_loaded_rigid_component: bool,
    frozen_clear_depth_gap_correlation: bool,
    admitted: bool,
}

#[derive(Debug, Serialize)]
struct TagResolutionSummary {
    resolved: usize,
    unresolved: usize,
    package_classes: BTreeMap<String, usize>,
}

#[derive(Debug, Serialize)]
struct Manifest {
    schema: &'static str,
    maps: Vec<String>,
    target_class: String,
    max_examples: usize,
    retained_examples: usize,
    total_occurrences: usize,
    structural_variants: usize,
    unique_complete_definition_hashes: usize,
    structural_variants_shared_by_both_maps: usize,
    complete_definition_hash_sets_identical: bool,
    shared_complete_definition_hash_count: usize,
    shared_complete_definition_hashes: Vec<String>,
    exact_repeated_default_percentage: f64,
    valid_definitions: usize,
    invalid_definitions: usize,
    definition_lengths: BTreeMap<usize, usize>,
    stable_reference_offset_count: usize,
    stable_reference_offsets_file: &'static str,
    paired_candidate_evidence: Option<ClassPairEvidence>,
    unk80_resolution: TagResolutionSummary,
    unk84_resolution: TagResolutionSummary,
    rigid_baseline: RigidBaseline,
    renderer_admission: RendererAdmissionReport,
    classification: ClassificationReport,
    capture_inputs: Vec<String>,
    notes: Vec<String>,
}

#[derive(Debug, Serialize)]
struct RankingEntry {
    class: String,
    score: f64,
    occurrences: usize,
    maps: Vec<String>,
    percentage_on_entities_without_loaded_rigid_component: f64,
    percentage_projecting_over_clear_depth: Option<f64>,
    stable_rigid_model_or_technique_reference_offsets: usize,
    rigid_stable_reference_class_jaccard_similarity: f64,
    proven_matrix_bounds_or_mesh_array_evidence: usize,
    exact_definition_default_percentage: f64,
    evidence: Vec<String>,
    permission_to_render: bool,
}

#[derive(Debug, Deserialize)]
struct FrozenCaptureManifest {
    map: String,
    width: usize,
    height: usize,
    /// Column-major world-to-clip matrix.
    view_projection: [f32; 16],
    camera_position: [f32; 3],
    clear_depth: f32,
    depth_f32_file: PathBuf,
    #[serde(default = "default_sample_radius")]
    sample_radius: usize,
}

fn default_sample_radius() -> usize {
    2
}

struct FrozenCapture {
    manifest: FrozenCaptureManifest,
    depth: Vec<f32>,
}

fn class_hex(class: u32) -> String {
    format!("0x{class:08X}")
}

fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02X}")).collect()
}

fn package_record(tag: TagHash, read_length: bool) -> Option<PackageRecord> {
    let manager = package_manager();
    let entry = manager.get_entry(tag)?;
    Some(PackageRecord {
        tag: tag.to_string(),
        reference: class_hex(entry.reference),
        file_type: entry.file_type,
        file_subtype: entry.file_subtype,
        declared_size: entry.file_size,
        byte_length: read_length
            .then(|| manager.read_tag(tag).ok().map(|bytes| bytes.len()))
            .flatten(),
    })
}

fn pointer_record(pointer: tiger_parse::ResourcePointer) -> PointerRecord {
    PointerRecord {
        valid: pointer.is_valid,
        resource_type: class_hex(pointer.resource_type),
        offset: pointer.offset,
    }
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

fn table_origins(map: TagHash) -> Result<(Option<TagHash>, BTreeMap<TagHash, BTreeSet<String>>)> {
    let manager = package_manager();
    let parent: SShadowkeepBubbleParent = manager.read_tag_struct(map)?;
    let definition: SShadowkeepBubbleDefinition = manager.read_tag_struct(parent.child_map)?;
    let mut origins = BTreeMap::<TagHash, BTreeSet<String>>::new();
    for table in definition
        .map_resources
        .iter()
        .flat_map(|container| container.data_tables.iter().copied())
    {
        origins.entry(table).or_default().insert("base".into());
    }
    let (scenario, scenario_tables) = scenario_tables(map)?;
    for table in scenario_tables {
        origins.entry(table).or_default().insert("scenario".into());
    }
    Ok((scenario, origins))
}

fn definition_analysis(
    resource: &SShadowkeepEntityResource,
    resource_bytes: &[u8],
) -> Result<DefinitionAnalysis> {
    let start = usize::try_from(resource.definition.offset)
        .context("definition offset exceeds addressable memory")?;
    if !resource.definition.is_valid || start >= resource_bytes.len() {
        bail!(
            "definition offset 0x{:X} is outside resource byte length 0x{:X}",
            resource.definition.offset,
            resource_bytes.len()
        );
    }
    let mut next_offsets = Vec::new();
    for pointer in [resource.unk8, resource.resource, resource.definition] {
        if pointer.is_valid
            && let Ok(offset) = usize::try_from(pointer.offset)
            && offset > start
            && offset <= resource_bytes.len()
        {
            next_offsets.push(offset);
        }
    }
    for pointer in &resource.resource_table {
        if pointer.is_valid
            && let Ok(offset) = usize::try_from(pointer.offset)
            && offset > start
            && offset <= resource_bytes.len()
        {
            next_offsets.push(offset);
        }
    }
    let end = next_offsets
        .into_iter()
        .min()
        .unwrap_or(resource_bytes.len());
    let span = &resource_bytes[start..end];
    let manager = package_manager();
    let child_tags = span
        .chunks_exact(4)
        .enumerate()
        .filter_map(|(word, chunk)| {
            let tag = TagHash(u32::from_le_bytes(chunk.try_into().unwrap()));
            package_record(tag, false).map(|package| {
                let offset = word * 4;
                let context_start = offset.saturating_sub(16);
                let context_end = (offset + 20).min(span.len());
                ChildTagRecord {
                    definition_offset: offset,
                    package,
                    context_hex_16_before_after: hex(&span[context_start..context_end]),
                }
            })
        })
        .collect::<Vec<_>>();
    let child_class_signature = child_tags
        .iter()
        .map(|child| child.package.reference.clone())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let _ = manager;
    Ok(DefinitionAnalysis {
        span_length: span.len(),
        sha256: sha256(span),
        prefix_sha256: sha256(&span[..span.len().min(DEFINITION_PREFIX_BYTES)]),
        hex_dump: hex(&span[..span.len().min(DEFINITION_DUMP_BYTES)]),
        child_tags,
        child_class_signature,
    })
}

fn record_reference_cluster(stats: &mut ClassStats, analysis: &DefinitionAnalysis) {
    let cluster = stats
        .reference_clusters
        .entry(analysis.span_length)
        .or_default();
    cluster.occurrences += 1;
    for child in &analysis.child_tags {
        let reference = cluster
            .references
            .entry((child.definition_offset, child.package.reference.clone()))
            .or_default();
        reference.occurrences_at_offset += 1;
        reference.tags.insert(child.package.tag.clone());
        reference
            .contexts
            .insert(child.context_hex_16_before_after.clone());
    }
}

fn stable_reference_offsets(stats: &ClassStats) -> Vec<StableReferenceOffset> {
    let mut stable = Vec::new();
    for (definition_length, cluster) in &stats.reference_clusters {
        if cluster.occurrences < 4 {
            continue;
        }
        for ((offset, package_class), reference) in &cluster.references {
            let coverage =
                reference.occurrences_at_offset as f64 * 100.0 / cluster.occurrences as f64;
            if coverage < 75.0 {
                continue;
            }
            stable.push(StableReferenceOffset {
                definition_length: *definition_length,
                cluster: format!("definition_length_{definition_length}"),
                offset: *offset,
                package_class: package_class.clone(),
                cluster_occurrences: cluster.occurrences,
                occurrences_at_offset: reference.occurrences_at_offset,
                coverage_percentage: coverage,
                distinct_tags: reference.tags.len(),
                example_tags: reference.tags.iter().take(8).cloned().collect(),
                surrounding_bytes: reference.contexts.iter().take(4).cloned().collect(),
            });
        }
    }
    stable
}

fn class_pair_evidence(
    class_entities: &BTreeMap<u32, usize>,
    pairs: &BTreeMap<(u32, u32), ClassPairAccumulator>,
) -> Vec<ClassPairEvidence> {
    pairs
        .iter()
        .map(|(&(class_a, class_b), pair)| {
            let entities_with_a = class_entities.get(&class_a).copied().unwrap_or_default();
            let entities_with_b = class_entities.get(&class_b).copied().unwrap_or_default();
            ClassPairEvidence {
                class_a: class_hex(class_a),
                class_b: class_hex(class_b),
                entities_with_a,
                entities_with_b,
                entities_with_both: pair.entities_with_both,
                probability_b_given_a: pair.entities_with_both as f64
                    / entities_with_a.max(1) as f64,
                probability_a_given_b: pair.entities_with_both as f64
                    / entities_with_b.max(1) as f64,
                both_without_loaded_rigid_component: pair.both_without_loaded_rigid_component,
            }
        })
        .collect()
}

fn nested_records(resource: &SShadowkeepEntityResource) -> Vec<NestedPointerRecord> {
    resource
        .resource_table
        .iter()
        .map(|pointer| NestedPointerRecord {
            valid: pointer.is_valid,
            resource_type: class_hex(pointer.resource_type),
            class_type: class_hex(pointer.class_type),
            parent_tag: pointer.parent_tag.to_string(),
            offset: pointer.offset,
        })
        .collect()
}

fn nested_signature(resource: &SShadowkeepEntityResource) -> Vec<String> {
    resource
        .resource_table
        .iter()
        .map(|pointer| {
            format!(
                "{}:{}:{}",
                class_hex(pointer.resource_type),
                class_hex(pointer.class_type),
                if pointer.is_valid { "valid" } else { "invalid" }
            )
        })
        .collect()
}

fn class_signature(classes: &[u32]) -> Vec<String> {
    classes.iter().copied().map(class_hex).collect()
}

fn cluster_key(
    package_class: &str,
    analysis: &DefinitionAnalysis,
    nested: &[String],
    siblings: &[String],
) -> String {
    format!(
        "{}|{}|{}|{}|{}|{}|{}",
        package_class,
        analysis.span_length,
        analysis.prefix_sha256,
        analysis.sha256,
        nested.join(","),
        analysis.child_class_signature.join(","),
        siblings.join(",")
    )
}

fn parse_cluster_key(key: &str, accumulator: &ClusterAccumulator) -> DefinitionCluster {
    let mut fields = key.split('|');
    DefinitionCluster {
        entity_resource_package_class: fields.next().unwrap_or_default().into(),
        definition_length: fields.next().unwrap_or("0").parse().unwrap_or(0),
        definition_prefix_sha256: fields.next().unwrap_or_default().into(),
        definition_sha256: fields.next().unwrap_or_default().into(),
        nested_resource_table_signature: split_signature(fields.next().unwrap_or_default()),
        resolved_child_class_signature: split_signature(fields.next().unwrap_or_default()),
        sibling_entity_resource_class_signature: split_signature(fields.next().unwrap_or_default()),
        classification: "F. Unresolved",
        count: accumulator.count,
        maps: accumulator.maps.clone(),
        representative_entity_resource: accumulator.representative.clone(),
    }
}

fn split_signature(value: &str) -> Vec<String> {
    if value.is_empty() {
        Vec::new()
    } else {
        value.split(',').map(str::to_owned).collect()
    }
}

fn read_captures(paths: &[PathBuf]) -> Result<BTreeMap<String, FrozenCapture>> {
    let mut captures = BTreeMap::new();
    for path in paths {
        let manifest: FrozenCaptureManifest = serde_json::from_slice(
            &fs::read(path)
                .with_context(|| format!("Reading capture manifest {}", path.display()))?,
        )
        .with_context(|| format!("Parsing capture manifest {}", path.display()))?;
        let depth_path = path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join(&manifest.depth_f32_file);
        let bytes = fs::read(&depth_path)
            .with_context(|| format!("Reading depth capture {}", depth_path.display()))?;
        if bytes.len() != manifest.width * manifest.height * 4 {
            bail!(
                "Depth capture {} has {} bytes, expected {}",
                depth_path.display(),
                bytes.len(),
                manifest.width * manifest.height * 4
            );
        }
        let depth = bytes
            .chunks_exact(4)
            .map(|chunk| f32::from_le_bytes(chunk.try_into().unwrap()))
            .collect();
        captures.insert(
            manifest.map.to_ascii_uppercase(),
            FrozenCapture { manifest, depth },
        );
    }
    Ok(captures)
}

fn correlate_gap(capture: Option<&FrozenCapture>, translation: [f32; 3]) -> GapEvidence {
    let Some(capture) = capture else {
        return GapEvidence::unavailable();
    };
    let matrix = Mat4::from_cols_array(&capture.manifest.view_projection);
    let world = Vec4::new(translation[0], translation[1], translation[2], 1.0);
    let clip = matrix * world;
    let in_front = clip.w > 0.0 && clip.is_finite();
    let ndc = clip / clip.w;
    let inside = in_front
        && (-1.0..=1.0).contains(&ndc.x)
        && (-1.0..=1.0).contains(&ndc.y)
        && (0.0..=1.0).contains(&ndc.z);
    let screen = [
        (ndc.x * 0.5 + 0.5) * capture.manifest.width as f32,
        (1.0 - (ndc.y * 0.5 + 0.5)) * capture.manifest.height as f32,
    ];
    let camera = Vec4::new(
        capture.manifest.camera_position[0],
        capture.manifest.camera_position[1],
        capture.manifest.camera_position[2],
        0.0,
    );
    let distance = (world - camera).xyz().length();
    if !inside {
        return GapEvidence {
            capture_available: true,
            in_front_of_camera: Some(in_front),
            inside_viewport: Some(false),
            screen_pixel: Some(screen),
            projected_distance: Some(distance),
            clear_depth_percentage: None,
            nearest_non_clear_depth: None,
            note: "Placement origin is outside the frozen viewport.".into(),
        };
    }
    let x = screen[0].floor() as isize;
    let y = screen[1].floor() as isize;
    let radius = capture.manifest.sample_radius as isize;
    let mut samples = Vec::new();
    for sample_y in (y - radius)..=(y + radius) {
        for sample_x in (x - radius)..=(x + radius) {
            if sample_x >= 0
                && sample_y >= 0
                && sample_x < capture.manifest.width as isize
                && sample_y < capture.manifest.height as isize
            {
                samples.push(
                    capture.depth[sample_y as usize * capture.manifest.width + sample_x as usize],
                );
            }
        }
    }
    let clear = samples
        .iter()
        .filter(|depth| (**depth - capture.manifest.clear_depth).abs() <= f32::EPSILON)
        .count();
    let nearest = samples
        .iter()
        .copied()
        .filter(|depth| (*depth - capture.manifest.clear_depth).abs() > f32::EPSILON)
        .max_by(f32::total_cmp);
    GapEvidence {
        capture_available: true,
        in_front_of_camera: Some(true),
        inside_viewport: Some(true),
        screen_pixel: Some(screen),
        projected_distance: Some(distance),
        clear_depth_percentage: Some(clear as f64 * 100.0 / samples.len().max(1) as f64),
        nearest_non_clear_depth: nearest,
        note: "Supporting origin correlation only; it is not treated as proof of geometry.".into(),
    }
}

fn write_json(path: &Path, value: &impl Serialize) -> Result<()> {
    fs::write(path, serde_json::to_vec_pretty(value)?)
        .with_context(|| format!("Writing {}", path.display()))
}

fn main() -> Result<()> {
    let args = Args::parse();
    let target_class = u32::from_str_radix(args.class.trim_start_matches("0x"), 16)
        .context("--class must be hexadecimal")?;
    let maps = args
        .maps
        .iter()
        .map(|map| TagHash::from_str(map).with_context(|| format!("Invalid --map {map}")))
        .collect::<Result<Vec<_>>>()?;
    alkahest_core::initialize_package_manager(
        None,
        Some(args.packages.to_string_lossy().as_ref()),
    )?;
    fs::create_dir_all(&args.output)
        .with_context(|| format!("Creating {}", args.output.display()))?;
    let captures = read_captures(&args.captures)?;

    let mut target_definition_pointer_types = BTreeSet::new();
    let mut target_nonempty_nested_tables = 0usize;
    let mut examples = Vec::<ExampleRecord>::new();
    let mut stats = BTreeMap::<u32, ClassStats>::new();
    let mut target_clusters = BTreeMap::<String, ClusterAccumulator>::new();
    let mut analysis_cache = HashMap::<(u32, u64), DefinitionAnalysis>::new();
    let mut rigid_baseline = RigidBaseline::default();
    let mut sibling_class_counts = BTreeMap::<String, usize>::new();
    let mut target_child_classes = BTreeMap::<String, ChildClassSummary>::new();
    let mut target_resolved_values = 0usize;
    let mut target_occurrences = 0usize;
    let mut class_entity_counts = BTreeMap::<u32, usize>::new();
    let mut class_pairs = BTreeMap::<(u32, u32), ClassPairAccumulator>::new();
    let mut target_unk80_resolved = 0usize;
    let mut target_unk80_unresolved = 0usize;
    let mut target_unk80_classes = BTreeMap::<String, usize>::new();
    let mut target_unk84_resolved = 0usize;
    let mut target_unk84_unresolved = 0usize;
    let mut target_unk84_classes = BTreeMap::<String, usize>::new();

    for map in &maps {
        let map_name = map.to_string().to_ascii_uppercase();
        let (scenario, origins) = table_origins(*map)?;
        for (table_tag, table_origins) in origins {
            let table: SShadowkeepMapDataTable = package_manager()
                .read_tag_struct(table_tag)
                .with_context(|| format!("Reading table {table_tag} for map {map}"))?;
            for entry in &table.data_entries {
                if entry.entity.is_none() {
                    continue;
                }
                let entity: SShadowkeepEntity = package_manager()
                    .read_tag_struct(entry.entity)
                    .with_context(|| format!("Reading entity {}", entry.entity))?;
                let mut sibling_classes = entity
                    .entity_resources
                    .iter()
                    .map(|resource_ref| resource_ref.resource.resource.resource_type)
                    .collect::<Vec<_>>();
                sibling_classes.sort_unstable();
                let has_rigid = entity.entity_resources.iter().any(|resource_ref| {
                    resource_ref.resource.resource.resource_type == RIGID_MODEL_COMPONENT
                        && resource_ref.resource.resource.is_valid
                        && resource_ref.resource.definition.is_valid
                });
                let sibling_strings = class_signature(&sibling_classes);
                let sibling_signature = sibling_strings.join(",");
                let entity_classes = sibling_classes
                    .iter()
                    .copied()
                    .collect::<BTreeSet<_>>()
                    .into_iter()
                    .collect::<Vec<_>>();
                for class in &entity_classes {
                    *class_entity_counts.entry(*class).or_default() += 1;
                }
                for (index, class_a) in entity_classes.iter().enumerate() {
                    for class_b in entity_classes.iter().skip(index + 1) {
                        let pair = class_pairs.entry((*class_a, *class_b)).or_default();
                        pair.entities_with_both += 1;
                        if !has_rigid {
                            pair.both_without_loaded_rigid_component += 1;
                        }
                    }
                }

                for resource_ref in &entity.entity_resources {
                    let resource_tag = resource_ref.resource.taghash();
                    let resource = &*resource_ref.resource;
                    let class = resource.resource.resource_type;
                    let class_stats = stats.entry(class).or_default();
                    class_stats.occurrences += 1;
                    class_stats.entities.insert(entry.entity.to_string());
                    class_stats.maps.insert(map_name.clone());
                    *class_stats
                        .sibling_signatures
                        .entry(sibling_signature.clone())
                        .or_default() += 1;
                    if has_rigid {
                        class_stats.on_entities_with_rigid += 1;
                    } else {
                        class_stats.on_entities_without_loaded_rigid_component += 1;
                    }
                    let gap_evidence =
                        correlate_gap(captures.get(&map_name), entry.translation.xyz().to_array());
                    match (
                        gap_evidence.capture_available,
                        gap_evidence.inside_viewport,
                        gap_evidence.clear_depth_percentage,
                    ) {
                        (false, _, _) => class_stats.capture_unavailable += 1,
                        (true, Some(false), _) => class_stats.off_screen += 1,
                        (true, Some(true), Some(clear)) if clear >= 50.0 => {
                            class_stats.projected_over_mostly_clear_depth += 1
                        }
                        (true, Some(true), Some(_)) => {
                            class_stats.projected_over_existing_geometry += 1
                        }
                        _ => {}
                    }
                    if resource.definition.is_valid {
                        class_stats.valid_definitions += 1;
                    } else {
                        class_stats.invalid_definitions += 1;
                        continue;
                    }

                    let resource_bytes = package_manager()
                        .read_tag(resource_tag)
                        .with_context(|| format!("Reading entity resource {resource_tag}"))?;
                    let analysis = analysis_cache
                        .entry((resource_tag.0, resource.definition.offset))
                        .or_insert_with(|| {
                            definition_analysis(resource, &resource_bytes)
                                .expect("validated entity-resource definition span")
                        })
                        .clone();
                    *class_stats
                        .complete_hash_counts
                        .entry(analysis.sha256.clone())
                        .or_default() += 1;
                    *class_stats
                        .definition_lengths
                        .entry(analysis.span_length)
                        .or_default() += 1;
                    for child_class in &analysis.child_class_signature {
                        *class_stats
                            .child_class_counts
                            .entry(child_class.clone())
                            .or_default() += 1;
                    }
                    record_reference_cluster(class_stats, &analysis);
                    let nested = nested_signature(resource);
                    *class_stats
                        .nested_signatures
                        .entry(nested.join(","))
                        .or_default() += 1;

                    if class == RIGID_MODEL_COMPONENT {
                        rigid_baseline.occurrences += 1;
                        *rigid_baseline
                            .definition_lengths
                            .entry(analysis.span_length)
                            .or_default() += 1;
                        for child_class in &analysis.child_class_signature {
                            *rigid_baseline
                                .child_tag_reference_pattern
                                .entry(child_class.clone())
                                .or_default() += 1;
                        }
                        let mut cursor = Cursor::new(resource_bytes.as_slice());
                        cursor.seek(SeekFrom::Start(resource.definition.offset))?;
                        if let Ok(component) = SShadowkeepRigidModelComponent::read_ds(&mut cursor)
                        {
                            rigid_baseline.parsed_definitions += 1;
                            if let Some(package) = package_record(component.model, false) {
                                *rigid_baseline
                                    .model_package_classes
                                    .entry(package.reference)
                                    .or_default() += 1;
                            }
                            *rigid_baseline
                                .material_variant_record_counts
                                .entry(component.material_variants.len())
                                .or_default() += 1;
                            *rigid_baseline
                                .technique_reference_counts
                                .entry(component.techniques.len())
                                .or_default() += 1;
                            for technique in &component.techniques {
                                if let Some(package) = package_record(*technique, false) {
                                    *rigid_baseline
                                        .technique_package_classes
                                        .entry(package.reference)
                                        .or_default() += 1;
                                }
                            }
                        }
                    }

                    if class != target_class {
                        continue;
                    }
                    target_definition_pointer_types
                        .insert(class_hex(resource.definition.resource_type));
                    if !resource.resource_table.is_empty() {
                        target_nonempty_nested_tables += 1;
                    }
                    target_occurrences += 1;
                    let unk80_package = package_record(resource.unk80, false);
                    if let Some(package) = &unk80_package {
                        target_unk80_resolved += 1;
                        *target_unk80_classes
                            .entry(package.reference.clone())
                            .or_default() += 1;
                    } else {
                        target_unk80_unresolved += 1;
                    }
                    let unk84_package = package_record(resource.unk84, false);
                    if let Some(package) = &unk84_package {
                        target_unk84_resolved += 1;
                        *target_unk84_classes
                            .entry(package.reference.clone())
                            .or_default() += 1;
                    } else {
                        target_unk84_unresolved += 1;
                    }
                    for child in &analysis.child_tags {
                        target_resolved_values += 1;
                        let summary = target_child_classes
                            .entry(child.package.reference.clone())
                            .or_default();
                        summary.occurrences += 1;
                        *summary
                            .file_types
                            .entry(format!(
                                "{}:{}",
                                child.package.file_type, child.package.file_subtype
                            ))
                            .or_default() += 1;
                        let example = format!(
                            "map={map_name} table={table_tag} entity={} resource={resource_tag} \
                             definition+0x{:X} tag={}",
                            entry.entity, child.definition_offset, child.package.tag
                        );
                        if summary.examples.len() < 8 && !summary.examples.contains(&example) {
                            summary.examples.push(example);
                        }
                    }
                    for sibling in &sibling_strings {
                        *sibling_class_counts.entry(sibling.clone()).or_default() += 1;
                    }
                    let package = package_record(resource_tag, true)
                        .with_context(|| format!("Resolving entity resource {resource_tag}"))?;
                    let key = cluster_key(&package.reference, &analysis, &nested, &sibling_strings);
                    let cluster = target_clusters.entry(key).or_default();
                    cluster.count += 1;
                    *cluster.maps.entry(map_name.clone()).or_default() += 1;
                    if cluster.representative.is_empty() {
                        cluster.representative = resource_tag.to_string();
                    }
                    examples.push(ExampleRecord {
                        map: map_name.clone(),
                        scenario: scenario.map(|tag| tag.to_string()),
                        table_origin: table_origins.iter().cloned().collect(),
                        table: table_tag.to_string(),
                        entity: entry.entity.to_string(),
                        entity_resource: package,
                        resource_pointer: pointer_record(resource.resource),
                        definition_pointer: pointer_record(resource.definition),
                        placement: PlacementRecord {
                            translation: entry.translation.xyz().to_array(),
                            rotation: entry.rotation.to_array(),
                            scale: [entry.translation.w; 3],
                        },
                        sibling_entity_resource_classes: sibling_strings.clone(),
                        has_loaded_rigid_component: has_rigid,
                        nested_resource_table: nested_records(resource),
                        unk80: resource.unk80.to_string(),
                        unk80_package,
                        unk84: resource.unk84.to_string(),
                        unk84_package,
                        definition_tag_values: analysis.child_tags.clone(),
                        likely_definition_span_length: analysis.span_length,
                        definition_sha256: analysis.sha256,
                        definition_prefix_sha256: analysis.prefix_sha256,
                        definition_hex_0x200: analysis.hex_dump,
                        gap_evidence,
                    });
                }
            }
        }
    }

    let target_stats = stats
        .get(&target_class)
        .with_context(|| format!("Class {} did not occur", class_hex(target_class)))?;
    let rigid_classes = rigid_baseline
        .model_package_classes
        .keys()
        .chain(rigid_baseline.technique_package_classes.keys())
        .cloned()
        .collect::<BTreeSet<_>>();
    examples.sort_by(|left, right| {
        let left_overlap = left
            .definition_tag_values
            .iter()
            .filter(|child| rigid_classes.contains(&child.package.reference))
            .count();
        let right_overlap = right
            .definition_tag_values
            .iter()
            .filter(|child| rigid_classes.contains(&child.package.reference))
            .count();
        right_overlap.cmp(&left_overlap).then_with(|| {
            (
                &left.map,
                &left.definition_sha256,
                &left.entity_resource.tag,
                &left.table,
            )
                .cmp(&(
                    &right.map,
                    &right.definition_sha256,
                    &right.entity_resource.tag,
                    &right.table,
                ))
        })
    });
    let mut retained = Vec::new();
    let mut retained_map_hashes = BTreeSet::new();
    for example in &examples {
        let has_rigid_overlap = example
            .definition_tag_values
            .iter()
            .any(|child| rigid_classes.contains(&child.package.reference));
        let map_hash = (example.map.clone(), example.definition_sha256.clone());
        if retained.len() < args.max_examples
            && (has_rigid_overlap || retained_map_hashes.insert(map_hash))
        {
            retained.push(example.clone());
        }
    }
    for example in examples {
        if retained.len() == args.max_examples {
            break;
        }
        let identity = (
            example.map.clone(),
            example.table.clone(),
            example.entity.clone(),
            example.entity_resource.tag.clone(),
        );
        if !retained.iter().any(|candidate| {
            (
                candidate.map.clone(),
                candidate.table.clone(),
                candidate.entity.clone(),
                candidate.entity_resource.tag.clone(),
            ) == identity
        }) {
            retained.push(example);
        }
    }

    let clusters = target_clusters
        .iter()
        .map(|(key, accumulator)| parse_cluster_key(key, accumulator))
        .collect::<Vec<_>>();
    let map_hash_sets = maps
        .iter()
        .map(|map| {
            let name = map.to_string().to_ascii_uppercase();
            clusters
                .iter()
                .filter(|cluster| cluster.maps.contains_key(&name))
                .map(|cluster| cluster.definition_sha256.clone())
                .collect::<BTreeSet<_>>()
        })
        .collect::<Vec<_>>();
    let complete_definition_hash_sets_identical =
        map_hash_sets.windows(2).all(|pair| pair[0] == pair[1]);
    let shared_complete_definition_hashes = map_hash_sets.first().cloned().unwrap_or_default();
    let shared_complete_definition_hashes = map_hash_sets
        .iter()
        .skip(1)
        .fold(shared_complete_definition_hashes, |shared, current| {
            shared.intersection(current).cloned().collect()
        });
    let default_count = target_stats
        .complete_hash_counts
        .values()
        .copied()
        .max()
        .unwrap_or(0);
    let repeated_default_percentage =
        default_count as f64 * 100.0 / target_stats.occurrences.max(1) as f64;
    let target_stable_references = stable_reference_offsets(target_stats);
    let all_pair_evidence = class_pair_evidence(&class_entity_counts, &class_pairs);
    let paired_candidate_evidence = PAIRED_CANDIDATE_CLASSES
        .contains(&target_class)
        .then(|| {
            let class_a = class_hex(PAIRED_CANDIDATE_CLASSES[0]);
            let class_b = class_hex(PAIRED_CANDIDATE_CLASSES[1]);
            all_pair_evidence
                .iter()
                .find(|pair| pair.class_a == class_a && pair.class_b == class_b)
                .cloned()
        })
        .flatten();

    let cooccurrence = CooccurrenceReport {
        target_class: class_hex(target_class),
        occurrences: target_occurrences,
        on_entities_with_loaded_rigid_component: target_stats.on_entities_with_rigid,
        on_entities_without_loaded_rigid_component: target_stats
            .on_entities_without_loaded_rigid_component,
        sibling_class_counts,
        sibling_signatures: target_stats.sibling_signatures.clone(),
        projected_over_mostly_clear_depth: target_stats.projected_over_mostly_clear_depth,
        projected_over_existing_geometry: target_stats.projected_over_existing_geometry,
        off_screen: target_stats.off_screen,
        capture_unavailable: target_stats.capture_unavailable,
    };

    let child_report = ChildClassReport {
        target_class: class_hex(target_class),
        resolved_values: target_resolved_values,
        classes: target_child_classes,
    };

    let mut ranking = Vec::new();
    for (class, class_stats) in &stats {
        if *class == RIGID_MODEL_COMPONENT {
            continue;
        }
        let stable_references = stable_reference_offsets(class_stats);
        let stable_child_classes = stable_references
            .iter()
            .map(|reference| reference.package_class.clone())
            .collect::<BTreeSet<_>>();
        let intersection = stable_child_classes.intersection(&rigid_classes).count();
        let union = stable_child_classes.union(&rigid_classes).count();
        let jaccard = intersection as f64 / union.max(1) as f64;
        let without_rigid = class_stats.on_entities_without_loaded_rigid_component as f64 * 100.0
            / class_stats.occurrences.max(1) as f64;
        let default = class_stats
            .complete_hash_counts
            .values()
            .copied()
            .max()
            .unwrap_or(0) as f64
            * 100.0
            / class_stats.occurrences.max(1) as f64;
        let stable_rigid_offsets = stable_references
            .iter()
            .filter(|reference| rigid_classes.contains(&reference.package_class))
            .count();
        let projected = class_stats.projected_over_mostly_clear_depth
            + class_stats.projected_over_existing_geometry;
        let clear_depth_percentage = (projected > 0).then(|| {
            class_stats.projected_over_mostly_clear_depth as f64 * 100.0 / projected as f64
        });
        let map_independent_default_penalty =
            if default >= 95.0 && class_stats.maps.len() > 1 && class_stats.occurrences >= 100 {
                15.0
            } else {
                0.0
            };
        let frequency_confidence = (class_stats.occurrences as f64 + 1.0).log10() * 2.0;
        let stable_visual_reference_confidence = (stable_rigid_offsets as f64 * 10.0).min(25.0);
        let spatial_confidence = clear_depth_percentage.unwrap_or_default() * 0.20;
        let score = (without_rigid * 0.30
            + jaccard * 15.0
            + stable_visual_reference_confidence
            + spatial_confidence
            + frequency_confidence
            - default * 0.30
            - map_independent_default_penalty)
            .clamp(-100.0, 100.0);
        ranking.push(RankingEntry {
            class: class_hex(*class),
            score,
            occurrences: class_stats.occurrences,
            maps: class_stats.maps.iter().cloned().collect(),
            percentage_on_entities_without_loaded_rigid_component: without_rigid,
            percentage_projecting_over_clear_depth: clear_depth_percentage,
            stable_rigid_model_or_technique_reference_offsets: stable_rigid_offsets,
            rigid_stable_reference_class_jaccard_similarity: jaccard,
            proven_matrix_bounds_or_mesh_array_evidence: 0,
            exact_definition_default_percentage: default,
            evidence: vec![
                format!(
                    "{} of {} occurrences are on entities without a loaded rigid entity component",
                    class_stats.on_entities_without_loaded_rigid_component, class_stats.occurrences
                ),
                format!(
                    "{stable_rigid_offsets} stable reference offsets resolve to rigid \
                     model/technique package classes; {intersection} stable package classes \
                     overlap that typed baseline"
                ),
                "Stable references require at least four occurrences in one definition-length \
                 cluster and at least 75% same-class coverage at one aligned offset."
                    .into(),
                "Matrix, bounds, and mesh-array terms remain zero without a proven schema; \
                 arbitrary floats are not interpreted."
                    .into(),
                match clear_depth_percentage {
                    Some(percentage) => format!(
                        "{percentage:.2}% of on-screen frozen placement origins project over at \
                         least 50% clear depth in the sampled neighborhood"
                    ),
                    None => "Clear-depth contribution is unavailable because no target placement \
                             origin projected inside a supplied frozen capture."
                        .into(),
                },
                format!("The dominant exact definition hash covers {default:.2}% of occurrences"),
                format!(
                    "Map-independent dominant-default penalty is \
                     {map_independent_default_penalty:.1}"
                ),
            ],
            permission_to_render: false,
        });
    }
    ranking.sort_by(|left, right| right.score.total_cmp(&left.score));

    let stable_visual_references = target_stable_references
        .iter()
        .filter(|reference| rigid_classes.contains(&reference.package_class))
        .count();
    let target_projected = target_stats.projected_over_mostly_clear_depth
        + target_stats.projected_over_existing_geometry;
    let target_clear_depth_percentage = (target_projected > 0).then(|| {
        target_stats.projected_over_mostly_clear_depth as f64 * 100.0 / target_projected as f64
    });
    let renderer_admission = RendererAdmissionReport {
        stable_visual_reference_contract: stable_visual_references > 0,
        consistent_typed_definition_layout: false,
        substantial_occurrence_without_loaded_rigid_component: target_stats
            .on_entities_without_loaded_rigid_component
            >= 25
            && target_stats.on_entities_without_loaded_rigid_component * 4
                >= target_stats.occurrences,
        frozen_clear_depth_gap_correlation: target_projected >= 4
            && target_clear_depth_percentage.is_some_and(|percentage| percentage >= 50.0),
        admitted: false,
    };
    let pair_evidence_text = paired_candidate_evidence.as_ref().map_or_else(
        || "No paired-candidate relationship was requested for this target class.".to_owned(),
        |pair| {
            format!(
                "{} and {} co-occur on {} entities, including {} entities without a loaded rigid \
                 component; P(B|A)={:.6}, P(A|B)={:.6}",
                pair.class_a,
                pair.class_b,
                pair.entities_with_both,
                pair.both_without_loaded_rigid_component,
                pair.probability_b_given_a,
                pair.probability_a_given_b,
            )
        },
    );
    let classification = ClassificationReport {
        label: "F. Unresolved".into(),
        evidence: vec![
            format!(
                "All {} structural variants use entity-resource package classes {:?}, resource \
                 pointer class {}, definition pointer classes {:?}; {} occurrences have a \
                 non-empty nested resource table",
                clusters.len(),
                clusters
                    .iter()
                    .map(|cluster| cluster.entity_resource_package_class.clone())
                    .collect::<BTreeSet<_>>(),
                class_hex(target_class),
                target_definition_pointer_types,
                target_nonempty_nested_tables,
            ),
            format!(
                "{} complete definition hashes and {} structural variants occur across {} valid \
                 definitions; {} complete hashes are shared by every supplied map and whole-map \
                 hash sets are {}",
                target_stats.complete_hash_counts.len(),
                clusters.len(),
                target_stats.valid_definitions,
                shared_complete_definition_hashes.len(),
                if complete_definition_hash_sets_identical {
                    "identical"
                } else {
                    "different"
                },
            ),
            format!(
                "{} instances ({:.2}%) co-occur with a loaded 0x808072B8 rigid component; {} \
                 occur without a loaded rigid entity component",
                target_stats.on_entities_with_rigid,
                target_stats.on_entities_with_rigid as f64 * 100.0
                    / target_stats.occurrences.max(1) as f64,
                target_stats.on_entities_without_loaded_rigid_component
            ),
            format!(
                "{} stable reference offsets survive the cluster-size and coverage threshold; {} \
                 resolve to a package class used by the typed rigid model/technique baseline",
                target_stable_references.len(),
                stable_visual_references,
            ),
            match target_clear_depth_percentage {
                Some(percentage) => format!(
                    "{} frozen on-screen origins were sampled and {percentage:.2}% project over \
                     mostly clear depth",
                    target_projected
                ),
                None => "No placement origin projected inside a supplied frozen depth capture, so \
                         spatial evidence is unavailable."
                    .into(),
            },
            pair_evidence_text,
            format!(
                "unk80 resolves for {target_unk80_resolved} occurrences and is unresolved for \
                 {target_unk80_unresolved}; unk84 resolves for {target_unk84_resolved} and is \
                 unresolved for {target_unk84_unresolved}"
            ),
            "Aligned package-tag words remain permissive collision evidence unless they survive \
             the stable same-class/same-offset threshold."
                .into(),
            "The four-part renderer gate is closed because no consistent typed definition layout \
             has been proven; permission_to_render remains false."
                .into(),
        ],
        unresolved_questions: vec![
            format!(
                "The {} definition's field ownership and runtime semantics remain unproven; no \
                 production tiger_type is admitted.",
                class_hex(target_class)
            ),
            "No package evidence yet establishes a complete model, material, technique, mesh, or \
             indirect visual-child contract for this class."
                .into(),
            "Frozen placement-origin correlation is supporting evidence only and cannot establish \
             geometry ownership without a typed layout."
                .into(),
        ],
    };
    let shared_complete_definition_hash_count = shared_complete_definition_hashes.len();
    let manifest = Manifest {
        schema: "alkahest-shadowkeep-entity-resource-dossier/v2",
        maps: maps.iter().map(ToString::to_string).collect(),
        target_class: class_hex(target_class),
        max_examples: args.max_examples,
        retained_examples: retained.len(),
        total_occurrences: target_occurrences,
        structural_variants: clusters.len(),
        unique_complete_definition_hashes: target_stats.complete_hash_counts.len(),
        structural_variants_shared_by_both_maps: clusters
            .iter()
            .filter(|cluster| cluster.maps.len() == maps.len())
            .count(),
        complete_definition_hash_sets_identical,
        shared_complete_definition_hash_count,
        shared_complete_definition_hashes: shared_complete_definition_hashes.into_iter().collect(),
        exact_repeated_default_percentage: repeated_default_percentage,
        valid_definitions: target_stats.valid_definitions,
        invalid_definitions: target_stats.invalid_definitions,
        definition_lengths: target_stats.definition_lengths.clone(),
        stable_reference_offset_count: target_stable_references.len(),
        stable_reference_offsets_file: "stable_reference_offsets.json",
        paired_candidate_evidence,
        unk80_resolution: TagResolutionSummary {
            resolved: target_unk80_resolved,
            unresolved: target_unk80_unresolved,
            package_classes: target_unk80_classes,
        },
        unk84_resolution: TagResolutionSummary {
            resolved: target_unk84_resolved,
            unresolved: target_unk84_unresolved,
            package_classes: target_unk84_classes,
        },
        rigid_baseline,
        renderer_admission,
        classification,
        capture_inputs: args
            .captures
            .iter()
            .map(|path| path.display().to_string())
            .collect(),
        notes: vec![
            "Definition spans stop at the next higher valid pointer in the same entity-resource \
             file, otherwise at file end."
                .into(),
            "Raw aligned u32 package resolutions are weak evidence; stable offsets require \
             cluster count >= 4 and same package class at the same offset in >= 75% of instances."
                .into(),
            "The score ranks investigation candidates and never grants permission to render."
                .into(),
            "A placement origin over clear depth is correlation, not proof that the entity \
             component owns geometry."
                .into(),
        ],
    };

    write_json(&args.output.join("manifest.json"), &manifest)?;
    write_json(&args.output.join("examples.json"), &retained)?;
    write_json(&args.output.join("cooccurrence.json"), &cooccurrence)?;
    write_json(&args.output.join("child_tag_classes.json"), &child_report)?;
    write_json(
        &args.output.join("stable_reference_offsets.json"),
        &target_stable_references,
    )?;
    write_json(&args.output.join("definition_clusters.json"), &clusters)?;
    let artifact_root = args.output.parent().unwrap_or_else(|| Path::new("."));
    write_json(
        &artifact_root.join("shadowkeep-entity-class-cooccurrence.json"),
        &all_pair_evidence,
    )?;
    write_json(
        &artifact_root.join("shadowkeep-entity-class-ranking.json"),
        &ranking,
    )?;

    println!(
        "class={} maps={} occurrences={} variants={} examples={} output={}",
        class_hex(target_class),
        maps.len(),
        target_occurrences,
        clusters.len(),
        retained.len(),
        args.output.display()
    );
    Ok(())
}

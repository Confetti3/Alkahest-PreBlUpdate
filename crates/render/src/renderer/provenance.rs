use std::{collections::BTreeMap, fs, path::Path, sync::LazyLock};

use ahash::AHashMap;
use alkahest_core::ConVars;
use alkahest_data::tfx::{ExternIndex, shadowkeep::SShadowkeepTechnique};
use anyhow::{Context, bail};
use d3d11::{BindFlags, CpuAccessFlags, Texture2dDesc, dxgi};
use parking_lot::Mutex;
use serde::Serialize;
use sha2::{Digest, Sha256};
use tiger_parse::PackageManagerExt;
use tiger_pkg::{TagHash, package_manager};

use super::{Renderer, surface::Surface};
use crate::{
    gpu::command_list::CommandList,
    tfx::{
        expression_vm::{
            self,
            opcodes::{Opcode, OpcodeIterator},
        },
        sequencer_vm::ObjectChannel,
        technique::Technique,
    },
};

#[derive(Debug, Serialize)]
pub struct SurfaceProvenance {
    pub surface: String,
    pub file: String,
    pub format: String,
    pub resource_format: String,
    pub width: u32,
    pub height: u32,
    pub statistics_encoding: &'static str,
    pub finite_pixel_count: u64,
    pub non_finite_pixel_count: u64,
    pub clipped_or_saturated_pixel_count: u64,
    pub nonzero_rgb_pixel_count: u64,
    pub minimum_rgb: Option<[f64; 3]>,
    pub maximum_rgb: Option<[f64; 3]>,
    pub mean_rgb: Option<[f64; 3]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nonzero_alpha_count: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pixels_different_from_clear_value: Option<u64>,
    pub sha256: String,
}

#[derive(Debug, Serialize)]
pub struct DeferredShadingProvenance {
    pub technique: String,
    pub vertex_shader: Option<String>,
    pub pixel_shader: Option<String>,
    pub draw_6_reached: bool,
    pub vertex_expression: Option<String>,
    pub pixel_expression: Option<String>,
    pub vertex_constant_buffer_slot: Option<u32>,
    pub vertex_constant_buffer_len: Option<usize>,
    pub pixel_constant_buffer_slot: Option<u32>,
    pub pixel_constant_buffer_len: Option<usize>,
    pub bound_deferred_srvs: Vec<String>,
    pub output_rtv_format: String,
}
#[derive(Debug, Serialize)]
pub struct ShadowkeepLightingProvenance {
    pub requested_feature_subscriptions: String,
    pub active_feature_subscriptions: String,
    pub render_settings: String,
    pub assets_ready: usize,
    pub assets_queued: usize,
    pub assets_loading: usize,
    pub assets_failed: usize,
    pub assets_using_fallback: usize,
    pub lighting_apply_stage_submitted: bool,
    pub deferred_light_draw_indexed_calls: usize,
    pub local_light_technique_hashes: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct GlobalLightingExternRead {
    pub extern_index: String,
    pub value_type: String,
    pub byte_offset: u32,
}

#[derive(Debug, Serialize)]
pub struct GlobalLightingChannelValue {
    pub index: u8,
    pub value: [f32; 4],
}

#[derive(Debug, Serialize)]
pub struct GlobalLightingDependencyStage {
    pub stage: String,
    pub shader: String,
    pub translated_expression_disassembly: String,
    pub push_global_channel_vector_values: Vec<GlobalLightingChannelValue>,
    pub push_global_channel_vector_indices: Vec<u8>,
    pub extern_reads: Vec<GlobalLightingExternRead>,
    pub constant_buffer_slot: u32,
    pub constant_buffer_len: usize,
    pub sampler_slots: Vec<usize>,
    pub texture_slots: Vec<u32>,
    pub expression_evaluation_result: String,
}

#[derive(Debug, Serialize)]
pub struct GlobalLightingDependencyManifest {
    pub schema: &'static str,
    pub technique: String,
    pub draw_6_reached: bool,
    pub stages: Vec<GlobalLightingDependencyStage>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct SkyTechniqueGlobalChannelRead {
    pub index: u8,
    pub resolved_hash: Option<String>,
    pub value: Option<[String; 4]>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct SkyTechniqueTextureRead {
    pub slot: u32,
    pub texture: String,
    pub loaded: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct SkyTechniqueObjectChannelRead {
    pub hash: String,
    pub supplied_by_model: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct SkyTechniqueDependencyStage {
    pub stage: String,
    pub shader: String,
    pub translated_expression_disassembly: String,
    pub global_channel_reads: Vec<SkyTechniqueGlobalChannelRead>,
    pub object_channel_reads: Vec<SkyTechniqueObjectChannelRead>,
    pub extern_reads: Vec<GlobalLightingExternRead>,
    pub constant_buffer_slot: u32,
    pub constant_buffer_len: usize,
    pub sampler_slots: Vec<usize>,
    pub texture_slots: Vec<u32>,
    pub expression_evaluation_result: String,
    pub textures: Vec<SkyTechniqueTextureRead>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct SkyTechniqueDependency {
    pub collection: String,
    pub model: String,
    pub technique: String,
    pub stages: Vec<SkyTechniqueDependencyStage>,
}

#[derive(Debug, Serialize)]
struct SkyTechniqueDependencyManifest {
    schema: &'static str,
    map: String,
    collection: String,
    techniques: Vec<SkyTechniqueDependency>,
}
#[derive(Clone, Debug, Default, Serialize)]
pub struct SkyObjectsCaptureStats {
    pub draw_indexed_calls: usize,
    pub decals_additive_submission_reached: bool,
    pub decals_additive_draw_indexed_calls: usize,
    pub transparents_submission_reached: bool,
    pub transparents_draw_indexed_calls: usize,
    pub maps: Vec<String>,
    pub collections: Vec<String>,
    pub models: Vec<String>,
}

#[derive(Debug, Default, Serialize)]
pub struct SkyObjectDomainDelta {
    pub clear_depth_pixels: usize,
    pub geometry_pixels: usize,
    pub changed_clear_depth_pixels: usize,
    pub changed_geometry_pixels: usize,
    pub clear_depth_rmse_rgb: [f64; 3],
    pub geometry_rmse_rgb: [f64; 3],
    pub clear_depth_mean_abs_delta_rgb: [f64; 3],
    pub geometry_mean_abs_delta_rgb: [f64; 3],
    pub gbuffer_depth_byte_identical: bool,
}

#[derive(Debug, Serialize)]
pub struct SkyObjectsAbManifest {
    pub schema: &'static str,
    pub requested_collection: String,
    pub stats: SkyObjectsCaptureStats,
    pub domain_delta: Option<SkyObjectDomainDelta>,
    pub before_sky_objects: Vec<SurfaceProvenance>,
    pub after_sky_objects: Vec<SurfaceProvenance>,
}

#[derive(Default)]
struct SkyObjectsCaptureState {
    active: bool,
    draw_indexed_calls: usize,
    decals_additive_submission_reached: bool,
    decals_additive_draw_indexed_calls: usize,
    transparents_submission_reached: bool,
    transparents_draw_indexed_calls: usize,
    maps: BTreeMap<TagHash, ()>,
    collections: BTreeMap<TagHash, ()>,
    models: BTreeMap<TagHash, ()>,
}

static SKY_OBJECTS_CAPTURE: LazyLock<Mutex<SkyObjectsCaptureState>> =
    LazyLock::new(|| Mutex::new(SkyObjectsCaptureState::default()));

pub fn begin_shadowkeep_sky_objects_capture() {
    *SKY_OBJECTS_CAPTURE.lock() = SkyObjectsCaptureState {
        active: true,
        ..Default::default()
    };
}

pub fn record_shadowkeep_sky_objects_submission(stage: alkahest_data::tfx::RenderStage) {
    let mut capture = SKY_OBJECTS_CAPTURE.lock();
    if !capture.active {
        return;
    }
    match stage {
        alkahest_data::tfx::RenderStage::DecalsAdditive => {
            capture.decals_additive_submission_reached = true;
        }
        alkahest_data::tfx::RenderStage::Transparents => {
            capture.transparents_submission_reached = true;
        }
        _ => {}
    }
}

pub fn record_shadowkeep_sky_object_draw(
    stage: alkahest_data::tfx::RenderStage,
    map: TagHash,
    collection: TagHash,
    model: TagHash,
) {
    let mut capture = SKY_OBJECTS_CAPTURE.lock();
    if !capture.active {
        return;
    }
    capture.draw_indexed_calls += 1;
    match stage {
        alkahest_data::tfx::RenderStage::DecalsAdditive => {
            capture.decals_additive_draw_indexed_calls += 1;
        }
        alkahest_data::tfx::RenderStage::Transparents => {
            capture.transparents_draw_indexed_calls += 1;
        }
        _ => {}
    }
    capture.maps.insert(map, ());
    capture.collections.insert(collection, ());
    capture.models.insert(model, ());
}

pub fn finish_shadowkeep_sky_objects_capture() -> SkyObjectsCaptureStats {
    let mut capture = SKY_OBJECTS_CAPTURE.lock();
    capture.active = false;
    SkyObjectsCaptureStats {
        draw_indexed_calls: capture.draw_indexed_calls,
        decals_additive_submission_reached: capture.decals_additive_submission_reached,
        decals_additive_draw_indexed_calls: capture.decals_additive_draw_indexed_calls,
        transparents_submission_reached: capture.transparents_submission_reached,
        transparents_draw_indexed_calls: capture.transparents_draw_indexed_calls,
        maps: capture.maps.keys().map(ToString::to_string).collect(),
        collections: capture
            .collections
            .keys()
            .map(ToString::to_string)
            .collect(),
        models: capture.models.keys().map(ToString::to_string).collect(),
    }
}

static SKY_TECHNIQUE_DEPENDENCIES: LazyLock<
    Mutex<BTreeMap<(TagHash, TagHash), BTreeMap<(TagHash, TagHash), SkyTechniqueDependency>>>,
> = LazyLock::new(|| Mutex::new(BTreeMap::new()));

pub fn record_shadowkeep_sky_technique_dependency(
    map: TagHash,
    collection: TagHash,
    model: TagHash,
    technique: &Technique,
    object_channels: &AHashMap<u32, ObjectChannel>,
) {
    if !ConVars::get_flag("render.shadowkeep_sky_diagnostics") {
        return;
    }

    let renderer = Renderer::instance();
    let externs = renderer.externs.get();
    let global_channel_hashes = &renderer.globals.channels.channel_ids;
    let global_channel_values = &externs.globals;
    let legacy_technique = package_manager()
        .read_tag_struct::<SShadowkeepTechnique>(technique.hash)
        .ok();
    let stages = technique
        .all_stages()
        .into_iter()
        .filter_map(|(_, stage)| stage)
        .map(|stage| {
            let bytecode = &stage.dynamic_constants.bytecode;
            let mut global_channel_reads = OpcodeIterator::new(bytecode)
                .filter_map(|(opcode, args)| {
                    (opcode == Opcode::PushGlobalChannelVector)
                        .then(|| args.first().copied())
                        .flatten()
                })
                .map(|index| SkyTechniqueGlobalChannelRead {
                    index,
                    resolved_hash: global_channel_hashes
                        .get(index as usize)
                        .map(|hash| format!("0x{hash:08X}")),
                    value: global_channel_values
                        .get(index as usize)
                        .map(|value| value.to_array().map(|component| format!("{component:?}"))),
                })
                .collect::<Vec<_>>();
            global_channel_reads.sort_by_key(|read| read.index);
            global_channel_reads.dedup_by_key(|read| read.index);
            let textures = legacy_technique
                .as_ref()
                .and_then(|technique| {
                    technique
                        .shaders()
                        .into_iter()
                        .map(|(shader, _)| shader)
                        .find(|shader| shader.shader == stage.shader.shader)
                })
                .into_iter()
                .flat_map(|shader| &shader.textures)
                .map(|texture| SkyTechniqueTextureRead {
                    slot: texture.slot,
                    texture: texture.texture.to_string(),
                    loaded: stage
                        .dynamic_constants
                        .textures
                        .iter()
                        .find(|(slot, _)| *slot == texture.slot)
                        .and_then(|(_, handle)| handle.as_ref())
                        .is_some_and(|handle| handle.is_loaded()),
                })
                .collect::<Vec<_>>();

            let mut object_channel_reads = OpcodeIterator::new(bytecode)
                .filter_map(|(opcode, args)| {
                    if opcode != Opcode::PushObjectChannelVector || args.len() < 4 {
                        return None;
                    }
                    let hash = u32::from_be_bytes(args[..4].try_into().ok()?);
                    Some(SkyTechniqueObjectChannelRead {
                        hash: format!("0x{hash:08X}"),
                        supplied_by_model: object_channels.contains_key(&hash),
                    })
                })
                .collect::<Vec<_>>();
            object_channel_reads.sort_by(|left, right| left.hash.cmp(&right.hash));
            object_channel_reads.dedup_by(|left, right| left.hash == right.hash);

            let mut extern_reads = OpcodeIterator::new(bytecode)
                .filter_map(|(opcode, args)| {
                    let (value_type, scalar_width) = match opcode {
                        Opcode::PushExternInputFloat => ("float", 4),
                        Opcode::PushExternInputVec4 => ("vec4", 16),
                        Opcode::PushExternInputMat4 => ("mat4", 16),
                        Opcode::PushExternInputTextureView => ("texture_view", 8),
                        Opcode::PushExternInputU32 => ("u32", 4),
                        Opcode::PushExternInputUav => ("uav", 8),
                        _ => return None,
                    };
                    let raw_index = args.first().copied()?;
                    let byte_offset = u32::from(args.get(1).copied().unwrap_or_default())
                        .saturating_mul(scalar_width);
                    let extern_index = ExternIndex::try_from(raw_index)
                        .map(|index| format!("{index:?}"))
                        .unwrap_or_else(|_| format!("0x{raw_index:02X}"));
                    Some(GlobalLightingExternRead {
                        extern_index,
                        value_type: value_type.to_owned(),
                        byte_offset,
                    })
                })
                .collect::<Vec<_>>();
            extern_reads.sort_by(|left, right| {
                left.extern_index
                    .cmp(&right.extern_index)
                    .then(left.byte_offset.cmp(&right.byte_offset))
                    .then(left.value_type.cmp(&right.value_type))
            });
            extern_reads.dedup_by(|left, right| left == right);

            let expression_slots = |opcode| {
                OpcodeIterator::new(bytecode)
                    .filter_map(|(candidate, args)| {
                        (candidate == opcode)
                            .then(|| args.first().map(|encoded| encoded & 0x1f))
                            .flatten()
                    })
                    .collect::<Vec<_>>()
            };
            let mut sampler_slots = expression_slots(Opcode::PopSamplerState)
                .into_iter()
                .map(usize::from)
                .chain(
                    stage
                        .dynamic_constants
                        .samplers
                        .iter()
                        .enumerate()
                        .filter_map(|(slot, sampler)| sampler.is_some().then_some(slot)),
                )
                .collect::<Vec<_>>();
            sampler_slots.sort_unstable();
            sampler_slots.dedup();
            let mut texture_slots = expression_slots(Opcode::PopTextureView)
                .into_iter()
                .map(u32::from)
                .chain(
                    stage
                        .dynamic_constants
                        .textures
                        .iter()
                        .map(|(slot, _)| *slot),
                )
                .collect::<Vec<_>>();
            texture_slots.sort_unstable();
            texture_slots.dedup();

            SkyTechniqueDependencyStage {
                stage: stage.stage.short_name().to_owned(),
                shader: stage.shader.shader.to_string(),
                translated_expression_disassembly: expression_vm::disassemble(bytecode)
                    .map(|lines| lines.join("\n"))
                    .unwrap_or_else(|error| format!("ERROR: {error:#}")),
                global_channel_reads,
                object_channel_reads,
                extern_reads,
                constant_buffer_slot: stage.dynamic_constants.cbuffer_slot,
                constant_buffer_len: stage.dynamic_constants.constant_buffer_len(),
                sampler_slots,
                texture_slots,
                expression_evaluation_result: format!(
                    "{:?}",
                    stage.dynamic_constants.expression_evaluation_result()
                ),
                textures,
            }
        })
        .collect();
    let dependency = SkyTechniqueDependency {
        collection: collection.to_string(),
        model: model.to_string(),
        technique: technique.hash.to_string(),
        stages,
    };

    let mut all_dependencies = SKY_TECHNIQUE_DEPENDENCIES.lock();
    let dependencies = all_dependencies.entry((map, collection)).or_default();
    let key = (model, technique.hash);
    if dependencies.get(&key) == Some(&dependency) {
        return;
    }
    dependencies.insert(key, dependency);
    let manifest = SkyTechniqueDependencyManifest {
        schema: "alkahest-shadowkeep-sky-technique-dependencies/v1",
        map: map.to_string(),
        collection: collection.to_string(),
        techniques: dependencies.values().cloned().collect(),
    };
    if let Err(error) = (|| -> anyhow::Result<()> {
        fs::create_dir_all("artifacts")?;
        let path =
            format!("artifacts/shadowkeep-sky-technique-dependencies-{map}-{collection}.json");
        fs::write(path, serde_json::to_vec_pretty(&manifest)?)?;
        Ok(())
    })() {
        tracing::error!(
            %map,
            %collection,
            error = ?error,
            "failed to write Shadowkeep sky technique dependencies"
        );
    }
}

#[derive(Debug, Serialize)]
pub struct FinalCombineProvenance {
    pub technique: String,
    pub vertex_shader: Option<String>,
    pub pixel_shader: Option<String>,
    pub draw_6_reached: bool,
    pub vertex_expression: Option<String>,
    pub pixel_expression: Option<String>,
    pub vertex_constant_buffer_slot: Option<u32>,
    pub vertex_constant_buffer_len: Option<usize>,
    pub pixel_constant_buffer_slot: Option<u32>,
    pub pixel_constant_buffer_len: Option<usize>,
    pub bound_input_srv: String,
    pub output_rtv_format: String,
}

#[derive(Debug, Serialize)]
pub struct GlobalLightingExternValue {
    pub byte_offset: u32,
    pub value: [f32; 4],
}

#[derive(Debug, Serialize)]
pub struct GlobalLightingAbManifest {
    pub schema: &'static str,
    pub global_lighting_enabled: bool,
    pub global_lighting_draw_6_reached: bool,
    pub global_lighting_stage_status: Vec<String>,
    pub positional_channel_values: Vec<GlobalLightingChannelValue>,
    pub extern_values: Vec<GlobalLightingExternValue>,
    pub before_global_lighting: Vec<SurfaceProvenance>,
    pub after_global_lighting: Vec<SurfaceProvenance>,
    pub after_final_shading: Vec<SurfaceProvenance>,
}

#[derive(Debug, Serialize)]
pub struct DirectionalLightVariantManifest {
    pub label: &'static str,
    pub direct_direction: [f32; 4],
    pub diffuse_direction: [f32; 4],
    pub before_global_lighting: Vec<SurfaceProvenance>,
    pub after_global_lighting: Vec<SurfaceProvenance>,
    pub after_final_shading: Vec<SurfaceProvenance>,
}

#[derive(Debug, Serialize)]
pub struct DirectionalLightAbManifest {
    pub schema: &'static str,
    pub selected_direction: [f32; 4],
    pub world_normal: Vec<SurfaceProvenance>,
    pub variants: Vec<DirectionalLightVariantManifest>,
}

#[derive(Debug, Serialize)]
pub struct FinalCombineManifest {
    pub schema: &'static str,
    pub final_combine: FinalCombineProvenance,
    pub capture: SurfaceProvenance,
}

#[derive(Debug, Serialize)]
pub struct ProvenanceManifest {
    pub schema: &'static str,
    pub deferred_shading: DeferredShadingProvenance,
    pub lighting: ShadowkeepLightingProvenance,
    pub captures: Vec<SurfaceProvenance>,
}

#[derive(Debug, Serialize)]
pub struct ExposureVariantProvenance {
    pub exposure_scale: f32,
    pub frame_scope_c1: [f32; 4],
    pub deferred_shading: DeferredShadingProvenance,
    pub captures: Vec<SurfaceProvenance>,
}

#[derive(Debug, Serialize)]
pub struct ExposureAbManifest {
    pub schema: &'static str,
    pub production_exposure_scale: f32,
    pub exposure_illum_relative: f32,
    pub variants: Vec<ExposureVariantProvenance>,
}

impl ExposureAbManifest {
    pub fn write(&self, directory: &Path) -> anyhow::Result<()> {
        let json =
            serde_json::to_vec_pretty(self).context("Failed to serialize exposure A/B manifest")?;
        fs::write(directory.join("manifest.json"), json)
            .context("Failed to write exposure A/B manifest")
    }
}

impl FinalCombineManifest {
    pub fn write(&self, directory: &Path) -> anyhow::Result<()> {
        let json = serde_json::to_vec_pretty(self)
            .context("Failed to serialize final-combine manifest")?;
        fs::write(directory.join("manifest.json"), json)
            .context("Failed to write final-combine manifest")
    }
}

impl ProvenanceManifest {
    pub fn write(&self, directory: &Path) -> anyhow::Result<()> {
        let json =
            serde_json::to_vec_pretty(self).context("Failed to serialize provenance manifest")?;
        fs::write(directory.join("manifest.json"), json)
            .context("Failed to write provenance manifest")
    }
}

impl SkyObjectsAbManifest {
    pub fn write(&self, directory: &Path) -> anyhow::Result<()> {
        fs::create_dir_all(directory).context("Failed to create sky-object A/B directory")?;
        let json = serde_json::to_vec_pretty(self)
            .context("Failed to serialize sky-object A/B manifest")?;
        fs::write(directory.join("manifest.json"), json)
            .context("Failed to write sky-object A/B manifest")
    }
}

/// Calculates the sky-object A/B delta in depth domains without requesting
/// another GPU readback. Both captures already contain tightly packed raw
/// surfaces because the diagnostic is explicitly armed.
pub fn sky_object_domain_delta(
    before_directory: &Path,
    before: &[SurfaceProvenance],
    after_directory: &Path,
    after: &[SurfaceProvenance],
) -> anyhow::Result<SkyObjectDomainDelta> {
    fn find_capture<'a>(
        captures: &'a [SurfaceProvenance],
        surface: &str,
    ) -> anyhow::Result<&'a SurfaceProvenance> {
        captures
            .iter()
            .find(|capture| capture.surface == surface)
            .with_context(|| format!("sky-object A/B capture omitted {surface}"))
    }

    let before_color = find_capture(before, "shading_result")?;
    let after_color = find_capture(after, "shading_result")?;
    let before_depth = find_capture(before, "gbuffer_depth")?;
    let after_depth = find_capture(after, "gbuffer_depth")?;
    anyhow::ensure!(
        before_color.width == after_color.width && before_color.height == after_color.height,
        "sky-object color capture dimensions changed during A/B"
    );
    anyhow::ensure!(
        before_depth.width == after_depth.width && before_depth.height == after_depth.height,
        "sky-object depth capture dimensions changed during A/B"
    );
    anyhow::ensure!(
        before_color.width == before_depth.width && before_color.height == before_depth.height,
        "sky-object color and depth captures have different dimensions"
    );

    let before_color_bytes = fs::read(before_directory.join(&before_color.file))
        .context("failed to read pre-sky shading capture")?;
    let after_color_bytes = fs::read(after_directory.join(&after_color.file))
        .context("failed to read post-sky shading capture")?;
    let before_depth_bytes = fs::read(before_directory.join(&before_depth.file))
        .context("failed to read pre-sky depth capture")?;
    let after_depth_bytes = fs::read(after_directory.join(&after_depth.file))
        .context("failed to read post-sky depth capture")?;
    let color_format = provenance_format(&before_color.resource_format)?;
    let depth_format = provenance_format(&before_depth.resource_format)?;
    anyhow::ensure!(
        color_format == provenance_format(&after_color.resource_format)?,
        "sky-object color capture format changed during A/B"
    );
    anyhow::ensure!(
        depth_format == provenance_format(&after_depth.resource_format)?,
        "sky-object depth capture format changed during A/B"
    );

    let color_bpp = bytes_per_pixel(color_format)?;
    let depth_bpp = bytes_per_pixel(depth_format)?;
    let pixel_count = before_color.width as usize * before_color.height as usize;
    anyhow::ensure!(
        before_color_bytes.len() == pixel_count * color_bpp
            && after_color_bytes.len() == pixel_count * color_bpp
            && before_depth_bytes.len() == pixel_count * depth_bpp
            && after_depth_bytes.len() == pixel_count * depth_bpp,
        "sky-object A/B capture byte count does not match its dimensions"
    );

    let clear_depth = if depth_pixel_count(depth_format, &before_depth_bytes, 0.0)?
        >= depth_pixel_count(depth_format, &before_depth_bytes, 1.0)?
    {
        0.0
    } else {
        1.0
    };
    let mut delta = SkyObjectDomainDelta {
        gbuffer_depth_byte_identical: before_depth_bytes == after_depth_bytes,
        ..Default::default()
    };
    let mut clear_squared = [0.0; 3];
    let mut geometry_squared = [0.0; 3];
    let mut clear_abs = [0.0; 3];
    let mut geometry_abs = [0.0; 3];
    for index in 0..pixel_count {
        let depth = decode_pixel(
            depth_format,
            &before_depth_bytes[index * depth_bpp..(index + 1) * depth_bpp],
        )?[0];
        let clear = (depth - clear_depth).abs() <= f32::EPSILON;
        let before_pixel = decode_pixel(
            color_format,
            &before_color_bytes[index * color_bpp..(index + 1) * color_bpp],
        )?;
        let after_pixel = decode_pixel(
            color_format,
            &after_color_bytes[index * color_bpp..(index + 1) * color_bpp],
        )?;
        let mut changed = false;
        for channel in 0..3 {
            let value = f64::from(after_pixel[channel] - before_pixel[channel]).abs();
            changed |= value != 0.0;
            if clear {
                clear_squared[channel] += value * value;
                clear_abs[channel] += value;
            } else {
                geometry_squared[channel] += value * value;
                geometry_abs[channel] += value;
            }
        }
        if clear {
            delta.clear_depth_pixels += 1;
            delta.changed_clear_depth_pixels += usize::from(changed);
        } else {
            delta.geometry_pixels += 1;
            delta.changed_geometry_pixels += usize::from(changed);
        }
    }
    for channel in 0..3 {
        if delta.clear_depth_pixels != 0 {
            let count = delta.clear_depth_pixels as f64;
            delta.clear_depth_rmse_rgb[channel] = (clear_squared[channel] / count).sqrt();
            delta.clear_depth_mean_abs_delta_rgb[channel] = clear_abs[channel] / count;
        }
        if delta.geometry_pixels != 0 {
            let count = delta.geometry_pixels as f64;
            delta.geometry_rmse_rgb[channel] = (geometry_squared[channel] / count).sqrt();
            delta.geometry_mean_abs_delta_rgb[channel] = geometry_abs[channel] / count;
        }
    }
    Ok(delta)
}

fn depth_pixel_count(format: dxgi::Format, bytes: &[u8], value: f32) -> anyhow::Result<usize> {
    let bytes_per_pixel = bytes_per_pixel(format)?;
    bytes
        .chunks_exact(bytes_per_pixel)
        .map(|pixel| decode_pixel(format, pixel).map(|decoded| decoded[0]))
        .collect::<anyhow::Result<Vec<_>>>()
        .map(|pixels| {
            pixels
                .into_iter()
                .filter(|pixel| (*pixel - value).abs() <= f32::EPSILON)
                .count()
        })
}

fn provenance_format(name: &str) -> anyhow::Result<dxgi::Format> {
    match name {
        "R8g8Typeless" => Ok(dxgi::Format::R8g8Typeless),
        "R8g8Unorm" => Ok(dxgi::Format::R8g8Unorm),
        "R8g8b8a8Typeless" => Ok(dxgi::Format::R8g8b8a8Typeless),
        "R8g8b8a8Unorm" => Ok(dxgi::Format::R8g8b8a8Unorm),
        "R8g8b8a8UnormSrgb" => Ok(dxgi::Format::R8g8b8a8UnormSrgb),
        "R10g10b10a2Typeless" => Ok(dxgi::Format::R10g10b10a2Typeless),
        "R10g10b10a2Unorm" => Ok(dxgi::Format::R10g10b10a2Unorm),
        "R11g11b10Float" => Ok(dxgi::Format::R11g11b10Float),
        "R16g16b16a16Typeless" => Ok(dxgi::Format::R16g16b16a16Typeless),
        "R16g16b16a16Float" => Ok(dxgi::Format::R16g16b16a16Float),
        "R32Typeless" => Ok(dxgi::Format::R32Typeless),
        "R32g8x24Typeless" => Ok(dxgi::Format::R32g8x24Typeless),
        _ => bail!("unsupported sky-object A/B provenance format {name}"),
    }
}

impl GlobalLightingDependencyManifest {
    pub fn write(&self, directory: &Path) -> anyhow::Result<()> {
        fs::create_dir_all(directory)
            .context("Failed to create global-lighting manifest directory")?;
        let json = serde_json::to_vec_pretty(self)
            .context("Failed to serialize global-lighting dependency manifest")?;
        fs::write(directory.join("manifest.json"), json)
            .context("Failed to write global-lighting dependency manifest")
    }
}

impl GlobalLightingAbManifest {
    pub fn write(&self, directory: &Path) -> anyhow::Result<()> {
        fs::create_dir_all(directory).context("Failed to create global-lighting A/B directory")?;
        let json = serde_json::to_vec_pretty(self)
            .context("Failed to serialize global-lighting A/B manifest")?;
        fs::write(directory.join("manifest.json"), json)
            .context("Failed to write global-lighting A/B manifest")
    }
}

impl DirectionalLightAbManifest {
    pub fn write(&self, directory: &Path) -> anyhow::Result<()> {
        fs::create_dir_all(directory)
            .context("Failed to create directional-light A/B directory")?;
        let json = serde_json::to_vec_pretty(self)
            .context("Failed to serialize directional-light A/B manifest")?;
        fs::write(directory.join("manifest.json"), json)
            .context("Failed to write directional-light A/B manifest")
    }
}

pub fn capture_surface(
    cmd: &CommandList,
    surface: &Surface,
    directory: &Path,
    clear_value: Option<[f32; 4]>,
    statistics_encoding: Option<&'static str>,
) -> anyhow::Result<SurfaceProvenance> {
    capture_surface_named(
        cmd,
        surface,
        directory,
        surface.name(),
        clear_value,
        statistics_encoding,
    )
}

pub fn capture_surface_named(
    cmd: &CommandList,
    surface: &Surface,
    directory: &Path,
    name: &str,
    clear_value: Option<[f32; 4]>,
    statistics_encoding: Option<&'static str>,
) -> anyhow::Result<SurfaceProvenance> {
    fs::create_dir_all(directory).context("Failed to create provenance directory")?;

    let desc = surface.texture.get_desc();
    let format = desc.format;
    let bytes_per_pixel = bytes_per_pixel(format)?;
    let staging = cmd.gpu().device.create_texture2d(
        &Texture2dDesc::builder()
            .width(desc.width)
            .height(desc.height)
            .mip_levels(1)
            .array_size(1)
            .format(format)
            .usage(d3d11::Usage::Staging)
            .bind_flags(BindFlags::empty())
            .cpu_access_flags(CpuAccessFlags::READ)
            .build(),
        None,
    )?;
    cmd.copy_resource(&surface.texture, &staging);

    let map = cmd
        .map(&staging, 0, d3d11::MapType::Read, false)
        .context("Failed to map provenance staging texture")?;
    let tight_row_pitch = desc.width as usize * bytes_per_pixel;
    let mut bytes = Vec::with_capacity(tight_row_pitch * desc.height as usize);
    for y in 0..desc.height as usize {
        let row = unsafe {
            std::slice::from_raw_parts(
                map.data.cast::<u8>().add(y * map.row_pitch as usize),
                tight_row_pitch,
            )
        };
        bytes.extend_from_slice(row);
    }
    drop(map);

    let filename = format!("{name}.bin");
    fs::write(directory.join(&filename), &bytes)
        .with_context(|| format!("Failed to write provenance capture {filename}"))?;
    let stats = compute_stats(format, &bytes, clear_value)?;

    Ok(SurfaceProvenance {
        surface: name.to_owned(),
        file: filename,
        format: format!(
            "{:?}",
            surface
                .desc()
                .depth_format
                .unwrap_or(surface.desc().view_format)
        ),
        resource_format: format!("{format:?}"),
        width: desc.width,
        height: desc.height,
        statistics_encoding: statistics_encoding.unwrap_or_else(|| {
            if surface.desc().view_format == dxgi::Format::R8g8b8a8UnormSrgb {
                "srgb_encoded_raw"
            } else {
                "linear"
            }
        }),
        finite_pixel_count: stats.finite_pixel_count,
        non_finite_pixel_count: stats.non_finite_pixel_count,
        clipped_or_saturated_pixel_count: stats.clipped_or_saturated_pixel_count,
        nonzero_rgb_pixel_count: stats.nonzero_rgb_pixel_count,
        minimum_rgb: stats.minimum_rgb,
        maximum_rgb: stats.maximum_rgb,
        mean_rgb: stats.mean_rgb,
        nonzero_alpha_count: stats.nonzero_alpha_count,
        pixels_different_from_clear_value: stats.pixels_different_from_clear_value,
        sha256: format!("{:x}", Sha256::digest(&bytes)),
    })
}

#[derive(Debug, PartialEq)]
struct PixelStats {
    finite_pixel_count: u64,
    non_finite_pixel_count: u64,
    clipped_or_saturated_pixel_count: u64,
    nonzero_rgb_pixel_count: u64,
    minimum_rgb: Option<[f64; 3]>,
    maximum_rgb: Option<[f64; 3]>,
    mean_rgb: Option<[f64; 3]>,
    nonzero_alpha_count: Option<u64>,
    pixels_different_from_clear_value: Option<u64>,
}

fn compute_stats(
    format: dxgi::Format,
    bytes: &[u8],
    clear_value: Option<[f32; 4]>,
) -> anyhow::Result<PixelStats> {
    let bytes_per_pixel = bytes_per_pixel(format)?;
    if !bytes.len().is_multiple_of(bytes_per_pixel) {
        bail!("Capture byte length is not aligned to format {format:?}");
    }

    let quantized_clear = clear_value
        .map(|clear| quantize_clear_rgb(format, clear))
        .transpose()?;
    let has_alpha = format_has_alpha(format);
    let mut finite_pixel_count = 0u64;
    let mut non_finite_pixel_count = 0u64;
    let mut clipped_or_saturated_pixel_count = 0u64;
    let mut nonzero_rgb_pixel_count = 0u64;
    let mut nonzero_alpha_count = 0u64;
    let mut minimum_rgb = [f64::INFINITY; 3];
    let mut maximum_rgb = [f64::NEG_INFINITY; 3];
    let mut sum_rgb = [0.0f64; 3];
    let mut pixels_different_from_clear_value = 0u64;

    for encoded in bytes.chunks_exact(bytes_per_pixel) {
        let [r, g, b, a] = decode_pixel(format, encoded)?;
        if quantized_clear.is_some_and(|clear| [r, g, b] != clear) {
            pixels_different_from_clear_value += 1;
        }
        if (r.is_finite() && r != 0.0) || (g.is_finite() && g != 0.0) || (b.is_finite() && b != 0.0)
        {
            nonzero_rgb_pixel_count += 1;
        }
        if has_alpha && a.is_finite() && a != 0.0 {
            nonzero_alpha_count += 1;
        }
        if [r, g, b]
            .into_iter()
            .any(|value| !value.is_finite() || value >= 1.0)
        {
            clipped_or_saturated_pixel_count += 1;
        }
        if r.is_finite() && g.is_finite() && b.is_finite() {
            finite_pixel_count += 1;
            for (index, value) in [r, g, b].into_iter().enumerate() {
                let value = value as f64;
                minimum_rgb[index] = minimum_rgb[index].min(value);
                maximum_rgb[index] = maximum_rgb[index].max(value);
                sum_rgb[index] += value;
            }
        } else {
            non_finite_pixel_count += 1;
        }
    }

    let (minimum_rgb, maximum_rgb, mean_rgb) = if finite_pixel_count == 0 {
        (None, None, None)
    } else {
        (
            Some(minimum_rgb),
            Some(maximum_rgb),
            Some(sum_rgb.map(|sum| sum / finite_pixel_count as f64)),
        )
    };

    Ok(PixelStats {
        finite_pixel_count,
        non_finite_pixel_count,
        clipped_or_saturated_pixel_count,
        nonzero_rgb_pixel_count,
        minimum_rgb,
        maximum_rgb,
        mean_rgb,
        nonzero_alpha_count: has_alpha.then_some(nonzero_alpha_count),
        pixels_different_from_clear_value: quantized_clear
            .map(|_| pixels_different_from_clear_value),
    })
}

fn quantize_clear_rgb(format: dxgi::Format, clear: [f32; 4]) -> anyhow::Result<[f32; 3]> {
    match format {
        dxgi::Format::R11g11b10Float => Ok([
            decode_unsigned_float(encode_unsigned_float(clear[0], 6), 6),
            decode_unsigned_float(encode_unsigned_float(clear[1], 6), 6),
            decode_unsigned_float(encode_unsigned_float(clear[2], 5), 5),
        ]),
        dxgi::Format::R8g8Typeless | dxgi::Format::R8g8Unorm => Ok([
            (clear[0].clamp(0.0, 1.0) * 255.0).round() / 255.0,
            (clear[1].clamp(0.0, 1.0) * 255.0).round() / 255.0,
            0.0,
        ]),
        _ => bail!("Clear-value statistics are unsupported for format {format:?}"),
    }
}

fn encode_unsigned_float(value: f32, mantissa_bits: u32) -> u32 {
    if !value.is_finite() {
        return if value.is_nan() {
            (31 << mantissa_bits) | 1
        } else {
            31 << mantissa_bits
        };
    }
    if value <= 0.0 {
        return 0;
    }

    let mantissa_scale = (1u32 << mantissa_bits) as f32;
    let exponent = value.log2().floor() as i32;
    if exponent < -14 {
        return (value * 2f32.powi(14) * mantissa_scale).round() as u32;
    }
    if exponent > 15 {
        return 31 << mantissa_bits;
    }

    let mut biased_exponent = (exponent + 15) as u32;
    let mut mantissa = ((value / 2f32.powi(exponent) - 1.0) * mantissa_scale).round() as u32;
    if mantissa == 1 << mantissa_bits {
        biased_exponent += 1;
        mantissa = 0;
    }
    if biased_exponent >= 31 {
        31 << mantissa_bits
    } else {
        (biased_exponent << mantissa_bits) | mantissa
    }
}
fn bytes_per_pixel(format: dxgi::Format) -> anyhow::Result<usize> {
    match format {
        dxgi::Format::R8g8b8a8Typeless
        | dxgi::Format::R8g8b8a8Unorm
        | dxgi::Format::R8g8b8a8UnormSrgb
        | dxgi::Format::R10g10b10a2Typeless
        | dxgi::Format::R10g10b10a2Unorm
        | dxgi::Format::R11g11b10Float
        | dxgi::Format::R32Typeless => Ok(4),
        dxgi::Format::R16g16b16a16Typeless | dxgi::Format::R16g16b16a16Float => Ok(8),
        dxgi::Format::R8g8Typeless | dxgi::Format::R8g8Unorm => Ok(2),
        dxgi::Format::R32g8x24Typeless => Ok(8),
        _ => bail!("Unsupported provenance format {format:?}"),
    }
}

fn format_has_alpha(format: dxgi::Format) -> bool {
    matches!(
        format,
        dxgi::Format::R8g8b8a8Typeless
            | dxgi::Format::R8g8b8a8Unorm
            | dxgi::Format::R8g8b8a8UnormSrgb
            | dxgi::Format::R10g10b10a2Typeless
            | dxgi::Format::R10g10b10a2Unorm
            | dxgi::Format::R16g16b16a16Typeless
            | dxgi::Format::R16g16b16a16Float
    )
}

fn decode_pixel(format: dxgi::Format, bytes: &[u8]) -> anyhow::Result<[f32; 4]> {
    let packed = || u32::from_le_bytes(bytes[..4].try_into().unwrap());
    match format {
        dxgi::Format::R8g8Typeless | dxgi::Format::R8g8Unorm => {
            Ok([bytes[0] as f32 / 255.0, bytes[1] as f32 / 255.0, 0.0, 0.0])
        }
        dxgi::Format::R8g8b8a8Typeless
        | dxgi::Format::R8g8b8a8Unorm
        | dxgi::Format::R8g8b8a8UnormSrgb => Ok([
            bytes[0] as f32 / 255.0,
            bytes[1] as f32 / 255.0,
            bytes[2] as f32 / 255.0,
            bytes[3] as f32 / 255.0,
        ]),
        dxgi::Format::R10g10b10a2Typeless | dxgi::Format::R10g10b10a2Unorm => {
            let value = packed();
            Ok([
                (value & 0x3ff) as f32 / 1023.0,
                ((value >> 10) & 0x3ff) as f32 / 1023.0,
                ((value >> 20) & 0x3ff) as f32 / 1023.0,
                ((value >> 30) & 0x3) as f32 / 3.0,
            ])
        }
        dxgi::Format::R11g11b10Float => {
            let value = packed();
            Ok([
                decode_unsigned_float(value & 0x7ff, 6),
                decode_unsigned_float((value >> 11) & 0x7ff, 6),
                decode_unsigned_float((value >> 22) & 0x3ff, 5),
                0.0,
            ])
        }
        dxgi::Format::R16g16b16a16Typeless | dxgi::Format::R16g16b16a16Float => Ok([
            decode_f16(u16::from_le_bytes(bytes[0..2].try_into().unwrap())),
            decode_f16(u16::from_le_bytes(bytes[2..4].try_into().unwrap())),
            decode_f16(u16::from_le_bytes(bytes[4..6].try_into().unwrap())),
            decode_f16(u16::from_le_bytes(bytes[6..8].try_into().unwrap())),
        ]),
        dxgi::Format::R32Typeless => {
            let depth = f32::from_le_bytes(bytes[..4].try_into().unwrap());
            Ok([depth, depth, depth, 0.0])
        }
        dxgi::Format::R32g8x24Typeless => {
            let depth = f32::from_le_bytes(bytes[..4].try_into().unwrap());
            Ok([depth, depth, depth, 0.0])
        }
        _ => bail!("Unsupported provenance format {format:?}"),
    }
}

fn decode_unsigned_float(bits: u32, mantissa_bits: u32) -> f32 {
    let mantissa_mask = (1 << mantissa_bits) - 1;
    let exponent = bits >> mantissa_bits;
    let mantissa = bits & mantissa_mask;
    match exponent {
        0 if mantissa == 0 => 0.0,
        0 => (mantissa as f32 / (1 << mantissa_bits) as f32) * 2f32.powi(-14),
        31 if mantissa == 0 => f32::INFINITY,
        31 => f32::NAN,
        _ => {
            (1.0 + mantissa as f32 / (1 << mantissa_bits) as f32) * 2f32.powi(exponent as i32 - 15)
        }
    }
}

fn decode_f16(bits: u16) -> f32 {
    let sign = ((bits >> 15) as u32) << 31;
    let exponent = ((bits >> 10) & 0x1f) as u32;
    let mantissa = (bits & 0x3ff) as u32;
    let converted = match exponent {
        0 if mantissa == 0 => sign,
        0 => {
            let shift = mantissa.leading_zeros() - 21;
            let normalized = (mantissa << shift) & 0x3ff;
            sign | ((113 - shift) << 23) | (normalized << 13)
        }
        31 => sign | 0x7f80_0000 | (mantissa << 13),
        _ => sign | ((exponent + 112) << 23) | (mantissa << 13),
    };
    f32::from_bits(converted)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn computes_unorm_statistics_and_alpha_count() {
        let stats = compute_stats(
            dxgi::Format::R8g8b8a8Unorm,
            &[0, 0, 0, 0, 255, 128, 0, 255],
            None,
        )
        .unwrap();
        assert_eq!(stats.finite_pixel_count, 2);
        assert_eq!(stats.nonzero_rgb_pixel_count, 1);
        assert_eq!(stats.minimum_rgb, Some([0.0; 3]));
        assert_eq!(
            stats.maximum_rgb,
            Some([1.0, (128.0f32 / 255.0) as f64, 0.0])
        );
        assert_eq!(stats.nonzero_alpha_count, Some(1));
        assert_eq!(stats.pixels_different_from_clear_value, None);
    }

    #[test]
    fn decodes_half_float_alpha() {
        let pixel = decode_pixel(
            dxgi::Format::R16g16b16a16Float,
            &[0x00, 0x3c, 0x00, 0xc0, 0x00, 0x00, 0x00, 0x38],
        )
        .unwrap();
        assert_eq!(pixel, [1.0, -2.0, 0.0, 0.5]);
    }

    #[test]
    fn decodes_r11g11b10_one() {
        let one_r11 = 15u32 << 6;
        let one_g11 = (15u32 << 6) << 11;
        let one_b10 = (15u32 << 5) << 22;
        let pixel = decode_pixel(
            dxgi::Format::R11g11b10Float,
            &(one_r11 | one_g11 | one_b10).to_le_bytes(),
        )
        .unwrap();
        assert_eq!(pixel, [1.0, 1.0, 1.0, 0.0]);
    }

    #[test]
    fn counts_pixels_different_from_r11_clear_value() {
        let clear = [
            encode_unsigned_float(0.001, 6),
            encode_unsigned_float(0.001, 6),
            encode_unsigned_float(0.001, 5),
        ];
        let packed_clear = clear[0] | (clear[1] << 11) | (clear[2] << 22);
        let brighter = encode_unsigned_float(0.25, 6) | (clear[1] << 11) | (clear[2] << 22);
        let bytes = [packed_clear.to_le_bytes(), brighter.to_le_bytes()].concat();

        let stats = compute_stats(
            dxgi::Format::R11g11b10Float,
            &bytes,
            Some([0.001, 0.001, 0.001, 0.0]),
        )
        .unwrap();

        assert_eq!(stats.pixels_different_from_clear_value, Some(1));
    }
    #[test]
    fn counts_pixels_different_from_r8g8_clear_value() {
        let stats = compute_stats(
            dxgi::Format::R8g8Typeless,
            &[255, 255, 128, 255],
            Some([1.0, 1.0, 1.0, 1.0]),
        )
        .unwrap();

        assert_eq!(stats.maximum_rgb, Some([1.0, 1.0, 0.0]));
        assert_eq!(stats.pixels_different_from_clear_value, Some(1));
    }

    #[test]
    fn counts_non_finite_and_saturated_rgb_pixels() {
        let stats = compute_stats(
            dxgi::Format::R16g16b16a16Float,
            &[
                0x00, 0x3c, 0x00, 0x3c, 0x00, 0x3c, 0x00, 0x3c, 0x00, 0x7c, 0x00, 0x3c, 0x00, 0x3c,
                0x00, 0x3c,
            ],
            None,
        )
        .unwrap();
        assert_eq!(stats.finite_pixel_count, 1);
        assert_eq!(stats.non_finite_pixel_count, 1);
        assert_eq!(stats.clipped_or_saturated_pixel_count, 2);
    }
}

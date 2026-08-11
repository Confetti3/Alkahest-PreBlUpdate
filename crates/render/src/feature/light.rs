use std::{
    collections::BTreeSet,
    sync::{
        Arc, LazyLock,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
};

use alkahest_core::job::{SCHEDULER, potassium::Priority};
use alkahest_data::tfx::{
    PipelineState, PrimitiveType, RenderStage, ShaderStage,
    common::AxisAlignedBBox,
    features::{
        dynamic::RenderStageSubscription,
        light::{SLight, SShadowingLight},
    },
};
use d3d11::dxgi;
use glam::{Mat4, Vec3, Vec4, Vec4Swizzles};
use itertools::Itertools;
use tiger_pkg::TagHash;

use super::FeatureRenderer;
use crate::{
    Renderer,
    renderer::visibility::OpaqueView,
    tfx::{
        externs::{self, DeferredLight, SimpleGeometry, VolumeFog},
        packet::CompactTransform,
        technique::Technique,
    },
    util::{geometry, threading::CommandListSetId},
};

static SHADOWKEEP_DEFERRED_LIGHT_DRAW_REPORTED: AtomicBool = AtomicBool::new(false);
static SHADOWKEEP_LIGHT_BINDING_REPORTED: AtomicBool = AtomicBool::new(false);
static SHADOWKEEP_LIGHT_CAPTURE_ACTIVE: AtomicBool = AtomicBool::new(false);
static SHADOWKEEP_LIGHT_CAPTURE_DRAWS: AtomicUsize = AtomicUsize::new(0);
static SHADOWKEEP_LIGHT_CAPTURE_TECHNIQUES: LazyLock<parking_lot::Mutex<BTreeSet<String>>> =
    LazyLock::new(|| parking_lot::Mutex::new(BTreeSet::new()));

#[derive(Debug)]
pub(crate) struct ShadowkeepLightCapture {
    pub draw_indexed_calls: usize,
    pub technique_hashes: Vec<String>,
}

pub(crate) fn begin_shadowkeep_light_capture() {
    SHADOWKEEP_LIGHT_CAPTURE_DRAWS.store(0, Ordering::Relaxed);
    SHADOWKEEP_LIGHT_CAPTURE_TECHNIQUES.lock().clear();
    SHADOWKEEP_LIGHT_CAPTURE_ACTIVE.store(true, Ordering::Release);
}

pub(crate) fn finish_shadowkeep_light_capture() -> ShadowkeepLightCapture {
    SHADOWKEEP_LIGHT_CAPTURE_ACTIVE.store(false, Ordering::Release);
    ShadowkeepLightCapture {
        draw_indexed_calls: SHADOWKEEP_LIGHT_CAPTURE_DRAWS.load(Ordering::Relaxed),
        technique_hashes: SHADOWKEEP_LIGHT_CAPTURE_TECHNIQUES
            .lock()
            .iter()
            .cloned()
            .collect(),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LightSubmissionPath {
    Current,
    Shadowkeep { shadowing: bool },
}

struct LightRendererData {
    technique_lighting_apply: Technique,
    technique_lighting_apply_shadowing: Option<Technique>,
    technique_volumetrics: Option<Technique>,
    technique_volumetrics_shadowing: Option<Technique>,
    // technique_light_probe_apply: Technique,

    // TODO(cohae): This should be a shared resource (eg. a struct in the renderer that we can use instead of recreating it for every light/cubemap)
    vb: d3d11::Buffer,
    ib: d3d11::Buffer,
}

pub struct LightRenderer {
    data: Arc<LightRendererData>,
    submission_path: LightSubmissionPath,

    local_to_world: glam::Mat4,
    light_space_transform: glam::Mat4,
    shadowmap_projection: glam::Mat4,
    bounds: Option<AxisAlignedBBox>,

    pub shadow_view: Option<(d3d11::Texture2D, d3d11::ShaderResourceView)>,
}

impl LightRenderer {
    /// Construct a light from the preserved Shadowkeep layout without
    /// normalizing it through a later-era `SLight` structure.
    pub fn new_shadowkeep(
        renderer: &Renderer,
        technique_shading: TagHash,
        technique_volumetrics: TagHash,
        light_space_transform: Mat4,
        bounds: Option<AxisAlignedBBox>,
    ) -> anyhow::Result<Box<Self>> {
        Self::new_impl(
            renderer,
            technique_shading,
            TagHash::NONE,
            technique_volumetrics,
            TagHash::NONE,
            light_space_transform,
            Mat4::IDENTITY,
            bounds,
            LightSubmissionPath::Shadowkeep { shadowing: false },
        )
    }

    /// Construct a preserved Shadowkeep shadowing light.  The shadow view is
    /// attached by the map loader when that optional resource is available.
    pub fn new_shadowkeep_shadowing(
        renderer: &Renderer,
        technique_shading: TagHash,
        technique_shading_shadowing: TagHash,
        technique_volumetrics: TagHash,
        technique_volumetrics_shadowing: TagHash,
        light_space_transform: Mat4,
        shadowmap_projection: Mat4,
    ) -> anyhow::Result<Box<Self>> {
        Self::new_impl(
            renderer,
            technique_shading,
            technique_shading_shadowing,
            technique_volumetrics,
            technique_volumetrics_shadowing,
            light_space_transform,
            shadowmap_projection,
            None,
            LightSubmissionPath::Shadowkeep { shadowing: true },
        )
    }

    pub fn new(
        renderer: &Renderer,
        light: &SLight,
        bounds: AxisAlignedBBox,
    ) -> anyhow::Result<Box<Self>> {
        Self::new_impl(
            renderer,
            light.technique_lighting_apply,
            TagHash::NONE,
            light.technique_volumetrics,
            TagHash::NONE,
            // light.technique_light_probe_apply,
            light.light_space_transform,
            Mat4::IDENTITY,
            Some(bounds),
            LightSubmissionPath::Current,
        )
    }

    pub fn new_shadowing(
        renderer: &Renderer,
        light: &SShadowingLight,
        shadowmap_projection: Mat4,
    ) -> anyhow::Result<Box<Self>> {
        Self::new_impl(
            renderer,
            light.technique_lighting_apply,
            light.technique_lighting_apply_shadowing,
            light.technique_volumetrics,
            light.technique_volumetrics_shadowing,
            light.light_space_transform,
            shadowmap_projection,
            None,
            LightSubmissionPath::Current,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn new_impl(
        renderer: &Renderer,
        technique_shading: TagHash,
        technique_shading_shadowing: TagHash,
        technique_volumetrics: TagHash,
        technique_volumetrics_shadowing: TagHash,
        // technique_light_probe: TagHash,
        light_space_transform: Mat4,
        shadowmap_projection: Mat4,
        bounds: Option<AxisAlignedBBox>,
        submission_path: LightSubmissionPath,
    ) -> anyhow::Result<Box<Self>> {
        let vb = renderer.gpu.create_buffer(
            &d3d11::BufferDesc::builder()
                .byte_width(std::mem::size_of_val(geometry::CUBE_VERTICES) as u32)
                .usage(d3d11::Usage::Immutable)
                .bind_flags(d3d11::BindFlags::VERTEX_BUFFER)
                .build(),
            Some(bytemuck::cast_slice(geometry::CUBE_VERTICES)),
        )?;

        let ib = renderer.gpu.create_buffer(
            &d3d11::BufferDesc::builder()
                .byte_width(std::mem::size_of_val(geometry::CUBE_INDICES) as u32)
                .usage(d3d11::Usage::Immutable)
                .bind_flags(d3d11::BindFlags::INDEX_BUFFER)
                .build(),
            Some(bytemuck::cast_slice(geometry::CUBE_INDICES)),
        )?;

        let load_technique = |hash| {
            if matches!(submission_path, LightSubmissionPath::Shadowkeep { .. }) {
                Technique::load_shadowkeep(&renderer.gpu, &renderer.asset_manager, hash)
            } else {
                Technique::load(&renderer.gpu, &renderer.asset_manager, hash)
            }
        };

        Ok(Box::new(Self {
            data: Arc::new(LightRendererData {
                technique_lighting_apply: load_technique(technique_shading)?,
                technique_lighting_apply_shadowing: technique_shading_shadowing
                    .is_some()
                    .then(|| load_technique(technique_shading_shadowing))
                    .transpose()?,
                technique_volumetrics: technique_volumetrics
                    .is_some()
                    .then(|| load_technique(technique_volumetrics))
                    .transpose()?,
                technique_volumetrics_shadowing: technique_volumetrics_shadowing
                    .is_some()
                    .then(|| load_technique(technique_volumetrics_shadowing))
                    .transpose()?,
                // technique_light_probe_apply: Technique::load(&renderer.gpu, technique_light_probe)?,
                vb,
                ib,
            }),
            submission_path,
            local_to_world: Mat4::IDENTITY,
            light_space_transform,
            shadowmap_projection,
            bounds,
            shadow_view: None,
        }))
    }
    fn submit_shadowkeep_lighting(&self, cmd: &mut crate::gpu::command_list::CommandList) {
        let renderer = Renderer::instance();
        let global_externs = renderer.externs.get();
        let view_position = global_externs.view.position();
        let volume = self.local_to_world * self.light_space_transform;

        cmd.externs.simple_geometry = Some(Box::new(SimpleGeometry {
            local_to_world: global_externs.view.world_to_projective * volume,
        }));

        let node_relative = Mat4::from_translation(-view_position) * self.local_to_world;
        let shadowing = matches!(
            self.submission_path,
            LightSubmissionPath::Shadowkeep { shadowing: true }
        );
        let existing_deferred_light = cmd
            .externs
            .deferred_light
            .as_ref()
            .cloned()
            .unwrap_or_default();
        cmd.externs.deferred_light = Some(Box::new(DeferredLight {
            legacy_unk40: Mat4::IDENTITY,
            unk40: if shadowing {
                Mat4::from_translation(view_position)
            } else {
                Mat4::IDENTITY
            },
            unk80: node_relative,
            ..*existing_deferred_light
        }));

        let (_, transform_rot, transform_translation) =
            self.local_to_world.to_scale_rotation_translation();
        let forward = transform_rot * Vec3::X;
        let up = transform_rot * Vec3::Z;
        let transform_translation = transform_translation - view_position;
        let transform_relative =
            Mat4::look_at_rh(transform_translation, transform_translation + forward, up);

        let has_shadow_view = renderer.settings().shadows && self.shadow_view.is_some();
        if has_shadow_view && let Some((shadowmap, shadowmap_srv)) = self.shadow_view.as_ref() {
            renderer
                .common
                .shadowmap_vs_t2
                .bind(cmd, 2, ShaderStage::Vertex);
            let existing_shadowmap = cmd
                .externs
                .deferred_shadow
                .as_ref()
                .cloned()
                .unwrap_or_default();
            let shadowmap_desc = shadowmap.get_desc();
            cmd.externs.deferred_shadow = Some(
                externs::DeferredShadow {
                    shadow_depthmap: shadowmap_srv.clone().into(),
                    resolution_width: shadowmap_desc.width as f32,
                    resolution_height: shadowmap_desc.height as f32,
                    unkc0: self.shadowmap_projection * transform_relative,
                    unk180: 2.0,
                    ..*existing_shadowmap
                }
                .into(),
            );
        }

        if let Err(error) = renderer.set_input_layout(cmd, 1) {
            tracing::error!(error = %error, "Failed to bind Shadowkeep light input layout");
            return;
        }
        cmd.set_input_topology(PrimitiveType::Triangles);
        cmd.input_assembler_set_vertex_buffers(0, &[Some(&self.data.vb)], Some(&[12]), Some(&[0]))
            .unwrap();
        cmd.state = cmd
            .state
            .select(&PipelineState::new(Some(8), Some(0), Some(2), Some(2)));
        let technique = if has_shadow_view {
            self.data
                .technique_lighting_apply_shadowing
                .as_ref()
                .unwrap_or(&self.data.technique_lighting_apply)
        } else {
            &self.data.technique_lighting_apply
        };
        technique.bind(cmd).unwrap();
        if SHADOWKEEP_LIGHT_BINDING_REPORTED
            .compare_exchange(false, true, Ordering::Relaxed, Ordering::Relaxed)
            .is_ok()
        {
            tracing::warn!(
                local_to_world = ?self.local_to_world,
                light_space_transform = ?self.light_space_transform,
                volume = ?volume,
                world_to_projective = ?global_externs.view.world_to_projective,
                node_relative = ?node_relative,
                state = format_args!("0x{:08X}", cmd.state.raw()),
                "Shadowkeep light binding diagnostic"
            );
        }

        cmd.input_assembler_set_index_buffer(&self.data.ib, dxgi::Format::R16Uint, 0);
        cmd.draw_indexed(geometry::CUBE_INDICES.len() as u32, 0, 0);
        if SHADOWKEEP_LIGHT_CAPTURE_ACTIVE.load(Ordering::Acquire) {
            SHADOWKEEP_LIGHT_CAPTURE_DRAWS.fetch_add(1, Ordering::Relaxed);
            SHADOWKEEP_LIGHT_CAPTURE_TECHNIQUES
                .lock()
                .insert(technique.hash.to_string());
        }
        if SHADOWKEEP_DEFERRED_LIGHT_DRAW_REPORTED
            .compare_exchange(false, true, Ordering::Relaxed, Ordering::Relaxed)
            .is_ok()
        {
            tracing::warn!(
                stage = ?RenderStage::LightingApply,
                technique = %technique.hash,
                index_count = geometry::CUBE_INDICES.len(),
                "Shadowkeep DeferredLights reached LightingApply and issued DrawIndexed"
            );
        }
        cmd.flush_states();
    }
}

#[profiling::all_functions]
impl FeatureRenderer for LightRenderer {
    fn visibility_test(&mut self, _view_index: usize, view: &dyn OpaqueView) -> bool {
        match self.submission_path {
            LightSubmissionPath::Shadowkeep { .. } => true,
            LightSubmissionPath::Current => self
                .bounds
                .as_ref()
                .is_none_or(|bounds| view.is_visible(bounds)),
        }
    }

    fn prepare(
        &mut self,
        _renderer: &crate::Renderer,
        _view_index: usize,
        extracted_data: &dyn std::any::Any,
    ) {
        // TODO(cohae): lights shouldnt need to extract permutations at all
        let (obj_local_to_world, _permutation) = extracted_data
            .downcast_ref::<(CompactTransform, usize)>()
            .expect("Invalid extracted data type")
            .clone();

        self.local_to_world = obj_local_to_world.to_mat4();

        let local_to_world_scaled = self.local_to_world * self.light_space_transform;
        let points = geometry::CUBE_VERTICES
            .iter()
            .map(|&v| local_to_world_scaled.project_point3(v))
            .collect_vec();

        self.bounds = Some(AxisAlignedBBox::from_points(&points));
    }

    fn submit(
        &self,
        cmd: &mut crate::gpu::command_list::CommandList,
        _view_index: usize,
        stage: RenderStage,
    ) {
        // Local-light volumes are consumers of a generated shadow map, never
        // casters. Submitting their lighting technique during ShadowGenerate
        // both draws the volume into its own map and requires DeferredLight
        // inputs that the caster pass correctly does not populate.
        if matches!(
            stage,
            RenderStage::LightProbeApply | RenderStage::ShadowGenerate
        ) {
            return;
        }
        if stage == RenderStage::LightingApply
            && matches!(self.submission_path, LightSubmissionPath::Shadowkeep { .. })
        {
            self.submit_shadowkeep_lighting(cmd);
            return;
        }

        let shadowmap_projection = self.shadowmap_projection;

        let (_, transform_rot, transform_translation) =
            self.local_to_world.to_scale_rotation_translation();

        let forward = transform_rot * Vec3::X;
        let up = transform_rot * Vec3::Z;
        let transform_translation =
            transform_translation - Renderer::instance().externs.view.position();
        let transform_relative =
            Mat4::look_at_rh(transform_translation, transform_translation + forward, up);

        {
            let local_to_world_scaled = self.local_to_world * self.light_space_transform;
            let global_externs = Renderer::instance().externs.get();
            let is_camera_in_volume = self
                .bounds
                .as_ref()
                .is_none_or(|b| b.contains(global_externs.view.position()));

            if is_camera_in_volume {
                cmd.state = cmd
                    .state
                    .select(&PipelineState::new(None, Some(0), None, None));
            } else {
                cmd.state = cmd
                    .state
                    .select(&PipelineState::new(None, Some(30), None, None));
            }

            cmd.externs.simple_geometry = Some(Box::new(SimpleGeometry {
                local_to_world: global_externs.view.world_to_projective
                    * local_to_world_scaled
                    * if is_camera_in_volume {
                        Mat4::from_scale(Vec3::NEG_ONE)
                    } else {
                        Mat4::IDENTITY
                    },
            }));

            let view_translation_inverse_mat4 =
                Mat4::from_translation(-global_externs.view.position());
            let local_to_world_relative = view_translation_inverse_mat4 * self.local_to_world;

            let (min, max) = compute_light_bounds(self.light_space_transform);
            let light_local_to_world = compute_light_local_to_world(self.local_to_world, min, max);

            cmd.externs.deferred_light = Some(Box::new(DeferredLight {
                // Preserve the Arrivals light ABI: the 0x80 matrix is the
                // view-relative node transform and the 0xC0 matrix is its
                // volume transform.  The earlier normalized inverse placed
                // the wrong matrix in both slots and produced an empty light
                // buffer despite valid light draws.
                legacy_unk40: Mat4::IDENTITY,
                unk40: Mat4::IDENTITY,
                unk80: local_to_world_relative,

                ..Default::default()
            }));

            cmd.externs.rigid_model = Some(Box::new(externs::RigidModel {
                local_to_world: light_local_to_world,
                ..Default::default()
            }));

            if stage == RenderStage::Volumetrics {
                let mut fog = VolumeFog::default();
                fog.unk00 = light_local_to_world.inverse();
                fog.unk40 = fog.unk00 * global_externs.view.target_pixel_to_world;
                fog.unka0 = (max - min).extend(1.);
                fog.unkb0 = 1.0;

                let p = fog
                    .unk00
                    .mul_vec4(global_externs.view.position().extend(1.0));
                let point_w_abs = (-p.wwww()).abs();
                fog.unk80 = Vec4::select(point_w_abs.cmpge(Vec4::splat(0.0001)), p / p.wwww(), p);
                // fog.unk80 = fog
                //     .unk00
                //     .project_point3(externs.view.position())
                //     .extend(1.0);

                if ((fog.unk80.x < -0.2) || (1.2 < fog.unk80.x))
                    || ((fog.unk80.y < -0.2) || (1.2 < fog.unk80.y))
                    || ((fog.unk80.z < -0.2) || (1.2 < fog.unk80.z))
                {
                    // cmd.state =
                    //     cmd.state
                    //         .select(&PipelineState::new(Some(0xf), Some(3), None, None));
                    fog.unkb4 = -1.0;
                } else {
                    // cmd.state = cmd
                    //     .state
                    //     .select(&PipelineState::new(Some(1), Some(2), None, None));
                    fog.unkb4 = 1.0;
                }

                cmd.externs.volume_fog = Some(fog.into());
            }
        }

        if Renderer::instance().settings().shadows
            && let Some((shadowmap, shadowmap_srv)) = self.shadow_view.as_ref()
        {
            // TODO(cohae): Unknown what this texture is supposed to be. VS loads the first pixel and uses it as multiplier for the shadowmap UVs
            Renderer::instance()
                .common
                .shadowmap_vs_t2
                .bind(cmd, 2, ShaderStage::Vertex);
            let existing_shadowmap = cmd
                .externs
                .deferred_shadow
                .as_ref()
                .cloned()
                .unwrap_or_default();

            let shadowmap_desc = shadowmap.get_desc();
            cmd.externs.deferred_shadow = Some(
                externs::DeferredShadow {
                    shadow_depthmap: shadowmap_srv.clone().into(),
                    resolution_width: shadowmap_desc.width as f32,
                    resolution_height: shadowmap_desc.height as f32,
                    // unkc0: shadowmap.camera_to_projective * transform_relative.view_matrix(),
                    unkc0: shadowmap_projection * transform_relative,
                    unk180: 1.0,
                    // unk180: renderer.settings.shadow_quality.pcf_samples() as u8 as f32,
                    ..*existing_shadowmap
                }
                .into(),
            );

            if stage == RenderStage::Volumetrics {
                if let Some(technique) = self
                    .data
                    .technique_volumetrics_shadowing
                    .as_ref()
                    .or(self.data.technique_volumetrics.as_ref())
                {
                    technique.bind(cmd).unwrap();
                } else {
                    return;
                }
            } else {
                // self.data.technique_lighting_apply.bind(cmd).unwrap();
                self.data
                    .technique_lighting_apply_shadowing
                    .as_ref()
                    .unwrap_or(&self.data.technique_lighting_apply)
                    .bind(cmd)
                    .unwrap();
            }
        } else if stage == RenderStage::Volumetrics {
            if let Some(ref technique) = self.data.technique_volumetrics {
                technique.bind(cmd).unwrap();
            } else {
                return;
            }
        } else {
            self.data.technique_lighting_apply.bind(cmd).unwrap();
        }

        cmd.set_input_topology(PrimitiveType::Triangles);
        if let Err(error) = Renderer::instance().set_input_layout(cmd, 1) {
            tracing::error!(error = %error, "Failed to bind light input layout");
            return;
        }

        cmd.input_assembler_set_index_buffer(&self.data.ib, dxgi::Format::R16Uint, 0);
        cmd.input_assembler_set_vertex_buffers(0, &[Some(&self.data.vb)], Some(&[12]), Some(&[0]))
            .unwrap();

        cmd.draw_indexed(geometry::CUBE_INDICES.len() as u32, 0, 0);
        cmd.flush_states();
    }

    fn submit_parallel(
        &self,
        renderer: &std::sync::Arc<Renderer>,
        _view_index: usize,
        set: CommandListSetId,
        stage: RenderStage,
        jobs: &mut Vec<alkahest_core::job::potassium::JobHandle>,
    ) {
        // let (scale, _rotation, _translation) =
        //     self.local_to_world.to_scale_rotation_translation();

        let pool_clone = renderer.cmd_pool.clone();
        let light_space_transform = self.light_space_transform;
        let local_to_world = self.local_to_world;
        let shadowmap_projection = self.shadowmap_projection;
        let local_to_world_scaled = local_to_world * light_space_transform;
        let data = self.data.clone();
        let bounds = self.bounds;

        let (_, transform_rot, transform_translation) =
            self.local_to_world.to_scale_rotation_translation();

        let forward = transform_rot * Vec3::X;
        let up = transform_rot * Vec3::Z;
        let transform_translation = transform_translation - renderer.externs.view.position();
        let transform_relative =
            Mat4::look_at_rh(transform_translation, transform_translation + forward, up);

        let shadow_view = self.shadow_view.clone();

        let job = SCHEDULER
            .job_builder("light_render")
            .priority(Priority::Medium)
            .spawn(move || {
                let cmd = pool_clone.get_command_list(set);
                {
                    let externs = Renderer::instance().externs.get();

                    let is_camera_in_volume = bounds
                        .as_ref()
                        .is_none_or(|b| b.contains(externs.view.position()));

                    if is_camera_in_volume {
                        cmd.state =
                            cmd.state
                                .select(&PipelineState::new(None, Some(0), None, None));
                    } else {
                        cmd.state =
                            cmd.state
                                .select(&PipelineState::new(None, Some(30), None, None));
                    }

                    cmd.externs.simple_geometry = Some(Box::new(SimpleGeometry {
                        local_to_world: externs.view.world_to_projective
                            * local_to_world_scaled
                            * if is_camera_in_volume {
                                Mat4::from_scale(Vec3::NEG_ONE)
                            } else {
                                Mat4::IDENTITY
                            },
                    }));

                    let view_translation_inverse_mat4 =
                        Mat4::from_translation(-externs.view.position());
                    let local_to_world_relative = view_translation_inverse_mat4 * local_to_world;

                    let (min, max) = compute_light_bounds(light_space_transform);
                    let light_local_to_world =
                        compute_light_local_to_world(local_to_world, min, max);

                    cmd.externs.deferred_light = Some(Box::new(DeferredLight {
                        legacy_unk40: Mat4::IDENTITY,
                        unk40: Mat4::IDENTITY,
                        unk80: local_to_world_relative,

                        ..Default::default()
                    }));

                    cmd.externs.rigid_model = Some(Box::new(externs::RigidModel {
                        local_to_world: light_local_to_world,
                        ..Default::default()
                    }));
                }

                if Renderer::instance().settings().shadows
                    && let Some((shadowmap, shadowmap_srv)) = shadow_view
                {
                    // TODO(cohae): Unknown what this texture is supposed to be. VS loads the first pixel and uses it as multiplier for the shadowmap UVs
                    Renderer::instance()
                        .common
                        .shadowmap_vs_t2
                        .bind(cmd, 2, ShaderStage::Vertex);
                    let existing_shadowmap = cmd
                        .externs
                        .deferred_shadow
                        .as_ref()
                        .cloned()
                        .unwrap_or_default();

                    let shadowmap_desc = shadowmap.get_desc();
                    cmd.externs.deferred_shadow = Some(
                        externs::DeferredShadow {
                            shadow_depthmap: shadowmap_srv.into(),
                            resolution_width: shadowmap_desc.width as f32,
                            resolution_height: shadowmap_desc.height as f32,
                            // unkc0: shadowmap.camera_to_projective * transform_relative.view_matrix(),
                            unkc0: shadowmap_projection * transform_relative,
                            unk180: 1.0,
                            // unk180: renderer.settings.shadow_quality.pcf_samples() as u8 as f32,
                            ..*existing_shadowmap
                        }
                        .into(),
                    );

                    if stage == RenderStage::Volumetrics {
                        if let Some(technique) = data
                            .technique_volumetrics_shadowing
                            .as_ref()
                            .or(data.technique_volumetrics.as_ref())
                        {
                            technique.bind(cmd).unwrap();
                        } else {
                            return;
                        }
                    } else {
                        data.technique_lighting_apply_shadowing
                            .as_ref()
                            .unwrap_or(&data.technique_lighting_apply)
                            .bind(cmd)
                            .unwrap();
                    }
                } else if stage == RenderStage::Volumetrics {
                    if let Some(ref technique) = data.technique_volumetrics {
                        technique.bind(cmd).unwrap();
                    } else {
                        return;
                    }
                } else {
                    data.technique_lighting_apply.bind(cmd).unwrap();
                }

                cmd.set_input_topology(PrimitiveType::Triangles);
                if let Err(error) = Renderer::instance().set_input_layout(cmd, 1) {
                    tracing::error!(error = %error, "Failed to bind light input layout");
                    return;
                }

                cmd.input_assembler_set_index_buffer(&data.ib, dxgi::Format::R16Uint, 0);
                cmd.input_assembler_set_vertex_buffers(
                    0,
                    &[Some(&data.vb)],
                    Some(&[12]),
                    Some(&[0]),
                )
                .unwrap();

                cmd.draw_indexed(geometry::CUBE_INDICES.len() as u32, 0, 0);
            });

        jobs.push(job);
    }

    fn subscribed_stages(&self) -> RenderStageSubscription {
        RenderStageSubscription::LIGHTING_APPLY
            | RenderStageSubscription::LIGHT_PROBE_APPLY
            | RenderStageSubscription::VOLUMETRICS
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
}

fn compute_light_bounds(light_space_transform: Mat4) -> (Vec3, Vec3) {
    let mut points = [
        Vec3::new(-1.0, -1.0, -1.0),
        Vec3::new(-1.0, -1.0, 1.0),
        Vec3::new(-1.0, 1.0, -1.0),
        Vec3::new(-1.0, 1.0, 1.0),
        Vec3::new(1.0, -1.0, -1.0),
        Vec3::new(1.0, -1.0, 1.0),
        Vec3::new(1.0, 1.0, -1.0),
        Vec3::new(1.0, 1.0, 1.0),
    ];

    for point in &mut points {
        let p = light_space_transform.mul_vec4(point.extend(1.0));
        let point_w_abs = (-p.wwww()).abs();
        *point = Vec4::select(
            point_w_abs.cmpge(Vec4::splat(0.0001)),
            p / p.wwww(),
            Vec4::W,
        )
        .truncate();
    }

    points
        .iter()
        .fold((Vec3::MAX, Vec3::MIN), |(min, max), &point| {
            (min.min(point), max.max(point))
        })
}

fn compute_light_local_to_world(node_local_to_world: Mat4, min: Vec3, max: Vec3) -> Mat4 {
    let bounds_center = min.midpoint(max);
    let bounds_half_extents = (max - min) / 2.0;

    // First matrix operation ("mat"):
    // Each column is computed by scaling one of node_local_to_world’s axes by the corresponding component of bounds_half_extents,
    // except for the w-axis which is a linear combination of the x, y, and z axes plus the original w-axis.
    let mat = Mat4 {
        x_axis: node_local_to_world.x_axis * bounds_half_extents.x,
        y_axis: node_local_to_world.y_axis * bounds_half_extents.y,
        z_axis: node_local_to_world.z_axis * bounds_half_extents.z,
        w_axis: node_local_to_world.x_axis * bounds_center.x
            + node_local_to_world.y_axis * bounds_center.y
            + node_local_to_world.z_axis * bounds_center.z
            + node_local_to_world.w_axis,
    };

    // Second matrix operation ("mat_scaled"):
    // Scale the x, y, and z axes by 2, and subtract all three from the w-axis.
    let mat_scaled = Mat4 {
        x_axis: mat.x_axis * 2.0,
        y_axis: mat.y_axis * 2.0,
        z_axis: mat.z_axis * 2.0,
        w_axis: mat.w_axis - mat.x_axis - mat.y_axis - mat.z_axis,
    };

    // Third matrix operation (computing light_local_to_world):
    // Rearrange the columns of mat_scaled: swap the x and z axes, leaving y and w unchanged.

    Mat4 {
        x_axis: mat_scaled.z_axis,
        y_axis: mat_scaled.y_axis,
        z_axis: mat_scaled.x_axis,
        w_axis: mat_scaled.w_axis,
    }
}

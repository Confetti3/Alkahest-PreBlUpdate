pub mod atmosphere;
pub mod bloom;
pub mod buffers;
pub mod gbuffer;
pub mod geometry;
pub mod lighting;
pub mod lowlevel;
pub mod sun_shadows;
pub mod transparent;
pub mod water;

use std::{fmt::Debug, path::Path, sync::Arc};

use alkahest_core::convar::ConVars;
use alkahest_data::tfx::{
    ExternIndex, FeatureRendererSubscription, PipelineState, PrimitiveType, RenderStage,
    ShaderStage,
};
use glam::{Mat4, Vec4, vec4};

use super::{
    Renderer,
    provenance::{
        DeferredShadingProvenance, ExposureAbManifest, ExposureVariantProvenance,
        FinalCombineManifest, FinalCombineProvenance, GlobalLightingAbManifest,
        GlobalLightingChannelValue, GlobalLightingDependencyManifest,
        GlobalLightingDependencyStage, GlobalLightingExternRead, GlobalLightingExternValue,
        ProvenanceManifest, ShadowkeepLightingProvenance, capture_surface,
    },
};
use crate::{
    camera::Camera,
    cmd_event_span,
    gpu::command_list::{CommandList, DepthMode},
    tfx::{
        expression_vm::{
            self,
            opcodes::{Opcode, OpcodeIterator},
        },
        externs::{self, GlobalLighting, ScreenArea, TextureView, UberDepth},
        scope::FrameScope,
        technique::{ShaderModule, Technique},
        view::{MainView, ShadowView, View, ViewKind},
    },
};

impl Renderer {
    pub fn submit_view(
        self: &Arc<Self>,
        cmd: &mut CommandList,
        view: &View,
        debug_pipeline: Option<DebugPipeline>,
    ) {
        cmd_event_span!(cmd, format!("submit_view_{}", view.name));
        let _gpuspan = self
            .profiler
            .scope(cmd, format!("submit_view_{}", view.name));

        *self.settings.write() = view.settings().clone();

        self.active_feature_renderers
            .store(self.frame_packet.read().misc.subscribed_features);

        self.prepare_externs(cmd, view);

        self.globals.scopes.view.bind(cmd).unwrap();

        match &view.kind {
            crate::tfx::view::ViewKind::Main(main_view) => {
                *self.surfaces.write() = main_view.surfaces.clone();
                main_view.surfaces.resize_surfaces(view.resolution);

                match debug_pipeline {
                    Some(DebugPipeline::Overdraw) => self.submit_view_overdraw(cmd, main_view),
                    _ => self.submit_view_shaded(cmd, main_view, debug_pipeline),
                }
            }
            crate::tfx::view::ViewKind::Shadow(shadow_view) => {
                self.submit_shadow_view(cmd, shadow_view)
            }
        }
    }

    fn submit_shadow_view(self: &Arc<Self>, cmd: &mut CommandList, view: &ShadowView) {
        if view.index >= 32 {
            error!("Shadow view index out of range ({}, max 32)", view.index);
            return;
        }

        cmd.state = PipelineState::new(Some(0), Some(2), Some(0), Some(6));
        cmd.flush_states();

        self.common
            .shadowmap_vs_t2
            .bind(cmd, 2, ShaderStage::Vertex);

        let shadowmap = &view.shadow_map;

        shadowmap.clear_depth(
            cmd,
            if cmd.depth_mode() == DepthMode::Forward {
                1.0
            } else {
                0.0
            },
            0,
        );
        shadowmap.bind_single(cmd);

        self.submit_stage_parallel_apply(
            cmd,
            view.index,
            RenderStage::ShadowGenerate,
            FeatureRendererSubscription::all(),
        );
    }

    fn submit_view_shaded(
        self: &Arc<Self>,
        cmd: &mut CommandList,
        view: &MainView,
        debug_pipeline: Option<DebugPipeline>,
    ) {
        // The preserved Arrivals renderer submits geometry directly on its
        // immediate context. Its scopes and techniques mutate shared binding
        // state, so routing that work through the post-BL deferred-command
        // path can produce an empty G-buffer even when every asset is ready.
        let geo = if self.era() == crate::renderer::RendererEra::Current
            && view.settings.multithreading
        {
            Some(self.submit_geometry_command_lists(cmd, view))
        } else {
            None
        };

        self.submit_gbuffer_generation(cmd, view, geo.as_ref());

        // Shadowkeep owns a different deferred/light/postprocess graph. The
        // modern post-BL sequence below references several explicitly-null
        // legacy pipelines. Run the admitted preserved opaque/light/shading
        // slice without entering the later-era atmosphere/transparent graph.
        if self.era() == crate::renderer::RendererEra::Shadowkeep {
            self.submit_shadowkeep_shaded(cmd, view, debug_pipeline);
            return;
        }

        if matches!(
            debug_pipeline,
            Some(DebugPipeline::LightDiffuse) | Some(DebugPipeline::LightSpecular)
        ) || debug_pipeline.is_some_and(|p| p.is_shaded())
        {
            self.submit_lighting(cmd, view, geo.as_ref(), debug_pipeline);
        }

        self.submit_atmosphere(cmd, view);

        self.clear_surface(cmd, view.shading_result, [0., 0., 0., 1.0]);
        self.bind_surfaces(cmd, &[view.shading_result], None);
        cmd.output_merger_set_depth_stencil_state(None, 0);

        cmd.state = PipelineState::new(Some(0), Some(0), Some(0), Some(0));
        if ConVars::get_flag("render.global_lighting") {
            self.execute_global_pipeline(
                cmd,
                &self.globals.pipelines.global_lighting_and_shading_gel,
                "global_lighting_and_shading_gel",
            );
        } else {
            self.execute_global_pipeline(
                cmd,
                &self.globals.pipelines.deferred_shading,
                "deferred_shading",
            );
        }

        // {
        //     cmd.state = PipelineState::new(Some(0), Some(0), Some(0), Some(0));
        //     cmd.flush_states();
        //     cmd.rasterizer_set_viewports(&[d3d11::Viewport::builder()
        //         .width(gpu.swapchain_resolution().0 as f32)
        //         .height(gpu.swapchain_resolution().1 as f32)
        //         .build()]);
        //     cmd.vertex_set_shader(Some(&self.common.blit_vs));
        //     cmd.pixel_set_shader(Some(&self.common.blit_ps));
        //     cmd.set_input_topology(alkahest_data::tfx::PrimitiveType::TriangleStrip);
        //     cmd.clear_render_target_view(&gpu.acquire_rtv(), &[0., 0., 0., 1.0]);
        //     cmd.output_merger_set_render_targets(&[Some(gpu.acquire_rtv())], None);
        //     let srv_shading_result = view.surfaces.get(self.shading_result).srv.clone();
        //     cmd.pixel_set_shader_resources(0, &[srv_shading_result]);
        //     cmd.draw(4, 0);
        // }

        let output = view.surfaces.get(view.shading_result);
        self.profiler.scope(cmd, "debug_view").span(|| {
            cmd.clear_render_target_view(output.rtv.as_ref().unwrap(), &[0., 0., 0., 1.0]);
            output.bind_single(cmd);

            if let Some(debug_pipeline) = debug_pipeline {
                let p = &self.globals.pipelines;
                let technique = match debug_pipeline {
                    DebugPipeline::Shaded => &p.deferred_shading,
                    DebugPipeline::ShadedNoAtm => &p.deferred_shading_no_atm,
                    DebugPipeline::ShadedNoSun => &p.deferred_shading,
                    DebugPipeline::ShadingOnly => &p.deferred_shading_no_atm,
                    DebugPipeline::Albedo => &p.debug_source_color,
                    DebugPipeline::Smoothness => &p.debug_specular_smoothness,
                    DebugPipeline::Metalness => &p.debug_metalness,
                    DebugPipeline::AmbientOcclusion => &p.debug_ambient_occlusion,
                    DebugPipeline::Emission => &p.debug_emissive,
                    DebugPipeline::EmissionIntensity => &p.debug_emissive_intensity,
                    DebugPipeline::Transmission => &p.debug_transmission,
                    DebugPipeline::Overcoat => &p.debug_colored_overcoat_id,
                    DebugPipeline::DepthEdges => &p.debug_depth_edges,
                    DebugPipeline::WorldNormal => &p.debug_world_normal,
                    DebugPipeline::LightDiffuse => &p.debug_diffuse_light,
                    DebugPipeline::LightSpecular => &p.debug_specular_light,

                    DebugPipeline::Overdraw => &p.deferred_shading,
                };

                self.execute_global_pipeline(cmd, technique, &format!("{debug_pipeline:?}"));
            } else {
                let sun_light_direction = if self.era() == crate::renderer::RendererEra::Shadowkeep {
                    self.externs.get().global_lighting.unk30
                } else {
                    self.externs
                        .get_global_channel_by_name("sun_light_direction")
                };
                self.debug_cbuffer
                    .write(
                        cmd,
                        &Mat4 {
                            x_axis: sun_light_direction,
                            ..Default::default()
                        },
                    )
                    .ok();
                self.debug_cbuffer.bind(cmd, ShaderStage::Pixel, 0);

                cmd.state = PipelineState::new(Some(0), Some(0), Some(0), Some(0));
                cmd.flush_states();
                cmd.vertex_set_shader(Some(&self.debug_vs));
                cmd.pixel_set_shader(Some(&self.debug_ps));
                cmd.set_input_topology(alkahest_data::tfx::PrimitiveType::TriangleStrip);
                cmd.pixel_set_shader_resources(
                    0,
                    &[
                        view.surfaces.get(view.gbuffers.albedo).srv(0),
                        view.surfaces.get(view.gbuffers.normal).srv(0),
                        view.surfaces.get(view.gbuffers.third).srv(0),
                        Some(&view.gbuffers.depth_proxy.lock().srv),
                    ],
                );
                cmd.draw(4, 0);
            }
        });

        if debug_pipeline.is_none_or(|p| p.has_atmosphere()) {
            self.debug_cbuffer
                .write(
                    cmd,
                    &Mat4::from_cols(
                        if self.era() == crate::renderer::RendererEra::Shadowkeep {
                            Vec4::ZERO
                        } else {
                            self.externs
                                .get_global_channel_by_name("sky_snapshot_intensity")
                        },
                        Vec4::ZERO,
                        Vec4::ZERO,
                        Vec4::ZERO,
                    ),
                )
                .ok();

            self.debug_cbuffer.bind(cmd, ShaderStage::Pixel, 0);

            cmd.state = PipelineState::new(Some(0), Some(0), Some(0), Some(0));
            cmd.flush_states();
            cmd.vertex_set_shader(Some(&self.blit_to_background_vs));
            cmd.pixel_set_shader(Some(&self.blit_to_background_ps));
            cmd.set_input_topology(alkahest_data::tfx::PrimitiveType::TriangleStrip);
            cmd.pixel_set_shader_resources(
                0,
                &[
                    Some(&view.gbuffers.depth_proxy.lock().srv),
                    view.surfaces.get(view.atmosphere.sky_lookup_near).srv(0),
                ],
            );
            cmd.draw(4, 0);
        }

        // view.shading_result_read
        //     .lock()
        //     .update(cmd, view.surfaces.get(view.shading_result));

        if debug_pipeline.is_none_or(|p| p.is_shaded()) {
            self.submit_transparent(cmd, view, geo.as_ref());

            view.shading_result_read
                .lock()
                .update(cmd, view.surfaces.get(view.shading_result));

            self.submit_water(cmd, view);
            if debug_pipeline.is_some() {
                self.apply_volume_fog(cmd, view);
            }
            self.submit_bloom(cmd, view);

            // // Turn on bit 0x10 for all stencil buffer pixels
            // {
            //     cmd_event_span!(cmd, "set_stencil_bit_0x10");
            //     cmd.set_stencil_ref(0x10);
            //     cmd.state = PipelineState::new(Some(0), Some(77), Some(0), Some(0));
            //     cmd.flush_states();
            //     cmd.vertex_set_shader(Some(&self.common.blit_vs));
            //     cmd.pixel_set_shader(None);
            //     cmd.set_input_topology(PrimitiveType::TriangleStrip);
            //     cmd.draw(4, 0);
            // }

            // {
            //     cmd.set_stencil_ref(0);
            //     cmd.state = PipelineState::new(Some(0), Some(50), Some(0), Some(0));
            //     // Copies the sky lookup to the screen where depth is infinite, and masks out sky pixels in the stencil buffer
            //     self.execute_global_pipeline(cmd, &self.globals.pipelines.sky, "sky");
            // }

            cmd.set_stencil_ref(0);
            view.shading_result_read
                .lock()
                .update(cmd, view.surfaces.get(view.shading_result));
            view.surfaces.get(view.postprocess).bind_single(cmd);
            cmd.state = PipelineState::new(Some(0), Some(0), Some(0), Some(0));
            // cmd.flush_states();
            self.execute_global_pipeline(
                cmd,
                self.globals
                            .pipelines
                            // .screen_area_global_lut3d_no_tonemap,
                            .get_specialized_lut3d_pipeline(true, false, false),
                "screen_area_global_lut3d",
            );
        } else {
            view.shading_result_read
                .lock()
                .update(cmd, view.surfaces.get(view.shading_result));
            self.bind_surfaces(cmd, &[view.postprocess], None);
            // Directly blit to output
            self.blit_srv(
                cmd,
                &view.shading_result_read.lock().srv.clone(),
                &view.surfaces.get(view.postprocess).rtv,
                true,
                "final_blit_debug",
            );
        }

        {
            profiling::scope!("prepare/submit immediate geometry");
            let _gpuspan = self.profiler.scope(cmd, "immediate_geometry");

            const IMMEDIATE_GEOMETRY_XRAY: bool = false;
            // if IMMEDIATE_GEOMETRY_XRAY {
            //     view.surfaces
            //         .get(view.gbuffers.depth)
            //         .clear_depth(cmd, 0.0, 0xff);
            // }

            self.bind_surfaces(cmd, &[view.postprocess], Some(view.gbuffers.depth));
            cmd.state = PipelineState::new(
                Some(0),
                Some(if IMMEDIATE_GEOMETRY_XRAY { 0 } else { 2 }),
                Some(2),
                Some(0),
            );
            cmd.flush_states();
            self.immediate.lock().prepare(&self.gpu);
            self.immediate.lock().submit(cmd);
        }

        cmd.state = PipelineState::new(Some(0), Some(0), Some(0), Some(0));
        if debug_pipeline.is_none_or(|p| p.aa_enabled()) {
            if view.settings.anti_aliasing {
                self.bind_surfaces(cmd, &[view.output], None);
                let _gpuspan = self.profiler.scope(cmd, "fxaa");
                *self.externs.get_mut().fxaa = externs::Fxaa {
                    source: view.postprocess.into(),
                    unk50: 0.75,
                    unk54: 1. / 6.,
                    unk58: 1. / 12.,
                    // unk80: self.externs.frame.render_time,
                    // unk90: vec4(0.25, -0.225, 0.40, 0.96),
                    ..Default::default()
                };

                self.execute_global_pipeline(cmd, &self.globals.pipelines.fxaa, "fxaa");
            } else {
                self.bind_surfaces(cmd, &[view.output], None);
                // Directly blit to output
                self.blit_srv(
                    cmd,
                    view.surfaces.get(view.postprocess).srv(0),
                    &view.surfaces.get(view.output).rtv,
                    true,
                    "final_blit_debug",
                );
            }
        } else {
            self.bind_surfaces(cmd, &[view.output], None);
            // Directly blit to output
            self.blit_srv(
                cmd,
                &view.shading_result_read.lock().srv.clone(),
                &view.surfaces.get(view.output).rtv,
                true,
                "final_blit_debug",
            );
        }

        // self.blit_srv(
        //     cmd,
        //     &view.shading_result_read.lock().srv,
        //     &view.surfaces.get(view.output).rtv,
        //     true,
        //     "final_blit",
        // );
    }

    fn submit_shadowkeep_shaded(
        &self,
        cmd: &mut CommandList,
        view: &MainView,
        debug_pipeline: Option<DebugPipeline>,
    ) {
        // Preserve direct G-buffer inspection while the later Shadowkeep
        // passes are admitted one at a time. These modes must not depend on
        // lighting or post-processing being available.
        if let Some(pipeline) = debug_pipeline.filter(|pipeline| !pipeline.is_shaded()) {
            match pipeline {
                DebugPipeline::Albedo => self.blit_surface(
                    cmd,
                    view.gbuffers.albedo,
                    view.output,
                    true,
                    "shadowkeep/debug_albedo",
                ),
                DebugPipeline::WorldNormal => self.blit_surface(
                    cmd,
                    view.gbuffers.normal,
                    view.output,
                    false,
                    "shadowkeep/debug_normal",
                ),
                DebugPipeline::Smoothness
                | DebugPipeline::Metalness
                | DebugPipeline::AmbientOcclusion
                | DebugPipeline::Emission
                | DebugPipeline::EmissionIntensity
                | DebugPipeline::Transmission
                | DebugPipeline::Overcoat => self.blit_surface(
                    cmd,
                    view.gbuffers.third,
                    view.output,
                    false,
                    "shadowkeep/debug_material",
                ),
                DebugPipeline::DepthEdges => self.blit_srv(
                    cmd,
                    &view.gbuffers.depth_proxy.lock().srv,
                    &view.surfaces.get(view.output).rtv,
                    false,
                    "shadowkeep/debug_depth",
                ),
                DebugPipeline::LightDiffuse | DebugPipeline::LightSpecular => {
                    // These need the lighting slice below and are selected
                    // after it has populated the corresponding targets.
                }
                DebugPipeline::Overdraw => self.submit_shadowkeep_geometry_preview(cmd, view),
                _ => unreachable!(),
            }
            if !matches!(
                pipeline,
                DebugPipeline::LightDiffuse | DebugPipeline::LightSpecular
            ) {
                return;
            }
        }

        // Shaded modes are the preserved renderer's normal path, not a
        // request for the temporary G-buffer lookdev composite.  The old
        // Arrivals renderer always ran opaque -> lighting -> deferred
        // shading before its final presentation.  Keep the preview only as
        // the explicit fallback when a required legacy producer is absent.
        let wants_global_lighting = ConVars::get_flag("render.global_lighting")
            && debug_pipeline.is_none_or(|pipeline| pipeline.has_sun());
        let pipelines = &self.globals.pipelines;
        if !pipelines.deferred_shading_no_atm.is_available()
            || (wants_global_lighting && !pipelines.global_lighting.is_available())
        {
            error!(
                global_lighting = pipelines.global_lighting.is_available(),
                deferred_shading_no_atm = pipelines.deferred_shading_no_atm.is_available(),
                "Shadowkeep shaded pass is unavailable; retaining G-buffer preview"
            );
            self.submit_shadowkeep_geometry_preview(cmd, view);
            return;
        }
        let capture_requested = ConVars::get_flag("render.shadowkeep_buffer_provenance");
        let global_lighting_ab_requested =
            ConVars::get_flag("render.shadowkeep_global_lighting_ab");
        let requested_feature_subscriptions = self.frame_packet.read().misc.subscribed_features;
        let asset_summary = self.asset_manager.diagnostic_summary();
        let capture_provenance =
            capture_requested && asset_summary.queued == 0 && asset_summary.loading == 0;
        let capture_global_lighting_ab = global_lighting_ab_requested
            && asset_summary.queued == 0
            && asset_summary.loading == 0;
        if capture_provenance {
            let _ = ConVars::set("render.shadowkeep_buffer_provenance", false.into());
        }
        if capture_global_lighting_ab {
            let _ = ConVars::set("render.shadowkeep_global_lighting_ab", false.into());
        }
        let global_lighting_ab_directory = Path::new("artifacts/shadowkeep-global-lighting-ab");
        let mut global_lighting_ab_before = Vec::new();
        let mut global_lighting_ab_after = Vec::new();

        // The preserved renderer starts these buffers at a small non-zero
        // diffuse floor before applying its global-lighting fullscreen pass.
        self.clear_surface(cmd, view.lighting.light_diffuse, [0.001, 0.001, 0.001, 0.0]);
        self.clear_surface(cmd, view.lighting.light_specular, [0.0; 4]);
        self.clear_surface(cmd, view.lighting.light_specular_ibl, [0.0; 4]);
        self.clear_surface(cmd, view.shadow_mask, [1.0; 4]);

        // Arrivals fills the diffuse/specular targets with local/deferred
        // lights before the optional fullscreen global-lighting technique.
        // Keep that producer in the Shadowkeep path instead of presenting a
        // zero light buffer to deferred shading.
        if capture_provenance {
            crate::feature::light::begin_shadowkeep_light_capture();
        }
        view.lighting
            .bind_diffuse_specular(cmd, &view.surfaces, &view.gbuffers);
        let diffuse = view.surfaces.get(view.lighting.light_diffuse);
        let specular = view.surfaces.get(view.lighting.light_specular);
        cmd.rasterizer_set_viewports(&[diffuse.viewport()]);
        cmd.output_merger_set_render_targets(&[diffuse.rtv.as_ref(), specular.rtv.as_ref()], None);
        cmd.state = PipelineState::new(Some(8), Some(0), Some(2), Some(2));
        cmd.flush_states();
        self.submit_stage(
            cmd,
            View::MAIN,
            RenderStage::LightingApply,
            FeatureRendererSubscription::all(),
        );
        let lighting_apply_stage_submitted = true;
        let light_capture = capture_provenance
            .then(crate::feature::light::finish_shadowkeep_light_capture);
        if capture_global_lighting_ab {
            // Produce the disabled baseline from this same frame before the
            // fullscreen pass mutates the light targets. This makes the
            // shading/output A/B comparable even when separate launches reach
            // asset readiness at different times.
            self.clear_surface(cmd, view.shading_result, [0.0, 0.0, 0.0, 1.0]);
            self.bind_surfaces(cmd, &[view.shading_result], None);
            cmd.output_merger_set_depth_stencil_state(None, 0);
            cmd.state = PipelineState::new(Some(0), Some(0), Some(0), Some(0));
            self.execute_shadowkeep_global_pipeline(
                cmd,
                &pipelines.deferred_shading_no_atm,
                "shadowkeep/global_lighting_ab_baseline",
            );
            self.blit_surface(
                cmd,
                view.shading_result,
                view.output,
                true,
                "shadowkeep/global_lighting_ab_baseline",
            );
        }
        if capture_global_lighting_ab {
            global_lighting_ab_before = self.capture_shadowkeep_global_lighting_surfaces(
                cmd,
                view,
                global_lighting_ab_directory.join("before_global_lighting"),
            );
        }

        let diffuse = view.surfaces.get(view.lighting.light_diffuse);
        let specular = view.surfaces.get(view.lighting.light_specular);
        cmd.output_merger_set_render_targets(&[diffuse.rtv.as_ref(), specular.rtv.as_ref()], None);
        let mut global_lighting_draw_6_reached = false;
        if wants_global_lighting {
            cmd.state = PipelineState::new(Some(8), Some(0), Some(0), Some(0));
            global_lighting_draw_6_reached = self.execute_shadowkeep_global_pipeline(
                cmd,
                &pipelines.global_lighting,
                "shadowkeep/global_lighting",
            );
            self.emit_shadowkeep_global_lighting_manifest(
                &pipelines.global_lighting,
                global_lighting_draw_6_reached,
            );
        }
        if capture_global_lighting_ab {
            global_lighting_ab_after = self.capture_shadowkeep_global_lighting_surfaces(
                cmd,
                view,
                global_lighting_ab_directory.join("after_global_lighting"),
            );
        }

        if matches!(
            debug_pipeline,
            Some(DebugPipeline::LightDiffuse | DebugPipeline::LightSpecular)
        ) {
            let source = if matches!(debug_pipeline, Some(DebugPipeline::LightDiffuse)) {
                view.lighting.light_diffuse
            } else {
                view.lighting.light_specular
            };
            self.blit_surface(cmd, source, view.output, true, "shadowkeep/debug_light");
            return;
        }
        let provenance_directory = Path::new("artifacts/shadowkeep-buffer-provenance");
        let mut provenance_captures = Vec::new();
        if capture_provenance {
            for (handle, clear_value) in [
                (view.gbuffers.albedo, None),
                (view.gbuffers.normal, None),
                (view.gbuffers.third, None),
                (view.gbuffers.depth, None),
                (
                    view.lighting.light_diffuse,
                    Some([0.001, 0.001, 0.001, 0.0]),
                ),
                (view.lighting.light_specular, Some([0.0; 4])),
            ] {
                match capture_surface(
                    cmd,
                    view.surfaces.get(handle),
                    provenance_directory,
                    clear_value,
                    None,
                ) {
                    Ok(capture) => provenance_captures.push(capture),
                    Err(error) => error!(
                        surface = view.surfaces.get(handle).name(),
                        error = ?error,
                        "Failed to capture Shadowkeep buffer provenance"
                    ),
                }
            }
        }

        self.capture_shadowkeep_exposure_ab(cmd, view);

        self.clear_surface(cmd, view.shading_result, [0.0, 0.0, 0.0, 1.0]);
        self.bind_surfaces(cmd, &[view.shading_result], None);
        cmd.output_merger_set_depth_stencil_state(None, 0);
        cmd.state = PipelineState::new(Some(0), Some(0), Some(0), Some(0));
        let deferred_draw_reached = self.execute_shadowkeep_global_pipeline(
            cmd,
            &pipelines.deferred_shading_no_atm,
            "shadowkeep/deferred_shading_no_atm",
        );
        self.capture_shadowkeep_final_combine_no_film_curve(cmd, view);


        self.blit_surface(
            cmd,
            view.shading_result,
            view.output,
            true,
            "shadowkeep/final_shading",
        );
        if capture_global_lighting_ab {
            let global_lighting_ab_after_final = self.capture_shadowkeep_global_lighting_surfaces(
                cmd,
                view,
                global_lighting_ab_directory.join("after_final_shading"),
            );
            let manifest = GlobalLightingAbManifest {
                schema: "alkahest-shadowkeep-global-lighting-ab/v1",
                global_lighting_enabled: wants_global_lighting,
                global_lighting_draw_6_reached,
                global_lighting_stage_status: self
                    .shadowkeep_global_lighting_stage_status(&pipelines.global_lighting),
                positional_channel_values: self
                    .shadowkeep_global_lighting_channel_values(&pipelines.global_lighting),
                extern_values: self.shadowkeep_global_lighting_extern_values(),
                before_global_lighting: global_lighting_ab_before,
                after_global_lighting: global_lighting_ab_after,
                after_final_shading: global_lighting_ab_after_final,
            };
            if let Err(error) = manifest.write(global_lighting_ab_directory) {
                error!(error = ?error, "Failed to write Shadowkeep global-lighting A/B manifest");
            }
        }

        if capture_provenance {
            for handle in [view.shading_result, view.output] {
                match capture_surface(
                    cmd,
                    view.surfaces.get(handle),
                    provenance_directory,
                    None,
                    None,
                ) {
                    Ok(capture) => provenance_captures.push(capture),
                    Err(error) => error!(
                        surface = view.surfaces.get(handle).name(),
                        error = ?error,
                        "Failed to capture Shadowkeep buffer provenance"
                    ),
                }
            }
            let deferred_shading = self.shadowkeep_deferred_provenance(view, deferred_draw_reached);
            info!(
                diagnostic = ?deferred_shading,
                "Shadowkeep deferred shading provenance"
            );
            let light_capture = light_capture.expect("capture was initialized above");
            let lighting = ShadowkeepLightingProvenance {
                requested_feature_subscriptions: format!("{requested_feature_subscriptions:?}"),
                active_feature_subscriptions: format!(
                    "{:?}",
                    self.active_feature_renderers.load()
                ),
                render_settings: format!(
                    "exposure_scale={}, exposure_illum_relative={}, vertex_ao={}, bloom={}, \
                     volumetrics={}, shadows={}, autoexposure={}, sun_shadows={}, \
                     anti_aliasing={}, multithreading={}, hzb_culling={}",
                    self.settings().exposure_scale,
                    self.settings().exposure_illum_relative,
                    self.settings().vertex_ao,
                    self.settings().bloom,
                    self.settings().volumetrics,
                    self.settings().shadows,
                    self.settings().autoexposure,
                    self.settings().sun_shadows,
                    self.settings().anti_aliasing,
                    self.settings().multithreading,
                    self.settings().hzb_culling,
                ),
                assets_ready: asset_summary.ready,
                assets_queued: asset_summary.queued,
                assets_loading: asset_summary.loading,
                assets_failed: asset_summary.failed,
                assets_using_fallback: asset_summary.fallback,
                lighting_apply_stage_submitted,
                deferred_light_draw_indexed_calls: light_capture.draw_indexed_calls,
                local_light_technique_hashes: light_capture.technique_hashes,
            };
            let manifest = ProvenanceManifest {
                schema: "alkahest-shadowkeep-buffer-provenance/v1",
                deferred_shading,
                lighting,
                captures: provenance_captures,
            };
            if let Err(error) = manifest.write(provenance_directory) {
                error!(error = ?error, "Failed to write Shadowkeep provenance manifest");
            } else {
                info!(
                    path = %provenance_directory.join("manifest.json").display(),
                    capture_count = manifest.captures.len(),
                    "Wrote Shadowkeep buffer provenance"
                );
            }
        }
    }
    fn capture_shadowkeep_global_lighting_surfaces(
        &self,
        cmd: &CommandList,
        view: &MainView,
        directory: impl AsRef<Path>,
    ) -> Vec<crate::renderer::provenance::SurfaceProvenance> {
        let directory = directory.as_ref();
        let mut captures = Vec::new();
        for (handle, clear_value) in [
            (view.lighting.light_diffuse, Some([0.001, 0.001, 0.001, 0.0])),
            (view.lighting.light_specular, Some([0.0; 4])),
            (view.lighting.light_specular_ibl, Some([0.0; 4])),
            (view.shading_result, Some([0.0, 0.0, 0.0, 1.0])),
            (view.output, None),
        ] {
            match capture_surface(
                cmd,
                view.surfaces.get(handle),
                directory,
                clear_value,
                None,
            ) {
                Ok(capture) => captures.push(capture),
                Err(error) => error!(
                    surface = view.surfaces.get(handle).name(),
                    error = ?error,
                    "Failed to capture Shadowkeep global-lighting A/B surface"
                ),
            }
        }
        captures
    }


    fn capture_shadowkeep_final_combine_no_film_curve(
        &self,
        cmd: &mut CommandList,
        view: &MainView,
    ) {
        if !ConVars::get_flag("render.shadowkeep_final_combine_no_film_curve") {
            return;
        }
        let _ = ConVars::set(
            "render.shadowkeep_final_combine_no_film_curve",
            false.into(),
        );
        if ConVars::get_flag("render.global_lighting") {
            error!(
                "Shadowkeep final-combine diagnostic requires render.global_lighting=false; \
                 diagnostic skipped"
            );
            return;
        }

        let pipeline = &self.globals.pipelines.final_combine_no_film_curve;
        if !pipeline.is_available() {
            error!("Shadowkeep final_combine_no_film_curve pipeline is unavailable");
            return;
        }

        view.shading_result_read
            .lock()
            .update(cmd, view.surfaces.get(view.shading_result));
        let input_srv = view.shading_result_read.lock().srv.clone();
        {
            let ext = self.externs.get_mut();
            ext.postprocess.input = input_srv.into();
            ext.postprocess.res_for_input = view
                .surfaces
                .get(view.shading_result)
                .resolution_with_recip();
            ext.postprocess.output_res =
                view.surfaces.get(view.postprocess).resolution_with_recip();
        }

        self.clear_surface(cmd, view.postprocess, [0.0, 0.0, 0.0, 1.0]);
        self.bind_surfaces(cmd, &[view.postprocess], None);
        cmd.output_merger_set_depth_stencil_state(None, 0);
        cmd.state = PipelineState::new(Some(0), Some(0), Some(0), Some(0));
        let draw_6_reached = self.execute_shadowkeep_global_pipeline(
            cmd,
            pipeline,
            "shadowkeep/final_combine_no_film_curve",
        );

        let directory = Path::new("artifacts/shadowkeep-final-combine-no-film-curve");
        let capture = match capture_surface(
            cmd,
            view.surfaces.get(view.postprocess),
            directory,
            None,
            Some("unknown_transfer_float"),
        ) {
            Ok(capture) => capture,
            Err(error) => {
                error!(
                    error = ?error,
                    "Failed to capture Shadowkeep final_combine_no_film_curve output"
                );
                return;
            }
        };
        let final_combine = self.shadowkeep_final_combine_provenance(
            view,
            pipeline,
            draw_6_reached,
            "shading_result_read -> shading_result proxy",
        );
        info!(
            diagnostic = ?final_combine,
            "Shadowkeep final_combine_no_film_curve provenance"
        );
        let manifest = FinalCombineManifest {
            schema: "alkahest-shadowkeep-final-combine-no-film-curve/v1",
            final_combine,
            capture,
        };
        if let Err(error) = manifest.write(directory) {
            error!(error = ?error, "Failed to write Shadowkeep final-combine manifest");
        } else {
            info!(
                path = %directory.join("manifest.json").display(),
                "Wrote Shadowkeep final_combine_no_film_curve diagnostic"
            );
        }
    }

    fn shadowkeep_final_combine_provenance(
        &self,
        view: &MainView,
        pipeline: &Technique,
        draw_6_reached: bool,
        bound_input_srv: &str,
    ) -> FinalCombineProvenance {
        let vertex = pipeline.stage_vertex.as_ref();
        let pixel = pipeline.stage_pixel.as_ref();
        FinalCombineProvenance {
            technique: pipeline.hash.to_string(),
            vertex_shader: vertex.map(|stage| stage.shader.shader.to_string()),
            pixel_shader: pixel.map(|stage| stage.shader.shader.to_string()),
            draw_6_reached,
            vertex_expression: vertex.map(|stage| {
                format!(
                    "{:?}",
                    stage.dynamic_constants.expression_evaluation_result()
                )
            }),
            pixel_expression: pixel.map(|stage| {
                format!(
                    "{:?}",
                    stage.dynamic_constants.expression_evaluation_result()
                )
            }),
            vertex_constant_buffer_slot: vertex.map(|stage| stage.dynamic_constants.cbuffer_slot),
            vertex_constant_buffer_len: vertex
                .map(|stage| stage.dynamic_constants.constant_buffer_len()),
            pixel_constant_buffer_slot: pixel.map(|stage| stage.dynamic_constants.cbuffer_slot),
            pixel_constant_buffer_len: pixel
                .map(|stage| stage.dynamic_constants.constant_buffer_len()),
            bound_input_srv: bound_input_srv.to_owned(),
            output_rtv_format: format!(
                "{:?}",
                view.surfaces.get(view.postprocess).desc().view_format
            ),
        }
    }

    fn capture_shadowkeep_exposure_ab(&self, cmd: &mut CommandList, view: &MainView) {
        if !ConVars::get_flag("render.shadowkeep_exposure_ab") {
            return;
        }
        let _ = ConVars::set("render.shadowkeep_exposure_ab", false.into());
        if ConVars::get_flag("render.global_lighting") {
            error!(
                "Shadowkeep exposure A/B requires render.global_lighting=false; diagnostic skipped"
            );
            return;
        }

        let (production_exposure_scale, exposure_illum_relative) = {
            let ext = self.externs.get_mut();
            (ext.frame.exposure_scale, ext.frame.exposure_illum_relative)
        };
        let directory = Path::new("artifacts/shadowkeep-exposure-ab");
        let mut variants = Vec::with_capacity(2);

        for (label, exposure_scale) in [("exposure-0.05", 0.05), ("exposure-1.0", 1.0)] {
            let frame_scope = match self.write_frame_scope(cmd, Some(exposure_scale)) {
                Ok(frame_scope) => frame_scope,
                Err(error) => {
                    error!(
                        error = ?error,
                        exposure_scale,
                        "Failed to set Shadowkeep exposure diagnostic FrameScope"
                    );
                    break;
                }
            };

            self.clear_surface(cmd, view.shading_result, [0.0, 0.0, 0.0, 1.0]);
            self.bind_surfaces(cmd, &[view.shading_result], None);
            cmd.output_merger_set_depth_stencil_state(None, 0);
            cmd.state = PipelineState::new(Some(0), Some(0), Some(0), Some(0));
            let draw_6_reached = self.execute_shadowkeep_global_pipeline(
                cmd,
                &self.globals.pipelines.deferred_shading_no_atm,
                "shadowkeep/exposure_ab/deferred_shading_no_atm",
            );
            self.blit_surface(
                cmd,
                view.shading_result,
                view.output,
                true,
                "shadowkeep/exposure_ab/final_shading",
            );

            let variant_directory = directory.join(label);
            let mut captures = Vec::with_capacity(2);
            for handle in [view.shading_result, view.output] {
                match capture_surface(
                    cmd,
                    view.surfaces.get(handle),
                    &variant_directory,
                    None,
                    None,
                ) {
                    Ok(capture) => captures.push(capture),
                    Err(error) => error!(
                        surface = view.surfaces.get(handle).name(),
                        exposure_scale,
                        error = ?error,
                        "Failed to capture Shadowkeep exposure diagnostic"
                    ),
                }
            }

            variants.push(ExposureVariantProvenance {
                exposure_scale,
                frame_scope_c1: [
                    frame_scope.exposure_scale,
                    frame_scope.exposure_illum_relative_glow,
                    frame_scope.exposure_scale_for_shading,
                    frame_scope.exposure_illum_relative,
                ],
                deferred_shading: self.shadowkeep_deferred_provenance(view, draw_6_reached),
                captures,
            });
        }

        if let Err(error) = self.write_frame_scope(cmd, None) {
            error!(error = ?error, "Failed to restore production FrameScope after exposure A/B");
        }

        let manifest = ExposureAbManifest {
            schema: "alkahest-shadowkeep-exposure-ab/v1",
            production_exposure_scale,
            exposure_illum_relative,
            variants,
        };
        if let Err(error) = manifest.write(directory) {
            error!(error = ?error, "Failed to write Shadowkeep exposure A/B manifest");
        } else {
            info!(
                path = %directory.join("manifest.json").display(),
                variant_count = manifest.variants.len(),
                "Wrote Shadowkeep exposure A/B diagnostic"
            );
        }
    }

    fn shadowkeep_deferred_provenance(
        &self,
        view: &MainView,
        draw_6_reached: bool,
    ) -> DeferredShadingProvenance {
        let pipeline = &self.globals.pipelines.deferred_shading_no_atm;
        let vertex = pipeline.stage_vertex.as_ref();
        let pixel = pipeline.stage_pixel.as_ref();
        DeferredShadingProvenance {
            technique: pipeline.hash.to_string(),
            vertex_shader: vertex.map(|stage| stage.shader.shader.to_string()),
            pixel_shader: pixel.map(|stage| stage.shader.shader.to_string()),
            draw_6_reached,
            vertex_expression: vertex.map(|stage| {
                format!(
                    "{:?}",
                    stage.dynamic_constants.expression_evaluation_result()
                )
            }),
            pixel_expression: pixel.map(|stage| {
                format!(
                    "{:?}",
                    stage.dynamic_constants.expression_evaluation_result()
                )
            }),
            vertex_constant_buffer_slot: vertex.map(|stage| stage.dynamic_constants.cbuffer_slot),
            vertex_constant_buffer_len: vertex
                .map(|stage| stage.dynamic_constants.constant_buffer_len()),
            pixel_constant_buffer_slot: pixel.map(|stage| stage.dynamic_constants.cbuffer_slot),
            pixel_constant_buffer_len: pixel
                .map(|stage| stage.dynamic_constants.constant_buffer_len()),
            bound_deferred_srvs: vec![
                format!(
                    "deferred_depth -> {} proxy",
                    view.surfaces.get(view.gbuffers.depth).name()
                ),
                format!(
                    "deferred_rt0 -> {}",
                    view.surfaces.get(view.gbuffers.albedo).name()
                ),
                format!(
                    "deferred_rt1 -> {}",
                    view.surfaces.get(view.gbuffers.normal).name()
                ),
                format!(
                    "deferred_rt2 -> {}",
                    view.surfaces.get(view.gbuffers.third).name()
                ),
                format!(
                    "light_diffuse -> {}",
                    view.surfaces.get(view.lighting.light_diffuse).name()
                ),
                format!(
                    "light_specular -> {}",
                    view.surfaces.get(view.lighting.light_specular).name()
                ),
                format!(
                    "light_specular_ibl -> {}",
                    view.surfaces.get(view.lighting.light_specular_ibl).name()
                ),
            ],
            output_rtv_format: format!(
                "{:?}",
                view.surfaces.get(view.shading_result).desc().view_format
            ),
        }
    }

    fn shadowkeep_global_lighting_channel_values(
        &self,
        pipeline: &Technique,
    ) -> Vec<GlobalLightingChannelValue> {
        let mut indices = pipeline
            .all_stages()
            .into_iter()
            .filter_map(|(_, stage)| stage)
            .flat_map(|stage| {
                OpcodeIterator::new(&stage.dynamic_constants.bytecode).filter_map(
                    |(opcode, args)| {
                        (opcode == Opcode::PushGlobalChannelVector)
                            .then(|| args.first().copied())
                            .flatten()
                    },
                )
            })
            .collect::<Vec<_>>();
        indices.sort_unstable();
        indices.dedup();
        let ext = self.externs.get();
        indices
            .into_iter()
            .filter_map(|index| {
                ext.globals
                    .get(index as usize)
                    .copied()
                    .map(|value| GlobalLightingChannelValue {
                        index,
                        value: value.to_array(),
                    })
            })
            .collect()
    }

    fn shadowkeep_global_lighting_extern_values(
        &self,
    ) -> Vec<GlobalLightingExternValue> {
        let ext = self.externs.get();
        let global_lighting = &ext.global_lighting;
        vec![
            GlobalLightingExternValue { byte_offset: 0x10, value: global_lighting.unk10.to_array() },
            GlobalLightingExternValue { byte_offset: 0x30, value: global_lighting.unk30.to_array() },
            GlobalLightingExternValue { byte_offset: 0x50, value: global_lighting.unk50.to_array() },
            GlobalLightingExternValue { byte_offset: 0x70, value: global_lighting.unk70.to_array() },
            GlobalLightingExternValue { byte_offset: 0x80, value: global_lighting.unk80.to_array() },
            GlobalLightingExternValue { byte_offset: 0x90, value: [global_lighting.unk90, 0.0, 0.0, 0.0] },
            GlobalLightingExternValue { byte_offset: 0x94, value: [global_lighting.unk94, 0.0, 0.0, 0.0] },
            GlobalLightingExternValue { byte_offset: 0x98, value: [global_lighting.unk98, 0.0, 0.0, 0.0] },
            GlobalLightingExternValue { byte_offset: 0x9C, value: [global_lighting.unk9c, 0.0, 0.0, 0.0] },
            GlobalLightingExternValue { byte_offset: 0xA0, value: [global_lighting.unka0, 0.0, 0.0, 0.0] },
            GlobalLightingExternValue { byte_offset: 0xB0, value: global_lighting.unkb0.to_array() },
            GlobalLightingExternValue { byte_offset: 0xC0, value: global_lighting.unkc0.to_array() },
            GlobalLightingExternValue { byte_offset: 0xD0, value: global_lighting.unkd0.to_array() },
        ]
    }
    fn shadowkeep_global_lighting_stage_status(
        &self,
        pipeline: &Technique,
    ) -> Vec<String> {
        pipeline
            .all_stages()
            .into_iter()
            .filter_map(|(_, stage)| stage)
            .map(|stage| {
                format!(
                    "stage={} shader={} expression={:?}",
                    stage.stage.short_name(),
                    stage.shader.shader,
                    stage.dynamic_constants.expression_evaluation_result()
                )
            })
            .collect()
    }


    fn emit_shadowkeep_global_lighting_manifest(
        &self,
        pipeline: &Technique,
        draw_6_reached: bool,
    ) {
        if self
            .shadowkeep_global_lighting_manifest_emitted
            .load()
        {
            return;
        }
        self.shadowkeep_global_lighting_manifest_emitted.store(true);

        let stages = pipeline
            .all_stages()
            .into_iter()
            .filter_map(|(_, stage)| stage)
            .map(|stage| {
                let bytecode = &stage.dynamic_constants.bytecode;
                let mut global_channel_indices = OpcodeIterator::new(bytecode)
                    .filter_map(|(opcode, args)| {
                        (opcode == Opcode::PushGlobalChannelVector)
                            .then(|| args.first().copied())
                            .flatten()
                    })
                    .collect::<Vec<_>>();
                global_channel_indices.sort_unstable();
                global_channel_indices.dedup();

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
                        let offset = u32::from(
                            args.get(1).copied().unwrap_or_default(),
                        )
                        .saturating_mul(scalar_width as u32);
                        let extern_index = ExternIndex::try_from(raw_index)
                            .map(|index| format!("{index:?}"))
                            .unwrap_or_else(|_| format!("0x{raw_index:02X}"));
                        Some(GlobalLightingExternRead {
                            extern_index,
                            value_type: value_type.to_owned(),
                            byte_offset: offset,
                        })
                    })
                    .collect::<Vec<_>>();
                extern_reads.sort_by(|left, right| {
                    left.extern_index
                        .cmp(&right.extern_index)
                        .then(left.byte_offset.cmp(&right.byte_offset))
                        .then(left.value_type.cmp(&right.value_type))
                });
                let push_global_channel_vector_values = global_channel_indices
                    .iter()
                    .filter_map(|&index| {
                        self.externs
                            .globals
                            .get(index as usize)
                            .copied()
                            .map(|value| GlobalLightingChannelValue {
                                index,
                                value: value.to_array(),
                            })
                    })
                    .collect();
                let resource_slots = |opcode| {
                    let mut slots = OpcodeIterator::new(bytecode)
                        .filter_map(|(candidate, args)| {
                            (candidate == opcode)
                                .then(|| args.first().map(|encoded| encoded & 0x1f))
                                .flatten()
                        })
                        .collect::<Vec<_>>();
                    slots.sort_unstable();
                    slots.dedup();
                    slots
                };
                let sampler_slots = resource_slots(Opcode::PopSamplerState)
                    .into_iter()
                    .map(usize::from)
                    .collect();
                let texture_slots = resource_slots(Opcode::PopTextureView)
                    .into_iter()
                    .map(u32::from)
                    .collect();


                let translated_expression_disassembly =
                    match expression_vm::disassemble(bytecode) {
                        Ok(lines) => lines.join("\n"),
                        Err(error) => format!("ERROR: {error:#}"),
                    };
                GlobalLightingDependencyStage {
                    stage: stage.stage.short_name().to_owned(),
                    shader: stage.shader.shader.to_string(),
                    translated_expression_disassembly,
                    push_global_channel_vector_values,
                    push_global_channel_vector_indices: global_channel_indices,
                    extern_reads,
                    constant_buffer_slot: stage.dynamic_constants.cbuffer_slot,
                    constant_buffer_len: stage.dynamic_constants.constant_buffer_len(),
                    sampler_slots,
                    texture_slots,
                    expression_evaluation_result: format!(
                        "{:?}",
                        stage.dynamic_constants.expression_evaluation_result()
                    ),
                }
            })
            .collect();
        let manifest = GlobalLightingDependencyManifest {
            schema: "alkahest-shadowkeep-global-lighting-dependencies/v1",
            technique: pipeline.hash.to_string(),
            draw_6_reached,
            stages,
        };
        if let Err(error) = manifest.write(Path::new("artifacts/shadowkeep-global-lighting")) {
            error!(error = ?error, "Failed to write Shadowkeep global-lighting dependency manifest");
        } else {
            info!(
                path = "artifacts/shadowkeep-global-lighting/manifest.json",
                draw_6_reached,
                "Wrote Shadowkeep global-lighting dependency manifest"
            );
        }
    }

    /// Arrivals fullscreen techniques use a six-vertex strip. The shared
    /// post-BL helper emits four vertices, which binds successfully but leaves
    /// these legacy passes with incomplete or empty screen coverage.
    fn execute_shadowkeep_global_pipeline(
        &self,
        cmd: &mut CommandList,
        pipeline: &Technique,
        name: &str,
    ) -> bool {
        cmd_event_span!(cmd, &format!("[{name}]"));
        if let Err(error) = pipeline.bind(cmd) {
            error!("Failed to run {name}: {error}");
            return false;
        }
        cmd.flush_states();
        cmd.set_input_topology(PrimitiveType::TriangleStrip);
        cmd.draw(6, 0);
        true
    }

    fn submit_shadowkeep_geometry_preview(&self, cmd: &mut CommandList, view: &MainView) {
        let output = view.surfaces.get(view.output);
        cmd.clear_render_target_view(output.rtv.as_ref().unwrap(), &[0.0, 0.0, 0.0, 1.0]);
        output.bind_single(cmd);
        cmd.output_merger_set_depth_stencil_state(None, 0);
        cmd.state = PipelineState::new(Some(0), Some(0), Some(0), Some(0));
        cmd.flush_states();
        cmd.vertex_set_shader(Some(&self.debug_vs));
        cmd.pixel_set_shader(Some(&self.debug_ps));
        cmd.set_input_topology(PrimitiveType::TriangleStrip);
        cmd.pixel_set_shader_resources(
            0,
            &[
                view.surfaces.get(view.gbuffers.albedo).srv(0),
                view.surfaces.get(view.gbuffers.normal).srv(0),
                view.surfaces.get(view.gbuffers.third).srv(0),
                Some(&view.gbuffers.depth_proxy.lock().srv),
            ],
        );
        cmd.draw(4, 0);
    }

    fn submit_view_overdraw(self: &Arc<Self>, cmd: &mut CommandList, view: &MainView) {
        self.submit_gbuffer_generation(cmd, view, None);

        self.clear_surface(cmd, view.postprocess, [0.0, 0.0, 0.0, 1.0]);
        self.bind_surfaces(cmd, &[view.postprocess], None);

        {
            cmd.state = PipelineState::new(Some(2), Some(0), Some(0), Some(0));
            cmd.state_override = PipelineState::new(Some(2), Some(0), Some(0), Some(0));
            let ShaderModule::Pixel(m) = &self
                .globals
                .pipelines
                .overdraw_visualizer
                .stage_pixel
                .as_ref()
                .expect("overdraw_visualizer is missing it's pixel stage")
                .shader_module
            else {
                panic!("overdraw_visualizer is missing it's pixel stage");
            };

            cmd.set_override_pixel_shader(m.clone());
            self.submit_stage_parallel_linear(
                cmd,
                View::MAIN,
                RenderStage::GenerateGbuffer,
                FeatureRendererSubscription::all(),
            );
            cmd.set_override_pixel_shader(None);
        }

        {
            cmd.vertex_set_shader(&self.common.blit_vs);
            cmd.pixel_set_shader(&self.common.overdraw_ps);

            let surf_postprocess = view.surfaces.get(view.postprocess);
            let surf_output = view.surfaces.get(view.output);
            surf_output.bind_single(cmd);
            cmd.pixel_set_shader_resources(0, &[surf_postprocess.srv(0)]);

            cmd.state = PipelineState::new(Some(0), Some(0), Some(0), Some(0));
            cmd.flush_states();
            cmd.set_input_topology(PrimitiveType::TriangleStrip);

            cmd.draw(4, 0);
        }
    }

    fn prepare_externs(&self, cmd: &mut CommandList, view: &View) {
        let fb_res = view.framebuffer_resolution();

        let misc = &self.frame_packet.read().misc;

        // let cam_view = Mat4::from_cols(
        //     [-0.962532818, -0.027713167, -0.269745320, 0.000000000].into(),
        //     [-0.271165162, 0.098371163, 0.957492828, 0.000000000].into(),
        //     [0.000000000, 0.994763792, -0.102200322, 0.000000000].into(),
        //     [15.103929520, -31.395317078, -47.990650177, 1.000000000].into(),
        // );
        // let cam_proj = Mat4::from_cols(
        //     [0.827271998, 0.000000000, 0.000000000, 0.000000000].into(),
        //     [0.000000000, 1.470705628, 0.000000000, 0.000000000].into(),
        //     [0.000000000, 0.000000000, 0.000002623, -1.000000000].into(),
        //     [0.000000000, 0.000000000, 0.150000393, 0.000000000].into(),
        // );

        let ext = self.externs.get_mut();
        ext.view
            .update(view.world_to_camera, view.camera_to_projective, fb_res);

        let global_tex = &self.globals.textures;

        *ext.frame = externs::Frame {
            game_time: misc.time,   //self.start_time.elapsed().as_secs_f32();
            render_time: misc.time, //self.start_time.elapsed().as_secs_f32();
            delta_game_time: misc.delta_time,
            unk10: misc.time_of_day,
            exposure_time: 0.016666668,
            exposure_scale: view.settings().exposure_scale,
            exposure_illum_relative: view.settings().exposure_illum_relative,
            specular_tint_lookup: global_tex.specular_tint_lookup.view.clone().into(),
            specular_lobe_lookup: global_tex.specular_lobe_lookup.view.clone().into(),
            specular_lobe_3d_lookup: global_tex.specular_lobe_3d_lookup.view.clone().into(),
            iridescence_lookup: global_tex.iridescence_lookup.view.clone().into(),
            ..*ext.frame.clone()
        };

        // TODO(cohae): Reconfirm the offset of iridescence lookup
        // let irr_lookup = &self.globals.textures.iridescence_lookup;
        // ext.frame.iridescence_lookup = irr_lookup.view.clone().into();

        let near = Camera::NEAR;
        let far = Camera::FAR;
        ext.deferred.depth_constants = vec4(
            1.0 / far,
            (far - near) / (far * near),
            0.00000000,
            0.00000000,
        );

        *ext.water_displacement = externs::WaterDisplacement {
            unk00: global_tex.water_displacement_unk00.view.clone().into(),
            unk08: global_tex.water_displacement_unk08.view.clone().into(),
            unk10: 0.045,
            unk14: 1.0, // value unknown, dont know where this is used
            unk18: 0.0,
            unk1c: 20.0,
            unk20: 600.0,
            unk24: 0.5,
            unk28: 2.0,
            unk2c: 0.0,
            unk30: 7.7,
        };

        if self.era() == crate::renderer::RendererEra::Shadowkeep {
            // The preserved pass consumes the positional global table
            // directly.  Keep its ABI defaults; no later-era channel names
            // are meaningful for this renderer.
            *ext.global_lighting = GlobalLighting {
                unk08: self.gpu.placeholder_white.view.clone().into(),
                // The old pixel expression reads this field at 0x10 as its
                // direct-light color. Package defaults leave the associated
                // skybox color/intensity channels zero until unavailable
                // activity automation runs, so retain the neutral ABI input.
                unk10: Vec4::ONE,
                // Preserved Arrivals directional defaults at 0x30/0x50.
                unk30: Vec4::NEG_Z,
                unk50: Vec4::ZERO,
                // Offsets 0x70/0x80 are the matching up/down ambient colors.
                // Their package intensities are 1, while their colors likewise
                // await absent automation; neutral ABI colors preserve the old
                // pass without importing later-era hashed-name lookup.
                unk70: Vec4::ONE,
                unk80: Vec4::ONE,
                unk94: -0.5,
                ..Default::default()
            };
            let white: TextureView = self.gpu.placeholder_white.view.clone().into();
            ext.shadow_mask.unk00 = white.clone();
            ext.shadow_mask.unk08 = white.clone();
            ext.shadow_mask.unk10 = white;
        } else {
            *ext.global_lighting = GlobalLighting {
                unk08: self.gpu.placeholder_white.view.clone().into(),
                unk10: ext.get_global_channel_by_name("sun_color")
                    * ext.get_global_channel_by_name("sun_intensity").x,
                unk30: ext.get_global_channel_by_name("sun_light_direction"),
                unk50: ext.get_global_channel_by_name("sun_ambient_direction"),
                unk70: ext.get_global_channel_by_name("up_ambient_color")
                    * ext.get_global_channel_by_name("up_ambient_intensity").x,
                unk80: ext.get_global_channel_by_name("down_ambient_color")
                    * ext.get_global_channel_by_name("down_ambient_intensity").x,
                unk90: ext.get_global_channel_by_name("up_ambient_sharpness").x,
                unk94: ext.get_global_channel_by_name("down_ambient_sharpness").x,
                unka0: 0.20,
                unkb0: vec4(0.01, 0.01, -0.5, -0.5),
                unkc0: vec4(0.02, -2.0, 0.0, 0.0),
                unkd0: vec4(0.00333, -2.33333, 0.00, 0.00),
                ..Default::default()
            };
        }

        // ext.shadow_mask.unk00 = self.gpu.placeholder_white.view.clone().into();
        // ext.shadow_mask.unk08 = self.lighting.ssao.into();
        // ext.shadow_mask.unk10 = view.gbuffers.uber_depth_half.into();

        // if let Some(vao_srv) = view.surfaces.get(self.lighting.vertex_ao).srv.clone() {
        //     ext.cubemaps.vertex_ao = vao_srv.into();
        // }

        // ext.atmosphere.unk38 = self.common.temporary_depth_lookup.view.clone().into();
        // ext.atmosphere.unk88 = self.common.temporary_atmos.view.clone().into();

        // TODO(cohae): Most of these need to be verified, currently they are just shifted +0x70 from pre-BL
        ext.atmosphere.unk110 = if self.era() == crate::renderer::RendererEra::Shadowkeep {
            ext.global_lighting.unk30
        } else {
            ext.get_global_channel_by_name("sun_light_direction")
        };

        // Sun
        ext.atmosphere.unk140 = ext.get_global_channel_by_id(0x56007c7);
        ext.atmosphere.unk150 = ext.get_global_channel_by_id(0x4aa1bef5).x;
        ext.atmosphere.unk154 = ext.get_global_channel_by_id(0x9859daf1).x;
        // ext.atmosphere.unk158 = ext.get_global_channel_by_id(0xc876108a).x;
        // ext.atmosphere.unk15c = ext.get_global_channel_by_id(0xe111d856).x;
        ext.atmosphere.unk160 = ext.get_global_channel_by_id(0xf853533c).x;

        // Fog
        ext.atmosphere.unk164 = ext.get_global_channel_by_id(0xed4bb08a).x;
        ext.atmosphere.unk168 = ext.get_global_channel_by_id(0x9e769ed2).x;
        ext.atmosphere.unk16c = ext.get_global_channel_by_id(0x49fbbce1).x;
        ext.atmosphere.unk170 = ext.get_global_channel_by_id(0x94d8ecdc).x;
        // ext.atmosphere.unk174 = ext.get_global_channel_by_id(0xbd2c7fe8).x;
        ext.atmosphere.unk180 = ext.get_global_channel_by_id(0x9ec7a5e8);
        ext.atmosphere.unk190 = ext.get_global_channel_by_id(0xb630810b).x;
        ext.atmosphere.unk194 = ext.get_global_channel_by_id(0x3eeacb23).x;
        ext.atmosphere.unk198 = ext.get_global_channel_by_id(0x7e92eb31).x;
        // ext.atmosphere.unk19c = ext.get_global_channel_by_id(0x9f4cb78f).x;

        // ext.atmosphere.unk1a0 = ext.get_global_channel_by_id(0x3e9cb6ed);
        // ext.atmosphere.unk1b0 = ext.get_global_channel_by_id(0x5fc9836).x;
        ext.atmosphere.unk1b4 = ext.get_global_channel_by_id(0xe283fbe0).x;
        ext.atmosphere.unk1b8 = ext.get_global_channel_by_id(0x5f3b8491).x;
        ext.atmosphere.unk1bc = ext.get_global_channel_by_id(0x79f2e305).x;
        ext.atmosphere.unk1c0 = ext.get_global_channel_by_id(0x62e4542e).x;
        ext.atmosphere.unk1c4 = ext.get_global_channel_by_id(0x949768cf).x;
        ext.atmosphere.unk1d0 = ext.get_global_channel_by_id(0xd9a2d8a3);
        ext.atmosphere.unk1e0 = ext.get_global_channel_by_id(0xd8281393).x;
        ext.atmosphere.unk1e4 = ext.get_global_channel_by_id(0x4da73ca7).x;
        ext.atmosphere.unk1e8 = ext.get_global_channel_by_id(0xe685c537).x;
        ext.atmosphere.unk1ec = ext.get_global_channel_by_id(0xe4a1bf60).x;
        // ext.atmosphere.unk1f0 = ext.get_global_channel_by_id(0x63d92f7).x;
        // ext.atmosphere.unk1f4 = ext.get_global_channel_by_id(0x49864a42).x;

        // The current fixed 37-register transparent setup belongs to the
        // post-BL scope layout. Shadowkeep owns a shorter, differently laid
        // out scope; leave its serializer-provided defaults intact until its
        // dedicated transparent pass is installed.
        if self.era() == crate::renderer::RendererEra::Current {
            self.globals
                .scopes
                .transparent_advanced
                .write_initial_constants(
                    cmd,
                    &[
                        vec4(0.00227, 0.00896, 0.32782, 0.6419),
                        vec4(0.0026, 4.86115, 0.00198, 0.00002),
                        vec4(0.9158, 233.93063, 0.51102, 0.08905),
                        vec4(147.09909, 0.55492, 0.52397, 0.00),
                        vec4(0.00, 0.64794, 0.14063, 0.01563),
                        Vec4::ZERO, // vec4(0.58584, 0.58584, 0.58584, 0.58584),
                        vec4(1.38137, 2.08133, 0.85451, 0.4165),
                        vec4(0.90933, 0.90933, 0.90933, 0.90933),
                        vec4(132.92885, 66.40444, 56.85342, 0.00),
                        vec4(132.92885, 66.40444, 1000.00, 0.0001),
                        vec4(131.92885, 65.40444, 55.85342, 0.67843),
                        vec4(131.92885, 65.40444, 999.00, 5.50),
                        vec4(0.00, 0.50, 25.57599, 0.00),
                        vec4(0.00, 0.00, 0.00, 0.00),
                        vec4(0.025, 10000.00, -9999.00, 1.00),
                        vec4(1.00, 1.00, 1.00, 0.00),
                        vec4(0.00, 0.00, 0.00, 0.00),
                        vec4(10.92799, 7.10136, 6.25467, 0.00),
                        vec4(0.00376, 0.00, 0.00, 0.00),
                        vec4(0.00, 0.00753, 0.00, 0.00),
                        vec4(0.00, 0.00, 0.01759, 0.00),
                        vec4(-1.13485, 6.87303, -0.33715, 1.00),
                        vec4(0.00, 0.00, 0.00, 0.00),
                        vec4(0.00, 0.00, 0.00, 0.00),
                        vec4(0.00, 0.00, 0.00, 0.00),
                        vec4(0.00, 0.00, 0.00, 0.00),
                        vec4(0.00, 0.00, 0.00, 0.00),
                        vec4(0.00, 0.00, 0.00, 0.00),
                        vec4(0.00, 0.00, 0.00, 0.00),
                        vec4(0.00, 0.00, 0.00, 0.00),
                        vec4(0.00, 0.00, 0.00, 0.00),
                        vec4(0.00, 0.00, 0.00, 0.00),
                        vec4(0.00, 0.00, 0.00, 0.00),
                        vec4(0.00, 0.00, 0.00, 0.00),
                        vec4(0.00, 0.00, 0.00, 0.00),
                        vec4(0.00, 0.00, 0.00, 0.00),
                        vec4(1.00, 0.00, 0.00, 0.00),
                    ],
                )
                .expect("Failed to write transparent_advanced initial constants");
        }

        let _ = self.write_frame_scope(cmd, None);

        if let ViewKind::Main(v) = &view.kind {
            self.prepare_main_view_externs(v);
        }
    }

    fn write_frame_scope(
        &self,
        cmd: &mut CommandList,
        exposure_override: Option<f32>,
    ) -> anyhow::Result<FrameScope> {
        let misc_guard = self.frame_packet.read();
        let misc = &misc_guard.misc;
        let ext = self.externs.get_mut();
        let exposure_scale = exposure_override.unwrap_or(ext.frame.exposure_scale);
        let frame_scope = FrameScope {
            game_time: ext.frame.game_time,
            render_time: ext.frame.render_time,
            delta_game_time: ext.frame.delta_game_time,
            exposure_time: ext.frame.exposure_time,
            exposure_scale,
            exposure_illum_relative_glow: ext.frame.exposure_illum_relative * 16.0,
            exposure_scale_for_shading: exposure_scale,
            exposure_illum_relative: ext.frame.exposure_illum_relative,
            random_seed_scales: vec4(
                (misc.time * 60.0 + 33.75) * 1.258699,
                (misc.time * 60.0 + 60.0) * 0.9583125,
                (misc.time * 60.0 + 60.0) * 8.789123,
                (misc.time * 60.0 + 33.75) * 2.311535,
            ),
            unk3: vec4(0.5, 0.5, 0.0, 0.0),
            unk4: vec4(1.0, 1.0, 0.0, 1.0),
            unk5: vec4(0.00, -f32::NAN, 512.00, 0.00),
            unk6: Vec4::ONE,
        };
        self.globals
            .scopes
            .frame
            .write_initial_constants(cmd, frame_scope.to_array().as_ref())?;
        self.globals.scopes.frame.bind(cmd)?;
        Ok(frame_scope)
    }

    fn prepare_main_view_externs(&self, view: &MainView) {
        let fb_res = view.surfaces.framebuffer_resolution();
        let ext = self.externs.get_mut();
        let misc = &self.frame_packet.read().misc;

        // ext.deferred.gbuffer_resolution_scale_offset =
        //     vec4(fb_res.0 as f32, fb_res.1 as f32, 0.0, 0.0);
        ext.deferred.deferred_depth = view.gbuffers.depth_proxy.lock().srv.clone().into();
        ext.deferred.deferred_rt0 = view.gbuffers.albedo.into();
        ext.deferred.deferred_rt1 = view.gbuffers.normal.into();
        ext.deferred.deferred_rt2 = view.gbuffers.third.into();

        ext.deferred.light_diffuse = view.lighting.light_diffuse.into();
        ext.deferred.light_specular = view.lighting.light_specular.into();
        ext.deferred.light_specular_ibl = view.lighting.light_specular_ibl.into();

        ext.deferred.sky_hemisphere_mips = self.common.temporary_sky_hemisphere.view.clone().into();

        ext.decal.depth_read = view.gbuffers.depth_proxy.lock().srv.clone().into();
        ext.decal.normals_read = view.gbuffers.normal_read.into();
        ext.decal.depth_constants = ext.deferred.depth_constants;
        ext.decal.unk30 = vec4(fb_res.0 as f32, fb_res.1 as f32, 0.0, 0.0);

        if self.era() == crate::renderer::RendererEra::Shadowkeep {
            // The preserved Arrivals lighting pass seeds all three shadow
            // inputs with white until its optional shadow/SSAO producers run.
            // Do the same for the admitted fullscreen pass; binding the
            // modern mask/depth surfaces here makes a null legacy input look
            // like a valid but empty light buffer.
            let white: TextureView = self.gpu.placeholder_white.view.clone().into();
            ext.shadow_mask.unk00 = white.clone();
            ext.shadow_mask.unk08 = white.clone();
            ext.shadow_mask.unk10 = white;
            ext.shadow_mask.unk20 = view.surfaces.get(view.shadow_mask).resolution_with_recip();
        } else {
            ext.shadow_mask.unk00 = view.shadow_mask.into();
            // ext.shadow_mask.unk08 = view.lighting.ssao.into();
            ext.shadow_mask.unk10 = view.gbuffers.uber_depth_half.into();
            ext.shadow_mask.unk20 = view.surfaces.get(view.shadow_mask).resolution_with_recip();
        }

        *ext.atmosphere = externs::Atmosphere {
            time_of_day_normalized: misc.time_of_day,
            unk80: misc.atmosphere.atmosphere_lookup_vertical.clone().into(),
            unke0: view.atmosphere.sky_lookup_near.into(),
            unkf0: view.atmosphere.sky_lookup_far.into(),
            unk1ec: 150.0,
            unk1c4: 600.0,

            // From EDZ nighttime capture
            unk150: -0.97563,
            unk154: 0.00386,
            unk1b4: 0.94444,
            unk1b8: 1.00,
            unk1bc: 0.00427,
            // time_of_day_normalized: 0.05198,
            unk110: vec4(0.99605, 0.03099, -0.0832, 0.00),
            sky_lookup_resolution: view
                .surfaces
                .get(view.atmosphere.sky_lookup_near)
                .resolution_with_recip(),
            unk1d0: vec4(0.00, 0.00, 0.00, 0.00),
            unk1e0: 0.00,
            unk1e4: 0.00386,
            unk1e8: 0.00427,

            ..Default::default()
        };

        ext.screen_area = ScreenArea {
            unk00: view.shading_result_read.lock().srv.clone().into(),
            unk30: TextureView::None, // health overlay
            unk38: self.common.default_lut.view.clone().into(), // LUT
            unk40: if view.settings.bloom {
                view.bloom.bloom_final.into()
            } else {
                self.common.temporary_bloom.view.clone().into()
            }, // bloom
            unk48: view.lighting.distortion.into(), // distortion
            unk58: self.common.temporary_vignette.view.clone().into(), // vignette
            unk7c: 0.9968,

            // unk80: 0.9968, // Skydock IV
            unk80: 0.1, // Orbit

            unk90: vec4(32.0, 1024.0, 0.0, 0.0),
            unka0: vec4(0.03125, -5.0, 14.0, 2.5),
            unkb0: 0.5,
            unkb4: 2.0,
            unke0: vec4(0.25, -0.225, 0.40, 0.96),
            unkf0: vec4(0.13281, 0.23611, 0.00, 0.00), // distortion related
            // unkf0: Vec4::ZERO,
            unk140: 0.05,
            unk150: vec4(0.3, 0.5, 0.0, 0.02),
            unk160: vec4(0.3, 0.5, 0.0, 0.5),
            ..Default::default()
        }
        .into();

        let depth_res = view.surfaces.get(view.gbuffers.depth).resolution();
        ext.uber_depth = UberDepth {
            original_depth: view.gbuffers.depth_proxy.lock().srv.clone().into(),
            unk30: view.gbuffers.uber_depth_half.into(),
            unk40: view.gbuffers.uber_depth_quarter.into(),
            unk50: ext.deferred.depth_constants,
            unk70: vec4(0.0, 0.0, depth_res.0 as f32, depth_res.1 as f32),
            ..Default::default()
        }
        .into();

        ext.cubemaps.unk00 = view.lighting.vertex_ao.into();

        *ext.transparent = externs::Transparent {
            unk00: view.atmosphere.sky_lookup_near.into(),
            unk10: view.atmosphere.sky_lookup_far.into(),
            // unk00: todo!(), // t11, Atmosphere (near?)
            // unk08: todo!(), // t12, Atmosphere (3x2)
            // unk10: todo!(), // t13, Atmosphere (far?)
            // unk18: todo!(), // t14, 3d lightprobe
            unk20: self.common.temporary_depth_angle_lookup.view.clone().into(), // t15
            // unk28: todo!(), // t16, 3d lightprobe
            // unk30: todo!(), // t17, 3d lightprobe
            // unk38: todo!(), // t18, 3d lightprobe
            // unk40: todo!(), // t19, 3d lightprobe
            // unk48: todo!(), // t20
            // unk50: todo!(), // t21
            // unk58: todo!(), // t22
            // unk60: todo!(), // t23
            unk70: vec4(0.22882, 0.00, 1.00, 45.00),
            unk80: vec4(0.00, 0.00, 1.17485, 2.86546),
            unk90: vec4(0.00, 0.00, 2.10913, 5.14044),
            unka0: vec4(0.00, 0.00, 3.46762, 8.41667),
            unkb0: vec4(0.00, 0.00, 0.00, 0.00),
            ..Default::default()
        };
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum DebugPipeline {
    Shaded,
    ShadedNoAtm,
    ShadedNoSun,
    ShadingOnly,

    Albedo,
    Smoothness,
    Metalness,
    AmbientOcclusion,
    Emission,
    EmissionIntensity,
    Transmission,
    Overcoat,

    DepthEdges,
    WorldNormal,
    Overdraw,

    LightDiffuse,
    LightSpecular,
}

impl DebugPipeline {
    pub fn is_shaded(&self) -> bool {
        matches!(
            self,
            DebugPipeline::Shaded
                | DebugPipeline::ShadedNoAtm
                | DebugPipeline::ShadedNoSun
                | DebugPipeline::ShadingOnly
        )
    }

    pub fn has_atmosphere(&self) -> bool {
        matches!(self, DebugPipeline::Shaded | DebugPipeline::ShadedNoSun)
    }

    pub fn has_sun(&self) -> bool {
        matches!(self, DebugPipeline::Shaded | DebugPipeline::ShadedNoAtm)
    }

    pub fn aa_enabled(&self) -> bool {
        self.is_shaded() || matches!(self, DebugPipeline::DepthEdges | DebugPipeline::WorldNormal)
    }
}

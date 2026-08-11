use std::sync::Arc;

use alkahest_core::convar::ConVars;
use alkahest_data::tfx::{
    FeatureRendererSubscription, PipelineState, RenderStage, TfxFeatureRenderer,
};
use glam::Vec4;

use super::Renderer;
use crate::{
    cmd_event_span,
    gpu::command_list::CommandList,
    renderer::submit::geometry::GeometryCommandLists,
    tfx::{
        externs::{TextureView, Transparent},
        view::{MainView, View},
    },
};

impl Renderer {
    pub(super) fn submit_transparent(
        self: &Arc<Self>,
        cmd: &mut CommandList,
        view: &MainView,
        _geo: Option<&GeometryCommandLists>,
    ) {
        {
            {
                let ext = &mut self.externs.get_mut();
                ext.deferred.deferred_depth = view.gbuffers.depth_proxy.lock().srv.clone().into();
                ext.view
                    .derive_matrices(self.surfaces().get(view.shading_result).resolution());
            }
            self.globals.scopes.view.bind(cmd).unwrap();
            self.globals.scopes.transparent.bind(cmd).unwrap();
            self.globals.scopes.transparent_advanced.bind(cmd).unwrap();

            cmd_event_span!(cmd, "decals_additive");
            let _gpuscope = self.profiler.scope(cmd, "decals_additive");
            self.bind_surfaces(cmd, &[view.shading_result], Some(view.gbuffers.depth));

            cmd.state = PipelineState::new(Some(8), Some(15), Some(2), Some(1));
            cmd.flush_states();
            self.submit_stage_parallel_apply(
                cmd,
                View::MAIN,
                RenderStage::DecalsAdditive,
                FeatureRendererSubscription::all(),
            );
        }
        {
            cmd_event_span!(cmd, "transparents");
            let _gpuscope = self.profiler.scope(cmd, "transparents");

            cmd.state = PipelineState::new(Some(8), Some(15), Some(2), Some(1));

            if self.settings().multithreading {
                self.submit_stage_parallel_linear(
                    cmd,
                    View::MAIN,
                    RenderStage::Transparents,
                    FeatureRendererSubscription::all_but(TfxFeatureRenderer::Water),
                );
            } else {
                self.submit_stage(
                    cmd,
                    View::MAIN,
                    RenderStage::Transparents,
                    FeatureRendererSubscription::all_but(TfxFeatureRenderer::Water),
                );
            }
        }

        {
            cmd_event_span!(cmd, "distortion");
            let _gpuscope = self.profiler.scope(cmd, "distortion");

            {
                let distortion = view.surfaces.get(view.lighting.distortion);
                let externs = &mut self.externs.get_mut();
                externs.view.derive_matrices(distortion.resolution());
                externs.deferred.deferred_depth = view.gbuffers.uber_depth_half.into();
            }
            self.globals.scopes.view.bind(cmd).unwrap();

            self.clear_surface(cmd, view.lighting.distortion, [0., 0., 0., 0.]);
            self.bind_surfaces(
                cmd,
                &[view.lighting.distortion],
                Some(view.gbuffers.depth_half),
            );

            cmd.state = PipelineState::new(Some(8), Some(15), Some(2), Some(1));
            cmd.flush_states();
            self.submit_stage(
                cmd,
                View::MAIN,
                RenderStage::Distortion,
                FeatureRendererSubscription::all(),
            );
        }

        // Rebind full resolution depth buffer
        {
            let output = view.surfaces.get(view.output);
            let externs = &mut self.externs.get_mut();
            externs.view.derive_matrices(output.resolution());
            externs.deferred.deferred_depth = view.gbuffers.depth.into();
        }
        self.globals.scopes.view.bind(cmd).unwrap();
    }

    pub(super) fn submit_shadowkeep_sky_objects(&self, cmd: &mut CommandList, view: &MainView) {
        cmd_event_span!(cmd, "shadowkeep/sky_objects");
        let _gpu_span = self.profiler.scope(cmd, "shadowkeep/sky_objects");
        let sky_feature = FeatureRendererSubscription::SKY_TRANSPARENT;
        let capture_sky_submission = ConVars::get_flag("render.shadowkeep_sky_diagnostics")
            || ConVars::get_flag("render.shadowkeep_sky_objects_ab");
        let has_decals_additive = self.has_stage_objects(RenderStage::DecalsAdditive, sky_feature);
        let has_transparents = self.has_stage_objects(RenderStage::Transparents, sky_feature);
        if !has_decals_additive && !has_transparents {
            return;
        }

        view.shading_result_read
            .lock()
            .update(cmd, view.surfaces.get(view.shading_result));

        {
            let ext = self.externs.get_mut();
            ext.deferred.deferred_depth = view.gbuffers.depth_proxy.lock().srv.clone().into();
            ext.view
                .derive_matrices(view.surfaces.get(view.shading_result).resolution());

            let shading_read: TextureView = view.shading_result_read.lock().srv.clone().into();
            *ext.transparent = Transparent {
                unk00: view.atmosphere.sky_lookup_far.into(),
                unk08: view.atmosphere.sky_lookup_far.into(),
                unk10: view.atmosphere.sky_lookup_near.into(),
                unk18: view.atmosphere.sky_lookup_near.into(),
                unk20: self.gpu.placeholder_grey.view.clone().into(),
                unk28: self.gpu.placeholder_light_grey.view.clone().into(),
                unk30: self.gpu.placeholder_light_grey.view.clone().into(),
                unk38: self.gpu.placeholder_light_grey.view.clone().into(),
                unk40: self.gpu.placeholder_light_grey.view.clone().into(),
                unk48: shading_read.clone(),
                unk50: self.gpu.placeholder_black.view.clone().into(),
                unk58: self.gpu.placeholder_light_grey.view.clone().into(),
                unk60: shading_read,
                // The modern main-view setup writes post-BL atmospheric coefficients here.
                // Arrivals initialized these legacy transparent vectors to one; retaining
                // the modern zeroed `unkb0.zw` divides the sky-fog shader by zero.
                unk70: Vec4::ONE,
                unk80: Vec4::ONE,
                unk90: Vec4::ONE,
                unka0: Vec4::ONE,
                unkb0: Vec4::ONE,
                ..(*ext.transparent).clone()
            };
        }

        self.globals.scopes.view.bind(cmd).unwrap();
        self.globals.scopes.transparent.bind(cmd).unwrap();
        self.bind_surfaces(cmd, &[view.shading_result], Some(view.gbuffers.depth));

        // Submit the authored stage even when this collection has no additive parts;
        // this preserves pass ordering and makes the zero-draw stage explicit.
        cmd.state = PipelineState::new(Some(8), Some(15), Some(2), Some(1));
        cmd.flush_states();
        self.submit_stage_authored_order(cmd, View::MAIN, RenderStage::DecalsAdditive, sky_feature);
        if capture_sky_submission {
            crate::renderer::provenance::record_shadowkeep_sky_objects_submission(
                RenderStage::DecalsAdditive,
            );
            tracing::trace!("Shadowkeep sky objects reached DecalsAdditive submission");
        }

        view.shading_result_read
            .lock()
            .update(cmd, view.surfaces.get(view.shading_result));
        {
            let read: TextureView = view.shading_result_read.lock().srv.clone().into();
            let ext = self.externs.get_mut();
            ext.transparent.unk48 = read.clone();
            ext.transparent.unk60 = read;
        }
        cmd.state = PipelineState::new(Some(8), Some(15), Some(2), Some(1));
        cmd.flush_states();
        if has_transparents {
            self.submit_stage_authored_order(
                cmd,
                View::MAIN,
                RenderStage::Transparents,
                sky_feature,
            );
            if capture_sky_submission {
                crate::renderer::provenance::record_shadowkeep_sky_objects_submission(
                    RenderStage::Transparents,
                );
                tracing::trace!("Shadowkeep sky objects reached Transparents submission");
            }
        }
    }

    /// Immediate Arrivals transparent submission. This shares the proven
    /// legacy `Transparent` extern layout with the map-authored sky path, but
    /// deliberately excludes SkyTransparent and Water: sky owns authored
    /// ordering and water requires its separate reflection producer.
    pub(super) fn submit_shadowkeep_transparents(&self, cmd: &mut CommandList, view: &MainView) {
        let feature = FeatureRendererSubscription::all_but(TfxFeatureRenderer::SkyTransparent)
            .without(TfxFeatureRenderer::Water);
        if !self.has_stage_objects(RenderStage::DecalsAdditive, feature)
            && !self.has_stage_objects(RenderStage::Transparents, feature)
        {
            return;
        }

        cmd_event_span!(cmd, "shadowkeep/transparents");
        let _gpu_span = self.profiler.scope(cmd, "shadowkeep/transparents");
        view.shading_result_read
            .lock()
            .update(cmd, view.surfaces.get(view.shading_result));
        {
            let ext = self.externs.get_mut();
            ext.deferred.deferred_depth = view.gbuffers.depth_proxy.lock().srv.clone().into();
            ext.view
                .derive_matrices(view.surfaces.get(view.shading_result).resolution());
            let shading_read: TextureView = view.shading_result_read.lock().srv.clone().into();
            *ext.transparent = Transparent {
                unk00: view.atmosphere.sky_lookup_far.into(),
                unk08: view.atmosphere.sky_lookup_far.into(),
                unk10: view.atmosphere.sky_lookup_near.into(),
                unk18: view.atmosphere.sky_lookup_near.into(),
                unk20: self.gpu.placeholder_grey.view.clone().into(),
                unk28: self.gpu.placeholder_light_grey.view.clone().into(),
                unk30: self.gpu.placeholder_light_grey.view.clone().into(),
                unk38: self.gpu.placeholder_light_grey.view.clone().into(),
                unk40: self.gpu.placeholder_light_grey.view.clone().into(),
                unk48: shading_read.clone(),
                unk50: self.gpu.placeholder_black.view.clone().into(),
                unk58: self.gpu.placeholder_light_grey.view.clone().into(),
                unk60: shading_read,
                unk70: Vec4::ONE,
                unk80: Vec4::ONE,
                unk90: Vec4::ONE,
                unka0: Vec4::ONE,
                unkb0: Vec4::ONE,
                ..(*ext.transparent).clone()
            };
        }
        self.globals.scopes.view.bind(cmd).unwrap();
        self.globals.scopes.transparent.bind(cmd).unwrap();
        self.bind_surfaces(cmd, &[view.shading_result], Some(view.gbuffers.depth));
        cmd.state = PipelineState::new(Some(8), Some(15), Some(2), Some(1));
        cmd.flush_states();
        self.submit_stage(cmd, View::MAIN, RenderStage::DecalsAdditive, feature);

        view.shading_result_read
            .lock()
            .update(cmd, view.surfaces.get(view.shading_result));
        {
            let read: TextureView = view.shading_result_read.lock().srv.clone().into();
            let ext = self.externs.get_mut();
            ext.transparent.unk48 = read.clone();
            ext.transparent.unk60 = read;
        }
        cmd.state = PipelineState::new(Some(8), Some(15), Some(2), Some(1));
        cmd.flush_states();
        self.submit_stage(cmd, View::MAIN, RenderStage::Transparents, feature);
    }
}

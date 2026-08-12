use std::sync::{Arc, Once};

use alkahest_data::tfx::PipelineState;
use glam::{Vec4, vec4};

use crate::{
    Renderer,
    asset::{Handle, texture::Texture},
    cmd_event_span,
    gpu::command_list::CommandList,
    tfx::{externs, view::MainView},
};

const SHADOWKEEP_ATMOSPHERE_BASIS: [Vec4; 3] = [
    vec4(-0.08323, 0.0, -0.99653, 1.0),
    vec4(-0.03088, 0.99952, 0.00258, 1.0),
    vec4(0.99605, 0.03098, -0.08319, 1.0),
];
static SHADOWKEEP_ATMOSPHERE_BASIS_LOGGED: Once = Once::new();

#[derive(Default, Clone)]
pub struct AtmosphereData {
    pub atmosphere_lookup_near_0: Handle<Texture>,
    pub atmosphere_lookup_far_0: Handle<Texture>,
    pub atmosphere_lookup_near_1: Handle<Texture>,
    pub atmosphere_lookup_far_1: Handle<Texture>,

    pub atmosphere_lookup_vertical: Handle<Texture>,

    pub shadowkeep_lookup_volume_0: Handle<Texture>,
    pub shadowkeep_lookup_volume_1: Handle<Texture>,
    pub shadowkeep_lookup_vertical: Handle<Texture>,
    pub shadowkeep_lookup_table: Option<Texture>,
    /// Sixteen monotonic authored scalars matching the depth of the two
    /// atmosphere lookup volumes. No decoded shader/extern binding consumes
    /// them, so retain them as provenance rather than treating them as a basis.
    pub shadowkeep_lookup_parameters: [Vec4; 4],
}

pub struct SunDirections {
    pub sun_directions: Vec<Vec4>,
    pub atmosphere_directions: Vec<Vec4>,
}

impl Renderer {
    pub(crate) fn submit_atmosphere(self: &Arc<Self>, cmd: &mut CommandList, view: &MainView) {
        cmd_event_span!(cmd, "atmosphere");
        let _gpu_span = self.profiler.scope(cmd, "atmosphere");

        self.generate_sky_mask(cmd, view);

        self.generate_sky_lookup(cmd, view);
    }

    /// Produce the two lookup surfaces consumed by the preserved Arrivals
    /// deferred and sky techniques. Returns false while authored textures are
    /// still loading so the caller can retain the procedural fallback.
    pub(crate) fn submit_shadowkeep_atmosphere_lookups(
        &self,
        cmd: &mut CommandList,
        view: &MainView,
    ) -> bool {
        let frame = self.frame_packet.read();
        let atmosphere = &frame.misc.atmosphere;
        let authored_inputs_ready = atmosphere.shadowkeep_lookup_table.is_some()
            && atmosphere.shadowkeep_lookup_volume_0.is_loaded()
            && !atmosphere.shadowkeep_lookup_volume_0.is_null()
            && atmosphere.shadowkeep_lookup_volume_1.is_loaded()
            && !atmosphere.shadowkeep_lookup_volume_1.is_null()
            && atmosphere.shadowkeep_lookup_vertical.is_loaded()
            && !atmosphere.shadowkeep_lookup_vertical.is_null();
        drop(frame);
        let Some(pipelines) = self.shadowkeep_atmosphere_pipelines.as_ref() else {
            return false;
        };
        if !authored_inputs_ready {
            return false;
        }

        self.clear_surface(cmd, view.atmosphere.sky_lookup_far, [0.0; 4]);
        self.bind_surfaces(cmd, &[view.atmosphere.sky_lookup_far], None);
        cmd.output_merger_set_depth_stencil_state(None, 0);
        cmd.state = PipelineState::new(Some(0), Some(0), Some(0), Some(0));
        let direction_generated = self.execute_shadowkeep_global_pipeline(
            cmd,
            &pipelines.sky_direction_lookup_generate,
            "shadowkeep/sky_direction_lookup_generate",
        );

        SHADOWKEEP_ATMOSPHERE_BASIS_LOGGED.call_once(|| {
            tracing::info!(
                basis = ?SHADOWKEEP_ATMOSPHERE_BASIS,
                source = "fixed renderer fallback; no compatible placement-local basis decoded",
                "using Shadowkeep atmosphere-space basis"
            );
        });
        {
            let mut ext = self.externs.write();
            [
                ext.postprocess.unkc0,
                ext.postprocess.unkd0,
                ext.postprocess.unke0,
            ] = SHADOWKEEP_ATMOSPHERE_BASIS;
        }
        self.clear_surface(cmd, view.atmosphere.sky_lookup_near, [0.0; 4]);
        self.bind_surfaces(cmd, &[view.atmosphere.sky_lookup_near], None);
        cmd.output_merger_set_depth_stencil_state(None, 0);
        cmd.state = PipelineState::new(Some(0), Some(0), Some(0), Some(0));
        let atmosphere_generated = self.execute_shadowkeep_global_pipeline(
            cmd,
            &pipelines.sky_lookup_generate,
            "shadowkeep/sky_lookup_generate",
        );
        direction_generated && atmosphere_generated
    }

    fn generate_sky_mask(self: &Arc<Self>, cmd: &mut CommandList, view: &MainView) {
        // Generate initial sky mask from uber depth
        {
            let sky_mask_surf = view.surfaces.get(view.atmosphere.sky_mask_initial);
            let uber_depth_surf = view.surfaces.get(view.gbuffers.uber_depth_half);
            sky_mask_surf.bind_single(cmd);
            let mut ext = self.externs.write();
            ext.postprocess.input = view.gbuffers.uber_depth_half.into();
            ext.postprocess.res_for_input = uber_depth_surf.resolution_with_recip();
            ext.postprocess.output_res = sky_mask_surf.resolution_with_recip();
        }

        {
            cmd.state = PipelineState::new(Some(0), Some(0), Some(0), Some(0));
            self.execute_global_pipeline(
                cmd,
                &self.globals.pipelines.sky_generate_sky_mask,
                "sky_generate_sky_mask",
            );
        }

        // Downsample 1/4th -> 1/8th
        {
            let sky_mask_initial_surf = view.surfaces.get(view.atmosphere.sky_mask_initial);
            let sky_mask_surf = view.surfaces.get(view.atmosphere.sky_mask);
            sky_mask_surf.bind_single(cmd);
            let mut ext = self.externs.write();
            *ext.postprocess = externs::Postprocess {
                input: view.atmosphere.sky_mask_initial.into(),
                res_for_input: sky_mask_initial_surf.resolution_with_recip(),
                output_res: sky_mask_surf.resolution_with_recip(),
                unkc0: Vec4::ZERO,
                ..Default::default()
            };
        }

        {
            cmd.state = PipelineState::new(Some(0), Some(0), Some(0), Some(0));
            self.execute_global_pipeline(
                cmd,
                &self.globals.pipelines.downsample_block_2x2,
                "downsample_block_2x2",
            );
        }

        // // Blur pass 1
        // {
        //     let sky_mask_surf = view.surfaces().get(view.atmosphere.sky_mask);
        //     let sky_mask_surf_temp = view.surfaces().get(view.atmosphere.sky_mask_temp);
        //     sky_mask_surf_temp.bind_single(cmd);
        //     let ext = self.externs.get_mut();
        //     *ext.postprocess = externs::Postprocess {
        //         input: view.atmosphere.sky_mask.into(),
        //         res_for_input: sky_mask_surf.resolution_with_recip(),
        //         output_res: sky_mask_surf_temp.resolution_with_recip(),
        //         unkc0: vec4(0.59197, 0.32437, 0.03, 0.022),
        //         unkd0: Vec4::ONE,
        //         ..Default::default()
        //     };
        // }

        // {
        //     cmd.state = PipelineState::new(Some(0), Some(0), Some(0), Some(0));
        //     self.execute_global_pipeline(
        //         cmd,
        //         &self.globals.pipelines.radial_blur_8,
        //         "radial_blur_8",
        //     );
        // }

        // // Blur pass 2
        // {
        //     let sky_mask_surf = view.surfaces().get(view.atmosphere.sky_mask);
        //     let sky_mask_surf_temp = view.surfaces().get(view.atmosphere.sky_mask_temp);
        //     sky_mask_surf.bind_single(cmd);
        //     let ext = self.externs.get_mut();
        //     *ext.postprocess = externs::Postprocess {
        //         input: view.atmosphere.sky_mask_temp.into(),
        //         res_for_input: sky_mask_surf_temp.resolution_with_recip(),
        //         output_res: sky_mask_surf.resolution_with_recip(),
        //         unkc0: vec4(0.59197, 0.32437, 0.08, 0.05867),
        //         unkd0: Vec4::NEG_ONE,
        //         ..Default::default()
        //     };
        // }

        // {
        //     cmd.state = PipelineState::new(Some(0), Some(0), Some(0), Some(0));
        //     self.execute_global_pipeline(
        //         cmd,
        //         &self.globals.pipelines.radial_blur_8,
        //         "radial_blur_8",
        //     );
        // }
    }

    fn generate_sky_lookup(self: &Arc<Self>, cmd: &mut CommandList, view: &MainView) {
        {
            let atm = &self.frame_packet.read().misc.atmosphere;
            let mut ext = self.externs.write();
            ext.atmosphere.unka0 = view.atmosphere.sky_mask.into();

            ext.atmosphere.unk40 = atm.atmosphere_lookup_near_0.clone().into();
            ext.atmosphere.unk58 = atm.atmosphere_lookup_near_1.clone().into();

            ext.postprocess.unkc0 = vec4(-0.08323, 0.00, -0.99653, 1.0);
            ext.postprocess.unkd0 = vec4(-0.03088, 0.99952, 0.00258, 1.0);
            ext.postprocess.unke0 = vec4(0.99605, 0.03098, -0.08319, 1.0);
        }

        {
            self.bind_surfaces(cmd, &[view.atmosphere.sky_lookup_near], None);
            cmd.state = PipelineState::new(Some(0), Some(0), Some(0), Some(0));
            self.execute_global_pipeline(
                cmd,
                &self.globals.pipelines.sky_lookup_generate_near,
                "sky_lookup_generate_near",
            );
        }

        {
            let atm = &self.frame_packet.read().misc.atmosphere;
            let mut ext = self.externs.write();
            ext.atmosphere.unk40 = atm.atmosphere_lookup_far_0.clone().into();
            ext.atmosphere.unk58 = atm.atmosphere_lookup_far_1.clone().into();
        }

        {
            self.bind_surfaces(cmd, &[view.atmosphere.sky_lookup_far], None);
            cmd.state = PipelineState::new(Some(0), Some(0), Some(0), Some(0));
            self.execute_global_pipeline(
                cmd,
                &self.globals.pipelines.sky_lookup_generate_far,
                "sky_lookup_generate_far",
            );
        }
    }
}

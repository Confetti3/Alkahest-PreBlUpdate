use std::sync::Arc;

use alkahest_render::{
    Renderer,
    camera::CameraProjection,
    cmd_event_span,
    gpu::command_list::CommandList,
    tfx::view::{View, ViewKind},
};
use glam::Mat4;

use crate::world::transform::Transform;

pub struct ShadowMap {
    pub last_update: u64,
    pub selected_for_update: bool,
    pub world_to_camera: Mat4,
    pub camera_to_projective: Mat4,
}

impl ShadowMap {
    pub fn create(transform: Transform, fov: f32, near: f32, far: f32) -> Self {
        let world_to_camera = transform.view_matrix();
        let projection = CameraProjection::Perspective.matrix(1.0, fov, near, far);

        ShadowMap {
            last_update: 0,
            selected_for_update: false,
            world_to_camera,
            camera_to_projective: projection,
        }
    }

    // fn initialize_surface(&mut self, gpu: &Gpu) {
    //     let surface_desc = SurfaceDesc::builder("shadowmap", SizeRelativity::Absolute)
    //         .format(dxgi::Format::R32Typeless)
    //         .depth_format(dxgi::Format::D32Float)
    //         .view_format(dxgi::Format::R32Float)
    //         .build();
    //     let surface = Surface::new(
    //         &gpu.device,
    //         (Self::SHADOWMAP_RESOLUTION, Self::SHADOWMAP_RESOLUTION),
    //         surface_desc,
    //     )
    //     .expect("Failed to create shadowmap surface");

    //     *self.surface.lock() = Some(surface);
    // }

    // pub fn bind(&mut self, cmd: &mut CommandList, renderer: &Renderer) {
    //     if self.surface.lock().is_none() {
    //         self.initialize_surface(cmd.gpu());
    //     }

    //     let surface_lock = self.surface.lock();
    //     let shadow_surface = surface_lock
    //         .as_ref()
    //         .expect("unreachable: shadow surface was just initialized");
    //     shadow_surface.clear_depth(cmd, 0.0, 0);
    //     shadow_surface.bind_single(cmd);

    //     let ext = renderer.externs.get_mut();
    //     ext.view.update(
    //         self.world_to_camera,
    //         self.camera_to_projective,
    //         (Self::SHADOWMAP_RESOLUTION, Self::SHADOWMAP_RESOLUTION),
    //     );
    //     renderer.globals.scopes.view.bind(cmd).unwrap();
    // }
}

pub fn s_extract_all_shadowmaps(
    world: &mut hecs::World,
    renderer: &Arc<Renderer>,
    update_budget: usize,
) {
    if !renderer.asset_manager.is_idle() {
        return;
    }

    profiling::scope!("extract_shadowmaps");
    let mut candidates = Vec::new();
    for (entity, shadowmap) in world.query::<&mut ShadowMap>().iter() {
        shadowmap.selected_for_update = false;
        candidates.push((entity, shadowmap.last_update));
    }
    candidates.sort_by_key(|(_, last_update)| *last_update);

    for (index, (entity, _)) in candidates
        .into_iter()
        .take(update_budget.min(View::MAX_VIEWS - View::FIRST_SHADOW))
        .enumerate()
    {
        let Ok((shadowmap, view)) = world.query_one_mut::<(&mut ShadowMap, &mut View)>(entity)
        else {
            continue;
        };
        let shadow_index = View::FIRST_SHADOW + index;
        let resolution = view.resolution();
        view.update(
            shadowmap.world_to_camera,
            shadowmap.camera_to_projective,
            resolution,
        );
        let ViewKind::Shadow(shadow_view) = &mut view.kind else {
            continue;
        };

        shadowmap.selected_for_update = true;
        shadow_view.index = shadow_index;
        renderer.cull_view(shadow_index, view);
    }
}

pub fn s_submit_all_shadowmaps(
    world: &mut hecs::World,
    cmd: &mut CommandList,
    renderer: &Arc<Renderer>,
    frame_index: u64,
) {
    if !renderer.asset_manager.is_idle() {
        return;
    }

    profiling::scope!("render_shadowmaps");
    let _gpuspan = renderer.profiler.scope(cmd, "render_shadowmaps");

    for (_entity, (shadowmap, view)) in world.query::<(&mut ShadowMap, &View)>().iter() {
        if !shadowmap.selected_for_update {
            continue;
        }

        let ViewKind::Shadow(v) = &view.kind else {
            continue;
        };
        if v.index >= View::MAX_VIEWS {
            continue;
        }

        {
            cmd_event_span!(cmd, format!("prepare_view_{}", view.name));
            let _gpuspan = renderer
                .profiler
                .scope(cmd, format!("prepare_view_{}", view.name));

            for node in renderer.frame_packet.read().iter_visible(v.index) {
                if let Some(render_object) = renderer
                    .objects
                    .write()
                    .get_mut(node.render_object_handle.into())
                {
                    render_object.prepare(renderer, v.index, &*node.data);
                } else if node.render_object_handle.is_valid() {
                    error!("Render object not found: {:?}", node.render_object_handle);
                }
            }
        }

        renderer.submit_view(cmd, view, None);
        shadowmap.last_update = frame_index;
        shadowmap.selected_for_update = false;
    }
}

// pub fn s_render_all_shadowmaps(
//     world: &hecs::World,
//     cmd: &mut CommandList,
//     renderer: &Arc<Renderer>,
// ) {
//     profiling::scope!("render_shadowmaps");
//     let _gpuspan = renderer.profiler.scope(cmd, "render_shadowmaps");
//     if renderer.asset_manager.count_loading() > 0 {
//         return;
//     }

//     cmd.state = PipelineState::new(Some(0), Some(2), Some(2), Some(6));
//     cmd.flush_states();

//     // cmd.set_depth_mode(DepthMode::Forward);
//     for (_entity, shadowmap) in world.query::<&mut ShadowMap>().iter() {
//         renderer
//             .common
//             .shadowmap_vs_t2
//             .bind(cmd, 2, ShaderStage::Vertex);

//         if shadowmap.surface.lock().is_some() {
//             // Shadow map already rendered
//             continue;
//         }

//         shadowmap.bind(cmd, renderer);
//         renderer.submit_stage(
//             cmd,
//             RenderStage::ShadowGenerate,
//             FeatureRendererSubscription::all(),
//         );
//     }
//     // cmd.set_depth_mode(DepthMode::Reverse);
// }

mod asset_viewer;
pub mod controller;
mod surface_viewer;

use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

use alkahest_core::ConVars;
use alkahest_data::tfx::{FeatureRendererSubscription, common::AxisAlignedBBox};
use alkahest_render::{
    Gpu, Renderer,
    camera::Camera,
    cmd_event_span,
    gpu::command_list::{CommandList, DepthMode},
    renderer::{
        RendererEra,
        hzb::Hzb,
        submit::{
            DebugPipeline,
            atmosphere::{AtmosphereData, SunDirections},
        },
    },
    tfx::{
        externs::get_global_channel_name,
        packet::FramePacketMisc,
        view::{RenderSettings, View, ViewKind},
    },
    visibility::frustum::Frustum,
};
use anyhow::Context;
use bitflags::Flags;
use d3d11::{
    Box3D, CpuAccessFlags, MapType, ShaderResourceView, Texture2D, Texture2dDesc, Usage, dxgi,
};
use egui::{
    FontId, Image, ImageSource, Rect, Response, RichText, Sense, TextStyle, Ui, UiBuilder, Vec2,
    Widget, containers::menu::MenuConfig, load::SizedTexture, vec2,
};
use glam::{Mat4, UVec2, Vec3, Vec4, Vec4Swizzles};
use google_material_symbols::GoogleMaterialSymbols;

#[cfg(feature = "wwise")]
use crate::audio;
#[cfg(feature = "wwise")]
use crate::world::audio::{s_start_all_audio_sources, s_update_audio_sources};
use crate::{
    app::SharedState,
    ui::{
        scene::controller::CameraController,
        util::{ExternalDataWidgetExt, UiExt},
    },
    world::{
        render_objects::{
            s_are_all_objects_loaded, s_extract_ambient_occlusion, s_extract_render_objects,
        },
        s_update_object_channels,
        sequencer::{s_evaluate_global_channel_expressions, s_get_all_global_channel_ids},
        shadowkeep_inspection::{MapEntityVisibility, MapInspectionNode},
        shadowmap::{s_extract_all_shadowmaps, s_submit_all_shadowmaps},
        transform::Transform,
    },
};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SceneDepthSample {
    pub pixel: UVec2,
    pub reverse_depth: f32,
    pub world_position: Vec3,
}

fn unproject_depth_sample(
    pixel: UVec2,
    resolution: UVec2,
    reverse_depth: f32,
    world_to_clip: Mat4,
) -> Option<SceneDepthSample> {
    if resolution.x == 0 || resolution.y == 0 || !reverse_depth.is_finite() || reverse_depth <= 0.0
    {
        return None;
    }
    let ndc = Vec3::new(
        ((pixel.x as f32 + 0.5) / resolution.x as f32) * 2.0 - 1.0,
        1.0 - ((pixel.y as f32 + 0.5) / resolution.y as f32) * 2.0,
        reverse_depth,
    );
    let world = world_to_clip.inverse() * ndc.extend(1.0);
    if !world.is_finite() || !world.w.is_finite() || world.w.abs() <= f32::EPSILON {
        return None;
    }
    let world_position = world.truncate() / world.w;
    world_position.is_finite().then_some(SceneDepthSample {
        pixel,
        reverse_depth,
        world_position,
    })
}

pub struct Scene {
    pub world: hecs::World,

    renderer: Arc<Renderer>,
    pub camera: Camera,
    pub view: View,
    last_frame_time: Instant,
    start_time: Instant,
    /// Time of day (0 - 3600)
    time_of_day: f32,
    animate_time_of_day: bool,
    time_scale: f32,
    diagnostic_freeze: bool,
    /// Allows map navigation without advancing the frozen diagnostic timeline.
    camera_input_while_frozen: bool,
    frozen_render_time: f32,
    sun_light_angle: f32,
    pub render_mode: RenderMode,
    keep_settings_open: bool,
    lock_resolution: bool,

    pub controller: CameraController,

    surface: d3d11::Texture2D,
    surface_srv: d3d11::ShaderResourceView,
    depth_readback: Option<Texture2D>,
    depth_readback_last_warning: Option<Instant>,

    profiler_results: Option<String>,
    show_surface_viewer: bool,
    show_channel_editor: bool,
    automate_channels: bool,

    frametimes: Vec<f32>,
    global_channels: [Vec4; 256],

    sun_shadow_views: [View; Renderer::NUM_CASCADES],

    scene_id: String,
    frame_index: u64,
    shadowkeep_sky_channels_logged: bool,
}

/// Construct a continuous east-to-west solar path from the existing scene
/// clock. Arrivals packages in the admitted map graph do not contain a
/// compatible global-channel producer, so this state is explicit rather than
/// pretending the positional defaults are activity automation.
fn shadowkeep_sun_state(time_of_day: f32, heading_degrees: f32) -> (Vec4, f32) {
    let day_fraction = (time_of_day / 3600.0).rem_euclid(1.0);
    let hour_angle = (day_fraction - 0.5) * std::f32::consts::TAU;
    let maximum_elevation = 0.7f32.atan();
    let local_direction = Vec3::new(
        hour_angle.cos() * maximum_elevation.cos(),
        -hour_angle.sin(),
        hour_angle.cos() * maximum_elevation.sin(),
    );
    let (heading_sin, heading_cos) = heading_degrees.to_radians().sin_cos();
    let sun_direction = Vec3::new(
        local_direction.x * heading_cos - local_direction.y * heading_sin,
        local_direction.x * heading_sin + local_direction.y * heading_cos,
        local_direction.z,
    )
    .normalize();
    let twilight = ((sun_direction.z + 0.04) / 0.12).clamp(0.0, 1.0);
    let daylight = twilight * twilight * (3.0 - 2.0 * twilight);
    (sun_direction.extend(0.0), daylight)
}

#[cfg(test)]
mod tests {
    use glam::{Mat4, UVec2, Vec3};

    use super::{shadowkeep_sun_state, unproject_depth_sample};

    #[test]
    fn shadowkeep_sun_state_tracks_scene_clock() {
        let (noon_direction, noon_daylight) = shadowkeep_sun_state(1800.0, 60.0);
        let legacy_noon_direction =
            Vec3::new(60.0f32.to_radians().cos(), 60.0f32.to_radians().sin(), 0.7).normalize();
        assert!((noon_direction.truncate() - legacy_noon_direction).length() < 0.0001);
        assert_eq!(noon_daylight, 1.0);

        let (midnight_direction, midnight_daylight) = shadowkeep_sun_state(0.0, 60.0);
        assert!(midnight_direction.z < 0.0);
        assert_eq!(midnight_daylight, 0.0);

        let (wrapped_direction, wrapped_daylight) = shadowkeep_sun_state(3600.0, 60.0);
        assert!((wrapped_direction - midnight_direction).length() < 0.0001);
        assert_eq!(wrapped_daylight, midnight_daylight);
    }

    #[test]
    fn depth_sample_unprojects_pixel_centers_and_rejects_clear_values() {
        let center = unproject_depth_sample(UVec2::ZERO, UVec2::ONE, 0.75, Mat4::IDENTITY).unwrap();
        assert_eq!(center.world_position, Vec3::new(0.0, 0.0, 0.75));

        let edge = unproject_depth_sample(UVec2::new(1, 1), UVec2::new(2, 2), 0.5, Mat4::IDENTITY)
            .unwrap();
        assert_eq!(edge.world_position, Vec3::new(0.5, -0.5, 0.5));
        assert!(unproject_depth_sample(UVec2::ZERO, UVec2::ONE, 0.0, Mat4::IDENTITY).is_none());
        assert!(
            unproject_depth_sample(UVec2::ZERO, UVec2::ONE, f32::NAN, Mat4::IDENTITY).is_none()
        );
    }
}

impl Scene {
    pub fn new(
        renderer: Arc<Renderer>,
        camera: Camera,
        shared: &SharedState,
        scene_id: impl Into<String>,
    ) -> anyhow::Result<Self> {
        let (surface, surface_srv) = Self::create_surface(&renderer.gpu, (512, 512))?;

        let sun_shadow_views = std::array::from_fn(|i| {
            let mut v = View::new_shadow(format!("shadow_csm_{i}"), &renderer.gpu, (2048, 2048))
                .expect("Failed to create shadow cascade view");
            v.disable_culling = true;
            v
        });

        let mut view = View::new_main("main", &renderer.gpu, (128, 128))?;
        *view.settings_mut() = RenderSettings::for_era(renderer.era());
        let view_surfaces = view.surfaces().unwrap().clone();
        view_surfaces.set_resolution_scale(shared.config.read().resolution_scale);
        let is_shadowkeep = renderer.era() == RendererEra::Shadowkeep;
        let global_channels = renderer.externs.read().default_globals;

        Ok(Self {
            world: hecs::World::new(),
            view,
            global_channels,
            renderer,
            camera,
            time_of_day: if is_shadowkeep { 1800.0 } else { 1200.0 },
            time_scale: 1.0,
            animate_time_of_day: true,
            diagnostic_freeze: false,
            camera_input_while_frozen: false,
            frozen_render_time: 0.0,
            sun_light_angle: 60f32,
            render_mode: if is_shadowkeep {
                RenderMode::Shaded
            } else {
                RenderMode::LightDiffuse
            },
            keep_settings_open: false,
            lock_resolution: false,
            controller: CameraController::new_orbit(Vec3::ZERO, 2.5),
            surface,
            surface_srv,
            depth_readback: None,
            depth_readback_last_warning: None,
            last_frame_time: Instant::now(),
            start_time: Instant::now(),
            profiler_results: None,
            show_surface_viewer: false,
            show_channel_editor: false,
            frametimes: Vec::new(),
            sun_shadow_views,
            scene_id: scene_id.into(),
            frame_index: 1,
            shadowkeep_sky_channels_logged: false,
            automate_channels: true,
        })
    }

    pub fn set_id(&mut self, id: impl Into<String>) {
        self.scene_id = id.into();
        self.shadowkeep_sky_channels_logged = false;
    }

    // pub fn set_global_channel(&mut self, id: u32, value: Vec4) {
    //     if let Some(index) = self.renderer.externs.get_global_channel_index(id) {
    //         self.global_channels[index] = value;
    //     }
    // }

    pub fn set_global_channel_by_name(&mut self, name: &str, value: Vec4) {
        if let Some(index) = self
            .renderer
            .externs
            .read()
            .get_global_channel_index_by_name(name)
        {
            self.global_channels[index] = value;
        }
    }

    pub fn with_controller(mut self, controller: CameraController) -> Self {
        self.controller = controller;
        self
    }

    fn create_surface(
        gpu: &Gpu,
        resolution: (u32, u32),
    ) -> anyhow::Result<(Texture2D, ShaderResourceView)> {
        let texture = gpu.create_texture2d(
            &Texture2dDesc::builder()
                .width(resolution.0)
                .height(resolution.1)
                .mip_levels(1)
                .format(dxgi::Format::R8g8b8a8Unorm)
                .bind_flags(d3d11::BindFlags::SHADER_RESOURCE)
                .build(),
            None,
        )?;

        let srv = gpu.create_shader_resource_view(&texture, None)?;

        Ok((texture, srv))
    }

    fn sample_main_depth(&mut self, rect: Rect, pointer: egui::Pos2) -> Option<SceneDepthSample> {
        if !rect.contains(pointer) || rect.width() <= 0.0 || rect.height() <= 0.0 {
            return None;
        }
        let (width, height) = self.view.framebuffer_resolution();
        if width == 0 || height == 0 {
            return None;
        }
        let pixel = UVec2::new(
            (((pointer.x - rect.left()) / rect.width()) * width as f32)
                .floor()
                .clamp(0.0, width.saturating_sub(1) as f32) as u32,
            (((pointer.y - rect.top()) / rect.height()) * height as f32)
                .floor()
                .clamp(0.0, height.saturating_sub(1) as f32) as u32,
        );
        let result = (|| -> anyhow::Result<Option<SceneDepthSample>> {
            if self.depth_readback.is_none() {
                self.depth_readback = Some(
                    self.renderer.gpu.create_texture2d(
                        &Texture2dDesc::builder()
                            .width(1)
                            .height(1)
                            .mip_levels(1)
                            .format(dxgi::Format::R32g8x24Typeless)
                            .usage(Usage::Staging)
                            .bind_flags(d3d11::BindFlags::empty())
                            .cpu_access_flags(CpuAccessFlags::READ)
                            .build(),
                        None,
                    )?,
                );
            }
            let staging = self
                .depth_readback
                .as_ref()
                .context("depth staging texture was not created")?;
            let ViewKind::Main(main_view) = &self.view.kind else {
                anyhow::bail!("main depth requested from a non-main view");
            };
            let source = main_view.gbuffers.depth_proxy.lock();
            let source_box = Box3D {
                left: pixel.x as i32,
                top: pixel.y as i32,
                front: 0,
                right: pixel.x as i32 + 1,
                bottom: pixel.y as i32 + 1,
                back: 1,
            };
            let context = self.renderer.gpu.context();
            context.copy_subresource_region(
                &source.res,
                0,
                Some(&source_box),
                staging,
                0,
                (0, 0, 0),
            );
            drop(source);
            let mapped = context.map(staging, 0, MapType::Read, false)?;
            let reverse_depth = unsafe { std::ptr::read_unaligned(mapped.data.cast::<f32>()) };
            drop(mapped);
            Ok(unproject_depth_sample(
                pixel,
                UVec2::new(width, height),
                reverse_depth,
                self.camera.world_to_clip_space(),
            ))
        })();
        match result {
            Ok(sample) => sample,
            Err(error) => {
                let now = Instant::now();
                if self
                    .depth_readback_last_warning
                    .is_none_or(|last| now.duration_since(last) >= Duration::from_secs(5))
                {
                    tracing::warn!(error = %format_args!("{error:#}"), "main depth readback failed");
                    self.depth_readback_last_warning = Some(now);
                }
                None
            }
        }
    }

    pub fn set_world(&mut self, world: hecs::World) {
        self.world = world;
    }

    pub fn take_world(&mut self) -> hecs::World {
        std::mem::take(&mut self.world)
    }

    pub fn enable_camera_input_while_frozen(&mut self) {
        self.camera_input_while_frozen = true;
    }

    pub fn clear(&mut self) {
        self.world.clear();
    }

    pub fn show_with_overlay<R, F>(
        &mut self,
        ui: &mut Ui,
        size: Vec2,
        egui_d3d11: &mut egui_d3d11::D3D11Renderer,
        overlay: F,
    ) -> Option<R>
    where
        F: FnOnce(&mut Ui, Rect, &Camera, &Response, Option<SceneDepthSample>) -> R,
    {
        let mut overlay = Some(overlay);
        let now = Instant::now();
        let delta_time = (now - self.last_frame_time).as_secs_f32();
        self.frametimes.push(delta_time);
        while self.frametimes.len() > 60 {
            self.frametimes.remove(0);
        }

        self.last_frame_time = now;
        let delta_time_average = if !self.frametimes.is_empty() {
            let sum: f32 = self.frametimes.iter().sum();
            sum / self.frametimes.len() as f32
        } else {
            delta_time
        };

        if self.show_surface_viewer {
            egui::Panel::right("surface_viewer").show(ui, |ui| {
                // self.show_texture_viewer(ui, egui_d3d11);
                self.show_surface_viewer(ui, egui_d3d11);
            });
        }

        if self.show_channel_editor {
            egui::Panel::right("channel_editor").show(ui, |ui| {
                self.show_global_channel_editor(ui);
            });
        }

        let overlay_result = egui::CentralPanel::default()
            .show(ui, |ui| {
                let panel_rect = ui.available_rect_before_wrap();

                let r = ui
                    .image(SizedTexture {
                        id: egui_d3d11.textures_mut().allocate_dx_temporary(
                            self.surface_srv.clone(),
                            None,
                            false,
                        ),
                        size,
                    })
                    .interact(Sense::CLICK | Sense::DRAG | Sense::HOVER);

                if !ui.is_rect_visible(r.rect) {
                    return None;
                }

                let mut bar_rect = r.rect;
                bar_rect.set_height(32.0);
                ui.painter().rect_filled(
                    bar_rect,
                    0.0,
                    egui::Color32::from_black_alpha(if ui.rect_contains_pointer(bar_rect) {
                        160
                    } else {
                        64
                    }),
                );
                ui.scope_builder(UiBuilder::new().max_rect(bar_rect), |ui| {
                    egui::MenuBar::new().ui(ui, |ui| {
                        self.show_toolbar(ui);
                    })
                });

                let fps_rect = ui.painter_at(panel_rect).text(
                    panel_rect.right_top() + Vec2::new(0.0, 3.0) + Vec2::splat(1.0),
                    egui::Align2::RIGHT_TOP,
                    format!("{} ", (1. / delta_time_average).round()),
                    egui::FontId::monospace(16.0),
                    egui::Color32::BLACK,
                );

                ui.painter_at(panel_rect).text(
                    panel_rect.right_top() + Vec2::new(0.0, 3.0),
                    egui::Align2::RIGHT_TOP,
                    format!("{} ", (1. / delta_time_average).round()),
                    egui::FontId::monospace(16.0),
                    egui::Color32::GREEN,
                );

                ui.scope_builder(
                    egui::UiBuilder::new().max_rect(panel_rect.shrink2(vec2(12.0, 4.0))),
                    |ui| {
                        ui.with_layout(egui::Layout::bottom_up(egui::Align::Min), |ui| {
                            if let Some(last_speed_change) = ui.memory(|m| {
                                m.data.get_temp::<Instant>("scene_last_speed_change".into())
                            }) && last_speed_change.elapsed().as_secs_f32() <= 2.0
                            {
                                ui.label(format!(
                                    "{} Camera Speed: {:.1}m/s",
                                    GoogleMaterialSymbols::Speed,
                                    self.controller.speed()
                                ));
                            }

                            if self.world.is_empty() {
                                ui.label(
                                    RichText::new(format!(
                                        "{} Scene is empty",
                                        GoogleMaterialSymbols::Warning
                                    ))
                                    .size(16.0),
                                );
                            }

                            if self.renderer.asset_manager.count_loading() > 0 {
                                ui.label(
                                    RichText::new(format!(
                                        "{} Loading assets... ({} in progress)",
                                        GoogleMaterialSymbols::HardDrive,
                                        self.renderer.asset_manager.count_loading()
                                    ))
                                    .size(16.0),
                                );
                            }
                        });
                    },
                );

                ui.style_mut().spacing.tooltip_width = 4096.0;
                Renderer::instance().profiler.set_enabled(false);
                ui.interact(
                    fps_rect,
                    "frame_counter_profiler_tooltip".into(),
                    Sense::hover(),
                )
                .on_hover_ui(|ui| {
                    Renderer::instance().profiler.set_enabled(true);
                    if let Some(profiler_results) = &self.profiler_results {
                        ui.add(
                            egui::Label::new(RichText::new(profiler_results.clone()).monospace())
                                .extend(),
                        );
                    } else {
                        ui.weak("Profiler data not available yet.");
                    }
                });

                let size_pixels = size * ui.ctx().pixels_per_point();
                let resolution = (size_pixels.x as u32, size_pixels.y as u32);

                if !self.diagnostic_freeze || self.camera_input_while_frozen {
                    self.controller.update(&mut self.camera, ui, &r, delta_time);

                    if !self.diagnostic_freeze && r.dragged_by(egui::PointerButton::Middle) {
                        let delta_adjusted = r.drag_delta() / 4.0;
                        self.sun_light_angle += delta_adjusted.x;
                        self.sun_light_angle = self.sun_light_angle.rem_euclid(360.0);
                    }
                }

                subsecond::call(|| {
                    self.render(delta_time, resolution);
                });
                let depth_sample = if (r.clicked_by(egui::PointerButton::Primary)
                    || r.double_clicked_by(egui::PointerButton::Primary))
                    && r.interact_pointer_pos()
                        .is_some_and(|pointer| r.rect.contains(pointer))
                {
                    r.interact_pointer_pos()
                        .and_then(|pointer| self.sample_main_depth(r.rect, pointer))
                } else {
                    None
                };
                overlay
                    .take()
                    .map(|overlay| overlay(ui, r.rect, &self.camera, &r, depth_sample))
            })
            .inner;

        #[cfg(feature = "wwise")]
        {
            use std::sync::atomic::AtomicU64;
            fn hash<T: std::hash::Hash>(value: &T) -> u64 {
                use std::hash::Hasher;
                let mut hasher = std::collections::hash_map::DefaultHasher::new();
                value.hash(&mut hasher);
                hasher.finish()
            }

            static LAST_ACTIVE_SCENE_ID: AtomicU64 = AtomicU64::new(u64::MAX);

            let scene_hash = hash(&self.scene_id);
            let old_scene_hash = LAST_ACTIVE_SCENE_ID.load(Ordering::SeqCst);
            if old_scene_hash != scene_hash {
                // Scene changed, stop all audio and start all the sources in this scene
                rrise::sound_engine::stop_all(None);
                s_start_all_audio_sources(&self.world);

                LAST_ACTIVE_SCENE_ID.store(scene_hash, Ordering::SeqCst);
            }

            audio::set_gameobject_pos(
                audio::LISTENER_ID,
                self.camera.position,
                self.camera.up(),
                -self.camera.forward(), // Apparently wwise's forward is not our forward?
                true,
            );

            s_update_audio_sources(&self.world, self.camera.position);
        }
        overlay_result
    }

    pub fn show(&mut self, ui: &mut Ui, size: Vec2, egui_d3d11: &mut egui_d3d11::D3D11Renderer) {
        let _ = self.show_with_overlay(ui, size, egui_d3d11, |_, _, _, _, _| ());
    }

    fn show_toolbar(&mut self, ui: &mut Ui) {
        ui.style_mut().spacing.item_spacing = vec2(8.0, 0.0);
        egui::containers::menu::MenuButton::new(GoogleMaterialSymbols::Tune.to_string())
            .config(
                MenuConfig::new().close_behavior(if self.keep_settings_open {
                    egui::PopupCloseBehavior::IgnoreClicks
                } else {
                    egui::PopupCloseBehavior::CloseOnClickOutside
                }),
            )
            .ui(ui, |ui| {
                self.show_settings_ui(ui);
            })
            .0
            .on_hover_text("Scene Settings");

        if matches!(self.controller, CameraController::Orbit { .. })
            && ui
                .selectable_label(
                    self.camera.draw_grid,
                    GoogleMaterialSymbols::Grid4x4.to_string(),
                )
                .clicked()
        {
            self.camera.draw_grid = !self.camera.draw_grid;
        }

        if ui
            .selectable_label(
                self.show_surface_viewer,
                GoogleMaterialSymbols::ImageSearch.to_string(),
            )
            .clicked()
        {
            self.show_surface_viewer = !self.show_surface_viewer;
        }

        if ui
            .selectable_label(
                self.show_channel_editor,
                GoogleMaterialSymbols::BarChart4Bars.to_string(),
            )
            .clicked()
        {
            self.show_channel_editor = !self.show_channel_editor;
        }

        self.render_mode.ui(ui);
        self.view.subscribed_features.show_input(ui);
    }

    fn show_settings_ui(&mut self, ui: &mut Ui) {
        ui.label(format!("Camera Pos: {:.1?}", self.camera.position));
        ui.label(format!(
            "Camera Yaw/Pitch: {:.1}/{:.1}",
            self.controller.yaw_pitch().x,
            self.controller.yaw_pitch().y
        ));

        ui.style_mut()
            .text_styles
            .insert(TextStyle::Body, FontId::proportional(16.0));
        ui.style_mut()
            .text_styles
            .insert(TextStyle::Heading, FontId::proportional(24.0));
        ui.style_mut()
            .text_styles
            .insert(TextStyle::Small, FontId::proportional(12.0));
        ui.style_mut()
            .text_styles
            .insert(TextStyle::Button, FontId::proportional(16.0));

        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing = vec2(8.0, 8.0);
            ui.heading("Scene Settings");
            if ui
                .selectable_label(
                    self.keep_settings_open,
                    GoogleMaterialSymbols::PushPin.to_string(),
                )
                .on_hover_text("Keep panel open")
                .clicked()
            {
                self.keep_settings_open = !self.keep_settings_open;
            }
            #[cfg(debug_assertions)]
            if ui
                .selectable_label(false, GoogleMaterialSymbols::Code.to_string())
                .on_hover_text("Load camera cbuffers")
                .clicked()
            {
                self.load_camera_cbuffers();
            }
            #[cfg(debug_assertions)]
            if ui
                .selectable_label(
                    self.lock_resolution,
                    GoogleMaterialSymbols::ScreenLockLandscape.to_string(),
                )
                .on_hover_text("Lock resolution to 1920x1080")
                .clicked()
            {
                self.lock_resolution = !self.lock_resolution;
            }
        });

        let is_shadowkeep = self.renderer.era() == RendererEra::Shadowkeep;
        let view_surfaces = self.view.surfaces().unwrap().clone();

        let freeze_changed = ui
            .checkbox(&mut self.diagnostic_freeze, "Diagnostic Freeze")
            .on_hover_text(
                "Freezes camera input, exposure, game/render time, time of day, and sequencer \
                 automation.",
            )
            .changed();
        if freeze_changed && self.diagnostic_freeze {
            self.frozen_render_time = self.start_time.elapsed().as_secs_f32();
        }

        let view_settings = self.view.settings_mut();
        ui.spacing_mut().item_spacing = vec2(8.0, 4.0);
        ui.add_enabled_ui(!is_shadowkeep, |ui| {
            ui.checkbox(&mut view_settings.autoexposure, "Auto-exposure")
                .setting_description_tooltip(
                    "Enables automatic exposure adjustment based on scene brightness.",
                    PerformanceImpact::None,
                );
        });
        if is_shadowkeep {
            ui.weak("Auto-exposure is unavailable for Shadowkeep; fixed exposure is active.");
        }

        // if settings_mut.autoexposure {
        //     ui.strong("Target Luminance");
        //     ui.spacing_mut().slider_width = ui.available_width() * 0.75;
        //     egui::Slider::new(
        //         &mut self.view.autoexposure.config.target_luminance,
        //         0.000002..=0.04,
        //     )
        //     .logarithmic(false)
        //     .show_value(true)
        //     .ui(ui);
        // }

        ui.add_enabled_ui(!view_settings.autoexposure, |ui| {
            ui.strong("Exposure Scale");
            ui.spacing_mut().slider_width = ui.available_width() * 0.75;
            egui::Slider::new(&mut view_settings.exposure_scale, 0.001..=4.0)
                .logarithmic(true)
                .show_value(true)
                .ui(ui);

            ui.strong("Exposure Illum Relative");
            ui.spacing_mut().slider_width = ui.available_width() * 0.75;
            egui::Slider::new(&mut view_settings.exposure_illum_relative, 0.01..=2.0)
                .logarithmic(false)
                .show_value(true)
                .ui(ui);
        });

        ui.add_space(4.0);

        // Time of Day slider
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing = vec2(8.0, 0.0);
            ui.strong("Time of Day");
            ui.label(format!(
                "({:02}:{:02})",
                (self.time_of_day / 3600.0 * 24.0).floor() as u32,
                ((self.time_of_day / 3600.0 * 24.0 * 60.0) % 60.0).floor() as u32
            ));
        });

        ui.spacing_mut().slider_width = ui.available_width();

        const DAYNIGHT_GRADIENT: ImageSource =
            egui::include_image!("../../../assets/ui/daynight_gradient_bar.png");
        Image::new(DAYNIGHT_GRADIENT).paint_at(
            ui,
            Rect::from_min_size(
                ui.cursor().min + vec2(0.0, 6.0),
                Vec2::new(ui.available_width(), 8.0),
            ),
        );

        ui.scope(|ui| {
            ui.style_mut().visuals.widgets.inactive.bg_fill = egui::Color32::from_black_alpha(48);
            ui.style_mut().visuals.widgets.inactive.bg_stroke =
                egui::Stroke::new(8f32, egui::Color32::WHITE);

            egui::Slider::new(&mut self.time_of_day, 0.0..=3600.0)
                .show_value(false)
                .handle_shape(egui::style::HandleShape::Rect { aspect_ratio: 0.5 })
                .ui(ui);
        });

        ui.checkbox(&mut self.animate_time_of_day, "Automate Time")
            .on_hover_text("Automatically animate time of day");

        if self.animate_time_of_day {
            ui.horizontal(|ui| {
                ui.label("Time scale:");
                egui::DragValue::new(&mut self.time_scale)
                    .fixed_decimals(1)
                    .speed(0.1)
                    .ui(ui);
            });
        }

        ui.spacing_mut().slider_width = ui.available_width() * 0.75;
        egui::Slider::new(&mut self.camera.fov_y, 10.0..=120.0)
            .text("Camera FOV")
            .show_value(true)
            .ui(ui);

        ui.spacing_mut().slider_width = 256.0;
        let mut resolution_scale = view_surfaces.resolution_scale();
        if ui
            .add(
                egui::Slider::new(&mut resolution_scale, 0.25..=2.0)
                    .step_by(0.25)
                    .text("Resolution Scale")
                    .custom_formatter(|value, _| format!("{:.0}%", value * 100.0)),
            )
            .changed()
        {
            view_surfaces.set_resolution_scale(resolution_scale);
        }

        ui.separator();

        if is_shadowkeep {
            let mut global_lighting = ConVars::get_flag("render.global_lighting");
            if ui
                .checkbox(&mut global_lighting, "Global Lighting")
                .on_hover_text("Directional global lighting for the Shadowkeep sun.")
                .changed()
            {
                let _ = ConVars::set("render.global_lighting", global_lighting.into());
            }

            let mut sky = ConVars::get_flag("render.sky");
            if ui
                .checkbox(&mut sky, "Sky")
                .on_hover_text("Package-authored atmosphere with the procedural fallback.")
                .changed()
            {
                let _ = ConVars::set("render.sky", sky.into());
            }

            let mut sky_objects = ConVars::get_flag("render.shadowkeep_sky_objects");
            if ui
                .checkbox(&mut sky_objects, "Authored Sky Objects")
                .on_hover_text("Map-authored Shadowkeep SkyTransparent geometry.")
                .changed()
            {
                let _ = ConVars::set("render.shadowkeep_sky_objects", sky_objects.into());
            }

            let mut sky_diagnostics = ConVars::get_flag("render.shadowkeep_sky_objects_ab");
            if ui
                .checkbox(&mut sky_diagnostics, "Capture Sky Diagnostics")
                .on_hover_text(
                    "One-shot SkyTransparent stage and DrawIndexed capture. It disables itself \
                     after completion.",
                )
                .changed()
            {
                let _ = ConVars::set("render.shadowkeep_sky_objects_ab", sky_diagnostics.into());
            }

            let mut feature_matrix = ConVars::get_flag("render.shadowkeep_feature_matrix");
            if ui
                .checkbox(&mut feature_matrix, "Collect Feature Matrix")
                .on_hover_text(
                    "Diagnostic only: records normalized map resource counts to a small JSON \
                     manifest.",
                )
                .changed()
            {
                let _ = ConVars::set("render.shadowkeep_feature_matrix", feature_matrix.into());
            }
        }

        ui.checkbox(&mut view_settings.vertex_ao, "Vertex AO")
            .setting_description_tooltip(
                "Enables ambient occlusion based on mesh vertex data.\nCan highly impact the look \
                 and feel of a scene, as it darkens indoor areas and crevices.",
                PerformanceImpact::None,
            );

        ui.add_enabled_ui(!is_shadowkeep, |ui| {
            ui.checkbox(&mut view_settings.bloom, "Bloom")
                .setting_description_tooltip(
                    "Enables bloom effect, which adds a glow to bright areas of the scene.",
                    PerformanceImpact::Low,
                );

            ui.checkbox(&mut view_settings.volumetrics, "Volumetrics")
                .setting_description_tooltip(
                    "Enables volumetric lighting effects, such as light shafts and fog.",
                    PerformanceImpact::Medium,
                );

            ui.checkbox(&mut view_settings.anti_aliasing, "Anti-Aliasing")
                .setting_description_tooltip(
                    "Enables FXAA anti-aliasing to smooth out jagged edges.",
                    PerformanceImpact::Low,
                );
        });

        ui.checkbox(&mut view_settings.shadows, "Local Shadows")
            .on_hover_text(
                "Authored Shadowkeep spotlight shadows. Local shadow maps update incrementally \
                 using the configured per-frame budget.",
            );
        if view_settings.shadows {
            ui.add(
                egui::Slider::new(&mut view_settings.local_shadow_updates_per_frame, 1..=8)
                    .text("Local Shadow Updates / Frame"),
            );
        }

        ui.collapsing("Advanced", |ui| {
            ui.add_enabled_ui(!is_shadowkeep, |ui| {
                ui.checkbox(&mut view_settings.multithreading, "Multi-threaded Submit")
                    .setting_description_tooltip(
                        "Enables multi-threaded submission of commands to the GPU. May improve \
                         performance on systems with many CPU cores, but can introduce stuttering \
                         on older systems",
                        PerformanceImpact::High,
                    );

                ui.checkbox(&mut view_settings.hzb_culling, "HZB Culling")
                    .setting_description_tooltip(
                        "Enables Hierarchical Z-Buffer (HZB) culling to optimize rendering by \
                         discarding occluded objects.",
                        PerformanceImpact::High,
                    );
            });
        });

        ui.checkbox(&mut view_settings.sun_shadows, "Sun Shadows")
            .on_hover_text("Cascaded directional shadows for the Shadowkeep sun.");
        if is_shadowkeep {
            ui.weak(
                "Bloom, volumetrics, anti-aliasing, threaded submit, and HZB are not connected to \
                 the Shadowkeep pass graph.",
            );
        }
    }

    pub fn render(&mut self, delta_time: f32, resolution: (u32, u32)) {
        let frame_delta_time = if self.diagnostic_freeze {
            0.0
        } else {
            delta_time
        };
        let frame_time = if self.diagnostic_freeze {
            self.frozen_render_time
        } else {
            self.start_time.elapsed().as_secs_f32()
        };
        s_update_object_channels(&self.world);

        let resolution = if self.lock_resolution {
            (1920, 1080)
        } else {
            resolution
        };

        let framebuffer_resolution = self.view.framebuffer_resolution();

        if framebuffer_resolution != self.surface.get_desc().resolution() {
            let (texture, srv) = Self::create_surface(&self.renderer.gpu, framebuffer_resolution)
                .expect("Failed to resize scene surface");
            self.surface = texture;
            self.surface_srv = srv;
        }

        if self.animate_time_of_day && !self.diagnostic_freeze {
            self.time_of_day += frame_delta_time * self.time_scale;
            self.time_of_day = self.time_of_day.rem_euclid(3600.0);
        }

        self.camera.aspect_ratio = resolution.0 as f32 / resolution.1 as f32;
        if !self.diagnostic_freeze || self.camera_input_while_frozen {
            self.controller.update_rotation(&mut self.camera);
        }
        self.camera.update();
        let camera_to_projective = self.camera.projection_matrix(self.camera.aspect_ratio);
        let world_to_camera = self.camera.view_matrix();
        self.view
            .update(world_to_camera, camera_to_projective, resolution);
        if !self.diagnostic_freeze {
            self.view
                .update_autoexposure(&self.renderer.gpu, frame_delta_time);
        }

        let is_shadowkeep = self.renderer.era() == RendererEra::Shadowkeep;
        let (shadowkeep_sun_direction, shadowkeep_daylight) =
            shadowkeep_sun_state(self.time_of_day, self.sun_light_angle);
        let manual_sun_direction = Vec3::new(
            self.sun_light_angle.to_radians().cos(),
            self.sun_light_angle.to_radians().sin(),
            0.7,
        )
        .normalize()
        .extend(0.0);
        let mut packet_misc = FramePacketMisc {
            delta_time: frame_delta_time,
            time: frame_time,
            time_of_day: (self.time_of_day / 3600.0).fract(),
            subscribed_features: self.view.subscribed_features,
            shadowkeep_sun_direction: is_shadowkeep.then_some(shadowkeep_sun_direction),
            shadowkeep_daylight: is_shadowkeep.then_some(shadowkeep_daylight),
            ..Default::default()
        };

        {
            if let Some((_, (atmos, _visibility))) = self
                .world
                .query::<(&AtmosphereData, Option<&MapEntityVisibility>)>()
                .iter()
                .find(|(_, (_, visibility))| {
                    !visibility.is_some_and(|visibility| !visibility.visible)
                })
            {
                packet_misc.atmosphere = atmos.clone();
            } else {
                packet_misc.atmosphere = Default::default();
            }
        }

        let mut cmd = CommandList::from_device_context(
            &self.renderer.gpu,
            self.renderer.gpu.context().clone(),
        );
        let _gpuspan = self.renderer.profiler.scope(&cmd, "Scene::render (total)");
        self.renderer.frame_packet.write().begin_frame(packet_misc);
        self.renderer
            .externs
            .write()
            .globals
            .copy_from_slice(&self.global_channels);

        if is_shadowkeep {
            // The preserved package hash table is positional. This setter only
            // becomes active when the package contains an exact FNV1 match;
            // the renderer's explicit era-specific state remains authoritative.
            self.renderer
                .externs
                .write()
                .set_global_channel_by_name("sun_light_direction", shadowkeep_sun_direction);
        } else if let Some((_, directions)) = self.world.query::<&SunDirections>().iter().next() {
            let time_of_day_half = self.time_of_day / 2.0;
            let a = time_of_day_half.floor() as usize % 1800;
            let b = time_of_day_half.ceil() as usize % 1800;
            let t = time_of_day_half.fract();

            let angles = &directions.sun_directions;
            let va = angles.get(a).copied().unwrap_or_default();
            let vb = angles.get(b).copied().unwrap_or_default();
            let sun_direction = va.lerp(vb, t);

            let angles = &directions.atmosphere_directions;
            let va = angles.get(a).copied().unwrap_or_default();
            let vb = angles.get(b).copied().unwrap_or_default();
            let atmos_direction = va.lerp(vb, t);

            self.renderer
                .externs
                .write()
                .set_global_channel_by_name("sun_light_direction", sun_direction);

            self.renderer
                .externs
                .write()
                .set_global_channel_by_name("sun_atmosphere_direction", atmos_direction);
        } else {
            self.renderer
                .externs
                .write()
                .set_global_channel_by_name("sun_light_direction", manual_sun_direction);
        }

        if self.render_mode == RenderMode::Lookdev {
            self.renderer.externs.write().set_global_channel_by_name(
                "sun_light_direction",
                -self.camera.forward().extend(0.0),
            );
        }

        {
            s_extract_ambient_occlusion(&self.world);
            let mut fp = self.renderer.frame_packet.write();
            s_extract_render_objects(&self.world, &mut fp);

            {
                let mut ext = self.renderer.externs.write();
                ext.unk_sequencer_values[0] = Vec4::splat(self.time_of_day / 3600.0);

                if self.renderer.era() == RendererEra::Current {
                    let distance_to_night = (self.time_of_day / 1800.0 - 1.0).abs();
                    ext.set_global_channel_by_name(
                        "cubemap_relighting_sky_intensity",
                        Vec4::splat(1.0 - distance_to_night),
                    );
                }
            }

            if self.automate_channels && !self.diagnostic_freeze {
                s_evaluate_global_channel_expressions(&self.world);
            }

            if self.renderer.era() == RendererEra::Current {
                // Fixes III not showing up in the Singularity.
                self.renderer
                    .externs
                    .write()
                    .set_global_channel_by_id(0x2C53817A, Vec4::splat(1.0));
            }
        }

        if is_shadowkeep && !self.shadowkeep_sky_channels_logged {
            let channels = {
                let ext = self.renderer.externs.read();
                [
                    ("sun_color", ext.try_get_global_channel_by_name("sun_color")),
                    (
                        "sun_intensity",
                        ext.try_get_global_channel_by_name("sun_intensity"),
                    ),
                    (
                        "skybox_sun_color",
                        ext.try_get_global_channel_by_name("skybox_sun_color"),
                    ),
                    (
                        "skybox_sun_intensity",
                        ext.try_get_global_channel_by_name("skybox_sun_intensity"),
                    ),
                    (
                        "up_ambient_color",
                        ext.try_get_global_channel_by_name("up_ambient_color"),
                    ),
                    (
                        "up_ambient_intensity",
                        ext.try_get_global_channel_by_name("up_ambient_intensity"),
                    ),
                    (
                        "down_ambient_color",
                        ext.try_get_global_channel_by_name("down_ambient_color"),
                    ),
                    (
                        "down_ambient_intensity",
                        ext.try_get_global_channel_by_name("down_ambient_intensity"),
                    ),
                    (
                        "skybox_up_ambient_color",
                        ext.try_get_global_channel_by_name("skybox_up_ambient_color"),
                    ),
                    (
                        "skybox_up_ambient_intensity",
                        ext.try_get_global_channel_by_name("skybox_up_ambient_intensity"),
                    ),
                    (
                        "skybox_down_ambient_color",
                        ext.try_get_global_channel_by_name("skybox_down_ambient_color"),
                    ),
                    (
                        "skybox_down_ambient_intensity",
                        ext.try_get_global_channel_by_name("skybox_down_ambient_intensity"),
                    ),
                    (
                        "sky_color_override",
                        ext.try_get_global_channel_by_name("sky_color_override"),
                    ),
                    (
                        "sky_snapshot_intensity",
                        ext.try_get_global_channel_by_name("sky_snapshot_intensity"),
                    ),
                ]
            };
            tracing::info!(
                map = %self.scene_id,
                time_of_day = self.time_of_day,
                sun_direction = ?shadowkeep_sun_direction,
                channels = ?channels,
                "Shadowkeep package sky channel provenance"
            );
            self.shadowkeep_sky_channels_logged = true;
        }

        let is_everything_loaded = s_are_all_objects_loaded(&self.world, &self.renderer);
        let can_draw_static_shadowmaps = is_everything_loaded && self.view.settings().shadows;

        {
            profiling::scope!("render_shadows");
            let _gpuspan = self.renderer.profiler.scope(&cmd, "render_shadows");
            if can_draw_static_shadowmaps {
                s_extract_all_shadowmaps(
                    &mut self.world,
                    &self.renderer,
                    self.view.settings().local_shadow_updates_per_frame,
                );
                s_submit_all_shadowmaps(
                    &mut self.world,
                    &mut cmd,
                    &self.renderer,
                    self.frame_index,
                );
            }

            let debug_pipeline: Option<DebugPipeline> = self.render_mode.into();
            let wants_sun = debug_pipeline.is_none_or(|pipeline| pipeline.has_sun());

            if ConVars::get_flag("render.global_lighting")
                && self.view.settings().sun_shadows
                && wants_sun
                && (!is_shadowkeep || shadowkeep_daylight > 0.001)
            {
                self.draw_sun_shadows(&mut cmd);
            } else if let ViewKind::Main(view) = &mut self.view.kind {
                view.sun_shadow_map_cascades.clear();
            }
        }

        {
            profiling::scope!("prepare");
            let _gpuspan = self.renderer.profiler.scope(&cmd, "prepare");

            if let ViewKind::Main(view) = &self.view.kind {
                profiling::scope!("download_hzb");
                let _gpuspan = self.renderer.profiler.scope(&cmd, "download_hzb");
                self.view.hzb = if view.settings.hzb_culling {
                    Hzb::download(
                        &self.renderer.gpu,
                        &view.gbuffers.hzb_depth_chain_cpu.lock(),
                        &self.camera,
                    )
                } else {
                    Hzb::EMPTY
                };
            }

            {
                profiling::scope!("visibility");
                let _gpuspan = self.renderer.profiler.scope(&cmd, "visibility");
                self.renderer.cull_view(View::MAIN, &self.view);
            }

            {
                profiling::scope!("prepare/upload");
                let _gpuspan = self.renderer.profiler.scope(&cmd, "prepare_upload");

                for node in self.renderer.frame_packet.read().iter_visible(View::MAIN) {
                    if let Some(render_object) = self
                        .renderer
                        .objects
                        .write()
                        .get_mut(node.render_object_handle.into())
                    {
                        render_object.prepare(&self.renderer, View::MAIN, &*node.data);
                    } else if node.render_object_handle.is_valid() {
                        error!("Render object not found: {:?}", node.render_object_handle);
                    }
                }
            }
            // Sort nodes by distance
            self.renderer
                .frame_packet
                .write()
                .frame_nodes
                .sort_by(|n1, n2| {
                    n2.distance
                        .partial_cmp(&n1.distance)
                        .unwrap_or(std::cmp::Ordering::Equal)
                });
        }

        {
            // self.renderer
            //     .submit_sun_shadows(&mut cmd, &self.camera, &self.view);
            self.renderer
                .submit_view(&mut cmd, &self.view, self.render_mode.into());
        }

        if let ViewKind::Main(view) = &self.view.kind {
            cmd.copy_resource(
                &self.renderer.surfaces().get(view.output).texture,
                &self.surface,
            );
        }

        self.global_channels
            .copy_from_slice(&self.renderer.externs.read().globals);

        // let cmd = self.draw_world(delta_time);
        // self.renderer.gpu.submit_command_list(cmd);

        // if self.show_debug_text
        // {
        //     let gpu = &self.renderer.gpu;
        //     let context = gpu.context();

        //     context.rasterizer_set_viewports(&[d3d11::Viewport::builder()
        //         .width(gpu.swapchain_resolution().0 as f32)
        //         .height(gpu.swapchain_resolution().1 as f32)
        //         .build()]);
        //     context.output_merger_set_render_targets(&[Some(gpu.acquire_rtv())], None);
        //     context.output_merger_set_depth_stencil_state(None, 0);
        //     context.rasterizer_set_state(None);
        //     self.renderer.debug_text.lock().draw(&self.renderer.gpu);
        // }

        drop(_gpuspan);
        self.renderer.profiler.end_frame();
        self.frame_index = self.frame_index.wrapping_add(1).max(1);

        static FRAME_COUNT: AtomicUsize = AtomicUsize::new(0);
        if FRAME_COUNT
            .fetch_add(1, Ordering::Relaxed)
            .is_multiple_of(10)
        {
            self.profiler_results = Some(self.renderer.profiler.get_results_string());
        }
    }

    fn draw_sun_shadows(&mut self, cmd: &mut CommandList) {
        let mut sun_dir = self
            .renderer
            .frame_packet
            .read()
            .misc
            .shadowkeep_sun_direction
            .unwrap_or_else(|| {
                self.renderer
                    .externs
                    .read()
                    .get_global_channel_by_name("sun_light_direction")
            })
            .xyz();
        if sun_dir.length() < 0.01 {
            sun_dir = Vec3::Z;
        }
        let sun_dir = -sun_dir.normalize();

        let total_frustum = {
            let (world_to_camera, camera_to_projective) =
                self.camera
                    .build_shadow_cascade(sun_dir, 0.05, Renderer::MAX_CASCADE_DISTANCE);

            Frustum::from_view_and_projection(world_to_camera, camera_to_projective)
        };

        self.renderer.cull_view(View::SUN, &total_frustum);
        {
            cmd_event_span!(cmd, "prepare_sun_view");
            let _gpuspan = self.renderer.profiler.scope(cmd, "prepare_sun_view");

            for node in self.renderer.frame_packet.read().iter_visible(View::SUN) {
                if let Some(render_object) = self
                    .renderer
                    .objects
                    .write()
                    .get_mut(node.render_object_handle.into())
                {
                    render_object.prepare(&self.renderer, View::SUN, &*node.data);
                } else if node.render_object_handle.is_valid() {
                    error!("Render object not found: {:?}", node.render_object_handle);
                }
            }
        }

        let mut sun_shadow_map_cascades = vec![];
        cmd.set_depth_mode(DepthMode::Forward);
        for c in 0..Renderer::NUM_CASCADES {
            profiling::scope!("submit_sun_shadow_cascade", &format!("cascade {}", c));
            let _gpuspan = self
                .renderer
                .profiler
                .scope(cmd, format!("shadow_cascade_{c}"));
            let shadow_view = &mut self.sun_shadow_views[c];
            let ViewKind::Shadow(sv) = &mut shadow_view.kind else {
                warn!("Invalid view kind for sun shadow view");
                continue;
            };
            sv.index = View::SUN;
            // self.bind_surfaces(cmd, &[], Some(shadow_map));
            // self.clear_surface_depth(cmd, shadow_map, 1.0, 0);

            let (z_near, z_far) = Renderer::get_cascade_distance_range(c);

            let (world_to_camera, camera_to_projective) =
                self.camera.build_shadow_cascade(sun_dir, z_near, z_far);

            // let frustum = Frustum::from_view_and_projection(world_to_camera, camera_to_projective);
            sun_shadow_map_cascades.push((
                camera_to_projective * world_to_camera,
                sv.shadow_map
                    .srv(0)
                    .expect("Sun shadow view is missing an SRV")
                    .clone(),
            ));

            shadow_view.update(
                world_to_camera,
                camera_to_projective,
                shadow_view.resolution(),
            );

            self.renderer.submit_view(cmd, shadow_view, None);
        }
        cmd.set_depth_mode(DepthMode::Reverse);

        if let ViewKind::Main(mv) = &mut self.view.kind {
            mv.sun_shadow_map_cascades = sun_shadow_map_cascades;
        }
    }

    pub fn output_srv(&self) -> &d3d11::ShaderResourceView {
        &self.surface_srv
    }

    pub fn copy_output_as_texture(&self) -> anyhow::Result<d3d11::ShaderResourceView> {
        let texture = self
            .renderer
            .gpu
            .create_texture2d(&self.surface.get_desc(), None)?;
        self.renderer
            .gpu
            .context()
            .copy_resource(&self.surface, &texture);
        Ok(self
            .renderer
            .gpu
            .create_shader_resource_view(&texture, None)?)
    }

    pub fn focus_on(&mut self, position: Vec3) {
        match &mut self.controller {
            CameraController::Orbit { target, .. } => {
                *target = position;
            }
            CameraController::FirstPerson { .. } => {
                self.camera.position = position;
            }
        }
    }

    pub fn focus_fit_ortho(&mut self, aabb: &AxisAlignedBBox) {
        match &mut self.controller {
            CameraController::Orbit { target, .. } => {
                *target = aabb.center();
                self.camera.max_ortho_width = aabb.extents().length() * 0.75;
            }
            CameraController::FirstPerson { .. } => {}
        }
    }

    pub fn focus_inspection_node(&mut self, node: &MapInspectionNode) {
        if let Some(bounds) = node.bounds {
            self.focus_bounds(&bounds);
        } else if let Some(transform) = node.transform {
            self.focus_point(transform.translation, transform.scale.length().max(1.0));
        }
    }

    pub fn focus_bounds(&mut self, bounds: &AxisAlignedBBox) {
        let center = bounds.center();
        let distance = (bounds.radius().max(1.0) * 2.4).clamp(2.0, 50_000.0);
        let forward = self.camera.forward();
        self.focus_on(center);
        if matches!(self.controller, CameraController::FirstPerson { .. }) {
            self.camera.position = center - forward * distance;
        }
    }

    pub fn focus_point(&mut self, point: Vec3, scale: f32) {
        let distance = (scale.abs() * 2.0).clamp(2.0, 5.0);
        let forward = self.camera.forward();
        self.focus_on(point);
        if matches!(self.controller, CameraController::FirstPerson { .. }) {
            self.camera.position = point - forward * distance;
        }
    }

    pub fn teleport_to_transform(&mut self, transform: Transform) {
        self.camera.position = transform.translation;
        self.camera.rotation = transform.rotation;
        let forward = self.camera.forward();
        self.controller.set_yaw_pitch(glam::Vec2::new(
            forward.y.atan2(forward.x).to_degrees(),
            (-forward.z).atan2(forward.x.hypot(forward.y)).to_degrees(),
        ));
    }

    fn show_global_channel_editor(&mut self, ui: &mut Ui) {
        ui.style_mut()
            .text_styles
            .insert(TextStyle::Body, FontId::proportional(16.0));
        ui.style_mut()
            .text_styles
            .insert(TextStyle::Button, FontId::proportional(16.0));

        let automated_ids = s_get_all_global_channel_ids(&self.world);
        ui.heading("Global Channels");
        ui.checkbox(&mut self.automate_channels, "Sequencer Automation");
        let externs = self.renderer.externs.read();
        egui::ScrollArea::vertical().show(ui, |ui| {
            for (i, channel) in self.global_channels.iter_mut().enumerate() {
                let Some(channel_id) = externs.global_ids.get(i) else {
                    continue;
                };
                let is_automated = automated_ids.contains(channel_id) && self.automate_channels;
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 4.0;
                    if let Some(name) = get_global_channel_name(*channel_id) {
                        ui.label(format!("Channel #{i} {name} (0x{channel_id:08X})"));
                    } else {
                        ui.label(format!("Channel #{i} 0x{channel_id:08X}"));
                    }
                    if is_automated {
                        ui.weak("(automated)");
                    }
                });
                ui.add_enabled_ui(!is_automated, |ui| {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().interact_size = egui::vec2(100.0, 32.0);
                        egui::DragValue::new(&mut channel.x)
                            .fixed_decimals(4)
                            .speed(0.01)
                            .ui(ui);
                        egui::DragValue::new(&mut channel.y)
                            .fixed_decimals(4)
                            .speed(0.01)
                            .ui(ui);
                        egui::DragValue::new(&mut channel.z)
                            .fixed_decimals(4)
                            .speed(0.01)
                            .ui(ui);
                        egui::DragValue::new(&mut channel.w)
                            .fixed_decimals(4)
                            .speed(0.01)
                            .ui(ui);
                    });
                });
            }
        });
    }

    fn load_camera_cbuffers(&mut self) {
        if let Ok(data) = std::fs::read("cb12.bin") {
            if data.len() < 240 {
                error!("Not enough data to reconstruct camera (need at least 240 bytes)");
                return;
            }

            let data_vec: &[Vec4] = bytemuck::cast_slice(&data);
            let camera_to_world =
                Mat4::from_cols(data_vec[4], data_vec[5], data_vec[6], data_vec[7]);
            let camera_to_projective =
                Mat4::from_cols(data_vec[11], data_vec[12], data_vec[13], data_vec[14]);

            let (_, rotation, position) = camera_to_world.to_scale_rotation_translation();
            let fov = (1.0 / camera_to_projective.y_axis.y).atan().to_degrees() * 2.0;
            info!(
                "Parsed camera from view scope data: pos={position:?} rot={rotation:?} fov={fov}"
            );

            let camera = &mut self.camera;
            camera.set_position(position);
            camera.set_fov(fov);

            fn extract_pitch_yaw(inv_view: Mat4) -> (f32, f32) {
                let r2 = Vec3::new(inv_view.x_axis.z, inv_view.y_axis.z, inv_view.z_axis.z);
                let forward = (-r2).normalize();
                let pitch = (-forward.z).clamp(-1.0, 1.0).asin().to_degrees();
                let yaw = forward.y.atan2(forward.x).to_degrees();

                let yaw = if yaw > 180.0 {
                    yaw - 360.0
                } else if yaw <= -180.0 {
                    yaw + 360.0
                } else {
                    yaw
                };

                (pitch, yaw)
            }

            let (pitch, yaw) = extract_pitch_yaw(camera_to_world.transpose());
            self.controller.set_yaw_pitch(glam::vec2(yaw, pitch));
        }

        if let Ok(cb13_data) = std::fs::read("cb13.bin") {
            let cb13_vec: &[Vec4] = bytemuck::cast_slice(&cb13_data);
            if cb13_vec.len() < 2 {
                error!("Not enough data to reconstruct camera (need at least 32 bytes)");
                return;
            }

            let settings = self.view.settings_mut();
            settings.autoexposure = false;
            settings.exposure_scale = cb13_vec[1].x;
            settings.exposure_illum_relative = cb13_vec[1].w;
            info!(
                "Parsed frame scope data, exposure scale: {}, illumination relative: {}",
                settings.exposure_scale, settings.exposure_illum_relative
            );
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RenderMode {
    Lookdev,
    Shaded,
    ShadedNoSun,
    ShadedNoAtm,
    ShadingOnly,
    // Matcap,

    // Material:
    Albedo,
    Smoothness,
    Metalness,
    AmbientOcclusion,
    Emission,
    EmissionIntensity,
    Transmission,
    IridescenceId,

    // Geometry:
    DepthEdges,
    WorldNormal,
    Overdraw,

    // Lighting:
    LightDiffuse,
    LightSpecular,
}

impl RenderMode {
    /// Returns true if the render mode UI changed the value
    pub fn ui(&mut self, ui: &mut Ui) -> bool {
        ui.style_mut()
            .text_styles
            .insert(TextStyle::Button, FontId::proportional(16.0));

        let mut changed = false;
        egui::ComboBox::from_id_salt("Render Mode")
            .height(400.0)
            .selected_text(format!("{} {:?}", GoogleMaterialSymbols::EvShadow, self))
            .show_ui(ui, |ui| {
                ui.style_mut()
                    .text_styles
                    .insert(TextStyle::Button, FontId::proportional(16.0));
                ui.style_mut().spacing.button_padding = Vec2::new(8.0, 2.0);
                ui.style_mut().spacing.item_spacing = Vec2::ZERO;

                macro_rules! mode {
                    ($ui:ident, $variant:expr, $name:literal) => {
                        if $ui.selectable_label(*self == $variant, $name).clicked() {
                            *self = $variant;
                            changed = true;
                        }
                    };
                }

                mode!(ui, RenderMode::Lookdev, "Lookdev");
                mode!(ui, RenderMode::Shaded, "Shaded");
                mode!(ui, RenderMode::ShadedNoAtm, "Shaded (No atmosphere)");
                mode!(ui, RenderMode::ShadedNoSun, "Shaded (No sun)");
                mode!(
                    ui,
                    RenderMode::ShadingOnly,
                    "Shading Only (Local-light/deferred shading only)"
                );
                // mode!(ui, RenderMode::Matcap, "Matcap");

                ui.section_separator("Material:");
                mode!(ui, RenderMode::Albedo, "Albedo");
                mode!(ui, RenderMode::Smoothness, "Smoothness");
                mode!(ui, RenderMode::Metalness, "Metalness");
                mode!(ui, RenderMode::AmbientOcclusion, "Ambient Occlusion");
                mode!(ui, RenderMode::Emission, "Emission");
                mode!(ui, RenderMode::EmissionIntensity, "Emission Intensity");
                mode!(ui, RenderMode::Transmission, "Transmission");
                mode!(ui, RenderMode::IridescenceId, "Iridescence ID");

                ui.section_separator("Geometry:");
                mode!(ui, RenderMode::DepthEdges, "Depth Edges");
                mode!(ui, RenderMode::WorldNormal, "World Normal");
                mode!(ui, RenderMode::Overdraw, "Overdraw");

                ui.section_separator("Lighting:");
                mode!(ui, RenderMode::LightDiffuse, "Diffuse Light");
                mode!(ui, RenderMode::LightSpecular, "Specular Light");
            });

        changed
    }
}

impl From<RenderMode> for Option<DebugPipeline> {
    fn from(val: RenderMode) -> Self {
        match val {
            RenderMode::Lookdev => None,
            RenderMode::Shaded => Some(DebugPipeline::Shaded),
            RenderMode::ShadedNoAtm => Some(DebugPipeline::ShadedNoAtm),
            RenderMode::ShadedNoSun => Some(DebugPipeline::ShadedNoSun),
            RenderMode::ShadingOnly => Some(DebugPipeline::ShadingOnly),
            // RenderMode::Matcap => Some(DebugPipeline::Matcap),
            RenderMode::Albedo => Some(DebugPipeline::Albedo),
            RenderMode::Smoothness => Some(DebugPipeline::Smoothness),
            RenderMode::Metalness => Some(DebugPipeline::Metalness),
            RenderMode::AmbientOcclusion => Some(DebugPipeline::AmbientOcclusion),
            RenderMode::Emission => Some(DebugPipeline::Emission),
            RenderMode::EmissionIntensity => Some(DebugPipeline::EmissionIntensity),
            RenderMode::Transmission => Some(DebugPipeline::Transmission),
            RenderMode::IridescenceId => Some(DebugPipeline::Overcoat),
            RenderMode::DepthEdges => Some(DebugPipeline::DepthEdges),
            RenderMode::WorldNormal => Some(DebugPipeline::WorldNormal),
            RenderMode::Overdraw => Some(DebugPipeline::Overdraw),
            RenderMode::LightDiffuse => Some(DebugPipeline::LightDiffuse),
            RenderMode::LightSpecular => Some(DebugPipeline::LightSpecular),
        }
    }
}

impl ExternalDataWidgetExt for FeatureRendererSubscription {
    fn show_input(&mut self, ui: &mut Ui) -> egui::Response {
        ui.style_mut()
            .text_styles
            .insert(TextStyle::Button, FontId::proportional(16.0));

        egui::ComboBox::from_id_salt("Feature Renderers")
            .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
            .height(400.0)
            .selected_text(format!(
                "{} Enabled Features",
                GoogleMaterialSymbols::CheckBox
            ))
            .show_ui(ui, |ui| {
                ui.style_mut()
                    .text_styles
                    .insert(TextStyle::Button, FontId::proportional(16.0));
                ui.style_mut().spacing.button_padding = Vec2::new(8.0, 2.0);
                ui.style_mut().spacing.item_spacing = Vec2::ZERO;

                let ctrl = ui.input(|i| i.modifiers.ctrl);
                let alt = ui.input(|i| i.modifiers.alt);
                macro_rules! feature {
                    ($ui:ident, $flag:expr, $name:literal) => {
                        if $ui.selectable_label(self.contains($flag), $name).clicked() {
                            if ctrl {
                                self.clear();
                                self.insert($flag);
                            } else if alt {
                                *self = FeatureRendererSubscription::all();
                                self.remove($flag);
                            } else if self.contains($flag) {
                                self.remove($flag);
                            } else {
                                self.insert($flag);
                            }
                        }
                    };
                }

                feature!(
                    ui,
                    FeatureRendererSubscription::CHUNKED_INSTANCE_OBJECTS,
                    "Static Objects"
                );
                feature!(
                    ui,
                    FeatureRendererSubscription::TERRAIN_PATCH,
                    "Terrain Patches"
                );
                feature!(
                    ui,
                    FeatureRendererSubscription::RIGID_OBJECT,
                    "Rigid Objects"
                );
                feature!(
                    ui,
                    FeatureRendererSubscription::SKY_TRANSPARENT,
                    "Sky Transparents"
                );
                feature!(
                    ui,
                    FeatureRendererSubscription::SPEEDTREE_TREES,
                    "Decorators"
                );
                feature!(
                    ui,
                    FeatureRendererSubscription::DYNAMIC_DECALS,
                    "Dynamic Decals"
                );
                feature!(ui, FeatureRendererSubscription::ROAD_DECALS, "Road Decals");
                feature!(ui, FeatureRendererSubscription::WATER, "Water");
                ui.add_enabled_ui(false, |ui| {
                    feature!(ui, FeatureRendererSubscription::LENS_FLARES, "Lens Flares");
                    feature!(ui, FeatureRendererSubscription::PARTICLES, "Particles");
                });

                ui.section_separator("Lighting");
                feature!(ui, FeatureRendererSubscription::CUBEMAPS, "Cubemaps");
                feature!(
                    ui,
                    FeatureRendererSubscription::CHUNKED_LIGHTS,
                    "Chunked Lights"
                );
                feature!(
                    ui,
                    FeatureRendererSubscription::DEFERRED_LIGHTS,
                    "Deferred Lights"
                );
            })
            .response
    }
}

trait SettingDescriptionTooltipExt {
    fn setting_description_tooltip(
        self,
        description: &str,
        performance_impact: PerformanceImpact,
    ) -> Self;
}

impl SettingDescriptionTooltipExt for Response {
    fn setting_description_tooltip(
        self,
        description: &str,
        performance_impact: PerformanceImpact,
    ) -> Response {
        self.on_hover_ui(|ui| {
            ui.style_mut()
                .text_styles
                .insert(TextStyle::Body, FontId::proportional(16.0));

            let perf_color = match performance_impact {
                PerformanceImpact::None => egui::Color32::GRAY,
                PerformanceImpact::Low => egui::Color32::GREEN,
                PerformanceImpact::Medium => egui::Color32::YELLOW,
                PerformanceImpact::High => egui::Color32::RED,
            };

            ui.label(description);
            ui.separator();
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing = Vec2::splat(0.0);
                ui.label("Performance Impact: ");
                ui.label(
                    RichText::new(format!("{:?}", performance_impact))
                        .color(perf_color)
                        .strong(),
                );
            });
        })
    }
}
#[derive(Debug)]
enum PerformanceImpact {
    None,
    Low,
    Medium,
    High,
}

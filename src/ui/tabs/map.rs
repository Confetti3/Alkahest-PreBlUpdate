use std::sync::Arc;

use alkahest_render::{Renderer, camera::Camera};
use egui::{Align, Color32, Layout, Rect, RichText, UiBuilder, vec2};
use glam::{Quat, Vec2, Vec3};
use tiger_pkg::TagHash;

use super::{Tab, TabResult};
use crate::{
    app::SharedState,
    task::Task,
    ui::{
        bubble_browser::bubble_display_name,
        map_workspace::{MapWorkspaceAction, MapWorkspaceState, show as show_workspace},
        scene::{Scene, controller::CameraController},
        util::{DButton, UiExt},
    },
    world::shadowkeep_map::{MapLoadProgress, MapLoadReport, load_shadowkeep_map_into_world},
};

type MapLoadTask = Task<anyhow::Result<(hecs::World, MapLoadReport)>>;

/// An interactive Shadowkeep map. Construction does no package traversal; the
/// loader and the startup catalog own package work outside ordinary UI frames.
pub struct MapTab {
    pub tag: TagHash,
    pub name: String,
    load_task: Option<MapLoadTask>,
    progress: MapLoadProgress,
    report: Option<MapLoadReport>,
    scene: Option<Box<Scene>>,
    error: Option<String>,
    cancel_requested: bool,
    cancelled: bool,
    workspace: MapWorkspaceState,
    shared: Arc<SharedState>,
}

impl Drop for MapTab {
    fn drop(&mut self) {
        if self.load_task.is_some() {
            self.progress.cancel();
        }
    }
}

impl MapTab {
    pub fn new(tag: TagHash, name: String, shared: &Arc<SharedState>) -> anyhow::Result<Self> {
        Ok(Self {
            tag,
            name,
            load_task: None,
            progress: MapLoadProgress::default(),
            report: None,
            scene: None,
            error: None,
            cancel_requested: false,
            cancelled: false,
            workspace: MapWorkspaceState::default(),
            shared: shared.clone(),
        })
    }

    fn begin_load_if_ready(&mut self) -> anyhow::Result<()> {
        if self.load_task.is_some()
            || self.scene.is_some()
            || self.error.is_some()
            || self.cancelled
        {
            return Ok(());
        }
        if !self.shared.renderer_status.read().is_ready() {
            return Ok(());
        }
        let renderer = Renderer::instance().clone();
        let progress = self.progress.clone();
        let tag = self.tag;
        self.load_task = Some(Task::new(format!("shadowkeep_map_{tag}"), move || {
            load_shadowkeep_map_into_world(tag, &renderer, &progress)
        }));
        Ok(())
    }

    fn reset_load(&mut self) {
        debug_assert!(self.load_task.is_none());
        self.progress = MapLoadProgress::default();
        self.report = None;
        self.scene = None;
        self.error = None;
        self.cancel_requested = false;
        self.cancelled = false;
    }

    fn complete_load(&mut self) -> anyhow::Result<()> {
        let Some(task) = self.load_task.as_mut() else {
            return Ok(());
        };
        let Some(result) = task.get() else {
            return Ok(());
        };
        self.load_task = None;
        let (world, report) =
            result.map_err(|_| anyhow::anyhow!("Shadowkeep map-load task panicked"))??;
        if self.cancel_requested || report.cancelled {
            self.cancel_requested = false;
            self.cancelled = true;
            return Ok(());
        }
        let mut scene = Scene::new(
            Renderer::instance().clone(),
            Camera::default(),
            &self.shared,
            format!("shadowkeep_map_{}", self.tag),
        )?
        .with_controller(CameraController::new_first_person());
        scene.enable_camera_input_while_frozen();
        // Shadowkeep bounds are not consistently expressed in the main view's
        // culling space, so keep the complete authored scene visible.
        scene.view.disable_culling = true;
        let surfaces = scene.view.surfaces().unwrap();
        surfaces.set_resolution_scale(surfaces.resolution_scale().min(0.75));
        if let Some(spawn) = report
            .inspection
            .spawn_nodes
            .first()
            .and_then(|id| report.inspection.node(*id))
            .and_then(|node| node.transform)
        {
            scene.camera.position = spawn.translation + Vec3::Z * 2.0;
        } else if let Some(bounds) = report.world_bounds.or(report.placement_bounds) {
            let radius = bounds.radius().max(4.0);
            scene.camera.position = bounds.center() + Vec3::new(-radius, -radius, radius * 0.5);
            let direction = (bounds.center() - scene.camera.position).normalize();
            let yaw = direction.y.atan2(direction.x);
            let pitch = (-direction.z).atan2(direction.x.hypot(direction.y));
            scene.camera.rotation = Quat::from_rotation_z(yaw) * Quat::from_rotation_y(pitch);
            scene
                .controller
                .set_yaw_pitch(Vec2::new(yaw.to_degrees(), pitch.to_degrees()));
        }
        scene.set_world(world);
        self.report = Some(report);
        self.scene = Some(Box::new(scene));
        Ok(())
    }

    fn centered_status(ui: &mut egui::Ui, title: &str, detail: Option<&str>, action: &str) -> bool {
        let (_, rect) = ui.allocate_space(ui.available_size());
        ui.painter()
            .rect_filled(rect, 0, Color32::from_rgb(14, 24, 28));
        let mut clicked = false;
        ui.scope_builder(
            UiBuilder::new()
                .max_rect(rect)
                .layout(Layout::top_down(Align::Center)),
            |ui| {
                ui.add_space(((rect.height() - 160.0) * 0.5).max(0.0));
                ui.heading(title);
                if let Some(detail) = detail {
                    ui.label(detail);
                }
                ui.add_space(16.0);
                clicked = DButton::new(action)
                    .min_size(vec2(220.0, 60.0))
                    .ui(ui)
                    .clicked();
            },
        );
        clicked
    }

    pub fn ui(
        &mut self,
        ui: &mut egui::Ui,
        egui_d3d11: &mut egui_d3d11::D3D11Renderer,
    ) -> TabResult {
        if let Err(error) = self
            .begin_load_if_ready()
            .and_then(|_| self.complete_load())
        {
            self.error = Some(format!("{error:#}"));
        }
        if let (Some(scene), Some(report)) = (self.scene.as_mut(), self.report.as_ref()) {
            return match show_workspace(
                ui,
                scene,
                report,
                &self.shared,
                &mut self.workspace,
                egui_d3d11,
            ) {
                MapWorkspaceAction::OpenBubble(tag) => {
                    match Self::new(tag, bubble_display_name(tag), &self.shared) {
                        Ok(map) => TabResult::Open(Tab::Map(map)),
                        Err(error) => {
                            error!("Failed to open Shadowkeep map {tag}: {error:#}");
                            TabResult::Continue
                        }
                    }
                }
                MapWorkspaceAction::OpenTag(tag) => {
                    TabResult::Open(Tab::Inspector(super::inspector::InspectorTab::new(
                        tag,
                        crate::inspection::InspectionKind::Tag,
                    )))
                }
                MapWorkspaceAction::None => TabResult::Continue,
            };
        }

        ui.heading(&self.name);
        ui.label(format!("Shadowkeep bubble: {}", self.tag));
        ui.add_space(12.0);
        if let Some(error) = &self.error {
            if Self::centered_status(ui, "Map load failed", Some(error), "RETRY") {
                self.reset_load();
            }
            return TabResult::Continue;
        }
        if self.cancelled {
            if Self::centered_status(ui, "Map load cancelled", None, "RESTART") {
                self.reset_load();
            }
            return TabResult::Continue;
        }
        let renderer_status = self.shared.renderer_status.read().clone();
        if matches!(renderer_status, crate::app::RendererStatus::Disabled) {
            ui.label(
                RichText::new("Map renderer is disabled").color(Color32::from_rgb(144, 192, 224)),
            );
            ui.label(renderer_status.scene_diagnostic());
            return TabResult::Continue;
        }
        if !renderer_status.is_ready() {
            ui.label(
                RichText::new("Waiting for 3D renderer").color(Color32::from_rgb(224, 192, 96)),
            );
            ui.label(renderer_status.scene_diagnostic());
            return TabResult::Continue;
        }
        let progress = self.progress.snapshot();
        let (_, rect) = ui.allocate_space(ui.available_size());
        ui.painter()
            .rect_filled(rect, 0, Color32::from_rgb(14, 24, 28));
        ui.d_paint_spinner_at(Rect::from_center_size(rect.center(), vec2(64.0, 64.0)));
        let mut cancel_clicked = false;
        ui.scope_builder(
            UiBuilder::new()
                .max_rect(rect)
                .layout(Layout::top_down(Align::Center)),
            |ui| {
                ui.add_space((rect.height() * 0.5 + 42.0).max(0.0));
                if self.cancel_requested {
                    ui.label("Cancelling map load…");
                } else {
                    ui.label(format!(
                        "Loading map: {}/{} tables, {} entries, {} visual resources",
                        progress.tables_seen,
                        progress.total_tables,
                        progress.entries_seen,
                        progress.visual_resources_loaded
                    ));
                }
                ui.weak(format!(
                    "{} GPU assets requested · {} diagnostics",
                    progress.gpu_assets_requested, progress.diagnostics
                ));
                if !self.cancel_requested {
                    ui.add_space(12.0);
                    cancel_clicked = DButton::new("CANCEL")
                        .min_size(vec2(220.0, 60.0))
                        .ui(ui)
                        .clicked();
                }
            },
        );
        if cancel_clicked {
            self.progress.cancel();
            self.cancel_requested = true;
        }
        TabResult::Continue
    }
}

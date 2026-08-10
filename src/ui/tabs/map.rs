use std::sync::Arc;

use alkahest_data::shadowkeep::{
    SShadowkeepBubbleDefinition, SShadowkeepBubbleParent, SShadowkeepMapDataTable,
};
use alkahest_render::renderer::shadowkeep::{ShadowkeepPassState, pass_status_ledger};
use alkahest_render::{Renderer, camera::Camera};
use egui::{Color32, Rect, RichText, vec2};
use glam::{Quat, Vec2, Vec3};
use tiger_parse::PackageManagerExt;
use tiger_pkg::{TagHash, package_manager};

use crate::{
    app::SharedState,
    task::Task,
    ui::{
        scene::{Scene, controller::CameraController},
        util::UiExt,
    },
    world::shadowkeep_map::{MapLoadProgress, MapLoadReport, load_shadowkeep_map_into_world},
};

type MapLoadTask = Task<anyhow::Result<(hecs::World, MapLoadReport)>>;

#[derive(Default)]
struct MapMetadata {
    containers: usize,
    tables: usize,
    entries: usize,
    unreadable_tables: usize,
}

/// An interactive Shadowkeep map. Creation is intentionally cheap: command
/// line and catalog opens may occur while renderer startup is still pending.
pub struct MapTab {
    pub tag: TagHash,
    pub name: String,
    load_task: Option<MapLoadTask>,
    progress: MapLoadProgress,
    report: Option<MapLoadReport>,
    scene: Option<Box<Scene>>,
    error: Option<String>,
    metadata: Result<MapMetadata, String>,
    shared: Arc<SharedState>,
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
            metadata: read_metadata(tag).map_err(|error| format!("{error:#}")),
            shared: shared.clone(),
        })
    }

    fn begin_load_if_ready(&mut self) -> anyhow::Result<()> {
        if self.load_task.is_some() || self.scene.is_some() || self.error.is_some() {
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
        let mut scene = Scene::new(
            Renderer::instance().clone(),
            Camera::default(),
            &self.shared,
            format!("shadowkeep_map_{}", self.tag),
        )?
        .with_controller(CameraController::new_first_person());
        // Legacy collection bounds are reconstructed from per-instance
        // transforms. Keep their conservative first-view bounds from hiding
        // the map while the camera is being auto-framed.
        scene.view.disable_culling = true;
        if let Some(spawn) = report.spawn_points.first().copied() {
            scene.camera.position = spawn + Vec3::Z * 2.0;
        } else if let Some(bounds) = report.world_bounds.or(report.placement_bounds) {
            let radius = bounds.radius().max(4.0);
            scene.camera.position = bounds.center() + Vec3::new(-radius, -radius, radius * 0.5);
            let direction = (bounds.center() - scene.camera.position).normalize();
            let yaw = direction.y.atan2(direction.x);
            let pitch = (-direction.z).atan2(direction.x.hypot(direction.y));
            scene.camera.rotation = Quat::from_rotation_z(yaw) * Quat::from_rotation_y(pitch);
            let yaw_pitch = Vec2::new(yaw.to_degrees(), pitch.to_degrees());
            scene.controller.set_yaw_pitch(yaw_pitch);
        }
        scene.set_world(world);
        self.report = Some(report);
        self.scene = Some(Box::new(scene));
        Ok(())
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, egui_d3d11: &mut egui_d3d11::D3D11Renderer) {
        if let Err(error) = self
            .begin_load_if_ready()
            .and_then(|_| self.complete_load())
        {
            self.error = Some(format!("{error:#}"));
        }
        if let Some(scene) = self.scene.as_mut() {
            if let Some(report) = &self.report {
                ui.collapsing("Shadowkeep map load report", |ui| {
                    ui.label(
                        RichText::new(
                            "Presentation: Shadowkeep deferred lighting → cubemap IBL → package atmosphere → authored SkyTransparent objects → direct sRGB output",
                        )
                        .color(Color32::from_rgb(160, 208, 160)),
                    );
                    ui.weak(
                        "Global directional lighting and sun shadows are enabled by default; base-bubble sky collections take precedence over incompatible freeroam scenario variants.",
                    );
                    ui.label(format!(
                        "{} map/activity tables ({} scenario), {} entries in {:.2?}; {} static, {} terrain, {} rigid, {} cubemap, {} sky render objects",
                        report.tables, report.activity_tables, report.entries, report.elapsed,
                        report.static_placements, report.terrain_placements,
                        report.rigid_render_objects, report.cubemap_render_objects,
                        report.sky_object_render_objects,
                    ));
                    if report.cancelled {
                        ui.label(RichText::new("Load cancelled at a safe resource boundary").color(Color32::YELLOW));
                    }
                    ui.label(format!(
                        "Loaded: {} static, {} terrain, {} rigid, {} cubemap, {} sky; {} duplicate references reused, {} resources deferred",
                        report.static_render_objects, report.terrain_render_objects,
                        report.rigid_render_objects, report.cubemap_render_objects,
                        report.sky_object_render_objects, report.deduplicated_resources,
                        report.deferred_resources,
                    ));
                    ui.label(format!(
                        "Sky objects: {} placements, {} distinct collections, {} records, {} kind-5 skipped; selected {:?}, deferred {:?}",
                        report.sky_object_placements,
                        report.sky_object_collections,
                        report.sky_object_records,
                        report.sky_object_skipped_kind_5,
                        report.sky_object_collection_tags,
                        report.deferred_sky_object_collections,
                    ));
                    ui.label(format!(
                        "Entity census: {} entries, {} without table resources, {} decode failures",
                        report.entity_entries, report.entries_without_table_resource,
                        report.entity_read_failures,
                    ));
                    if !report.resource_class_counts.is_empty() {
                        ui.collapsing("Resource class census", |ui| {
                            for (resource_class, count) in &report.resource_class_counts {
                                let deferred = report
                                    .deferred_resource_classes
                                    .get(resource_class)
                                    .copied()
                                    .unwrap_or_default();
                                ui.monospace(format!(
                                    "{resource_class:08X}: {count} total, {deferred} deferred"
                                ));
                            }
                        });
                    }
                    if !report.deferred_entity_resource_classes.is_empty() {
                        let mut deferred_classes = report
                            .deferred_entity_resource_classes
                            .iter()
                            .collect::<Vec<_>>();
                        deferred_classes.sort_unstable_by(
                            |(left_class, left_count), (right_class, right_count)| {
                                right_count
                                    .cmp(left_count)
                                    .then_with(|| left_class.cmp(right_class))
                            },
                        );
                        ui.collapsing("Top deferred entity classes", |ui| {
                            for (resource_class, deferred) in deferred_classes.into_iter().take(8) {
                                let total = report
                                    .entity_resource_class_counts
                                    .get(resource_class)
                                    .copied()
                                    .unwrap_or_default();
                                let valid = report
                                    .valid_entity_resource_definitions
                                    .get(resource_class)
                                    .copied()
                                    .unwrap_or_default();
                                let invalid = report
                                    .invalid_entity_resource_definitions
                                    .get(resource_class)
                                    .copied()
                                    .unwrap_or_default();
                                let loaded = report
                                    .loaded_entity_resource_classes
                                    .get(resource_class)
                                    .copied()
                                    .unwrap_or_default();
                                let failed = report
                                    .failed_entity_resource_classes
                                    .get(resource_class)
                                    .copied()
                                    .unwrap_or_default();
                                ui.monospace(format!(
                                    "{resource_class:08X}: {total} total, {valid} valid definitions, \
                                     {invalid} invalid definitions, {loaded} loaded, {deferred} deferred, \
                                     {failed} failed"
                                ));
                                if let Some(examples) =
                                    report.entity_resource_examples.get(resource_class)
                                {
                                    for example in examples {
                                        ui.monospace(format!(
                                            "  entity {} resource {} @ {:#X}, table {}, translation {:?}",
                                            example.entity, example.resource,
                                            example.definition_offset, example.table,
                                            example.translation,
                                        ));
                                    }
                                }
                            }
                        });
                    }
                    if let Some(bounds) = report.world_bounds {
                        ui.monospace(format!(
                            "visual bounds: min {:?}, max {:?}, radius {:.1}",
                            bounds.min.truncate(), bounds.max.truncate(), bounds.radius(),
                        ));
                    }
                    if report.is_degraded() {
                        ui.label(RichText::new(format!(
                            "Degraded: {} skipped resources; {} diagnostics",
                            report.skipped_resources, report.diagnostics.len(),
                        )).color(Color32::from_rgb(224, 160, 96)));
                        for diagnostic in report.diagnostics.iter().take(8) {
                            ui.monospace(format!(
                                "{} @ {:#X} ({:08X}): {}",
                                diagnostic.table, diagnostic.entry_offset,
                                diagnostic.resource_class, diagnostic.error,
                            ));
                        }
                    }
                });
            }
            let asset_summary = Renderer::instance().asset_manager.diagnostic_summary();
            ui.collapsing("Shadowkeep asset diagnostics", |ui| {
                ui.label(format!(
                    "{} ready, {} queued, {} loading, {} failed, {} fallbacks",
                    asset_summary.ready,
                    asset_summary.queued,
                    asset_summary.loading,
                    asset_summary.failed,
                    asset_summary.fallback,
                ));
                for diagnostic in Renderer::instance()
                    .asset_manager
                    .diagnostics()
                    .into_iter()
                    .filter(|diagnostic| {
                        matches!(
                            diagnostic.state,
                            alkahest_render::asset::manager::AssetLoadState::Failed { .. }
                                | alkahest_render::asset::manager::AssetLoadState::Fallback { .. }
                        )
                    })
                    .take(16)
                {
                    ui.monospace(format!(
                        "{:?} {}: {:?}",
                        diagnostic.kind, diagnostic.tag, diagnostic.state
                    ));
                }
            });
            ui.collapsing("Shadowkeep pass status", |ui| {
                for pass in pass_status_ledger(&Renderer::instance().globals.pipelines) {
                    let color = match pass.state {
                        ShadowkeepPassState::Ready => Color32::from_rgb(128, 220, 148),
                        ShadowkeepPassState::Degraded => Color32::from_rgb(232, 192, 96),
                        ShadowkeepPassState::DisabledAsAbsent => Color32::from_rgb(160, 160, 160),
                        ShadowkeepPassState::Failed => Color32::from_rgb(232, 112, 112),
                    };
                    ui.colored_label(color, format!("{:?}: {}", pass.state, pass.name));
                    ui.small(pass.evidence);
                }
            });
            scene.show(ui, ui.available_size(), egui_d3d11);
            return;
        }

        ui.heading(&self.name);
        ui.label(format!("Shadowkeep bubble: {}", self.tag));
        ui.add_space(12.0);
        if let Some(error) = &self.error {
            ui.label(RichText::new("Map load failed").color(Color32::DARK_RED));
            ui.label(error);
            if ui.button("Retry").clicked() {
                self.progress.cancel();
                self.progress = MapLoadProgress::default();
                self.report = None;
                self.scene = None;
                self.error = None;
            }
            return;
        }
        let renderer_status = self.shared.renderer_status.read().clone();
        if matches!(renderer_status, crate::app::RendererStatus::Disabled) {
            ui.label(
                RichText::new("Metadata-only map view").color(Color32::from_rgb(144, 192, 224)),
            );
            ui.label(renderer_status.scene_diagnostic());
            match &self.metadata {
                Ok(metadata) => {
                    ui.label(format!(
                        "{} containers, {} tables, {} entries",
                        metadata.containers, metadata.tables, metadata.entries
                    ));
                    ui.label(format!(
                        "{} unreadable table references",
                        metadata.unreadable_tables
                    ));
                }
                Err(error) => {
                    ui.label(RichText::new(error).color(Color32::DARK_RED));
                }
            }
            return;
        }
        if !renderer_status.is_ready() {
            ui.label(
                RichText::new("Waiting for 3D renderer").color(Color32::from_rgb(224, 192, 96)),
            );
            ui.label(renderer_status.scene_diagnostic());
            return;
        }
        let progress = self.progress.snapshot();
        let (_, rect) = ui.allocate_space(ui.available_size());
        ui.painter()
            .rect_filled(rect, 0, Color32::from_rgb(14, 24, 28));
        ui.d_paint_spinner_at(Rect::from_center_size(rect.center(), vec2(64.0, 64.0)));
        ui.painter().text(
            rect.center() + vec2(0.0, 42.0), egui::Align2::CENTER_TOP,
            format!("Loading map: {}/{} tables, {} entries, {} visual resources, {} GPU assets, {} diagnostics{}",
                progress.tables_seen, progress.total_tables, progress.entries_seen,
                progress.visual_resources_loaded, progress.gpu_assets_requested, progress.diagnostics,
                if progress.cancelled { " (cancelling)" } else { "" }),
            egui::FontId::proportional(18.0), Color32::GRAY,
        );
    }
}

impl Drop for MapTab {
    fn drop(&mut self) {
        self.progress.cancel();
    }
}

fn read_metadata(tag: TagHash) -> anyhow::Result<MapMetadata> {
    let parent: SShadowkeepBubbleParent = package_manager().read_tag_struct(tag)?;
    let definition: SShadowkeepBubbleDefinition =
        package_manager().read_tag_struct(parent.child_map)?;
    let mut metadata = MapMetadata::default();
    for container in &definition.map_resources {
        metadata.containers += 1;
        for &table_tag in &container.data_tables {
            match package_manager().read_tag_struct::<SShadowkeepMapDataTable>(table_tag) {
                Ok(table) => {
                    metadata.tables += 1;
                    metadata.entries += table.data_entries.len();
                }
                Err(_) => metadata.unreadable_tables += 1,
            }
        }
    }
    Ok(metadata)
}

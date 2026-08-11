use std::sync::Arc;

use egui::{Color32, RichText, ScrollArea, Ui, vec2};
use tiger_pkg::TagHash;

use crate::{
    app::SharedState,
    ui::scene::Scene,
    world::{
        shadowkeep_inspection::{
            MapEntityVisibility, MapInspectionFilter, MapInspectionNodeId, MapInspectionNodeKind,
            ShadowkeepMapInspection, set_node_visibility,
        },
        shadowkeep_map::{
            MapLoadReport, shadowkeep_bubble_catalog, shadowkeep_bubble_catalog_matches,
        },
    },
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum MapOutlinerMode {
    #[default]
    World,
    Source,
    Spawns,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum MapHiddenFilter {
    #[default]
    All,
    HiddenOnly,
    VisibleOnly,
}

#[derive(Default)]
pub struct BubbleBrowserState {
    pub search: String,
    pub selected: Option<TagHash>,
}

#[derive(Default)]
struct MapWorkspaceViewCache {
    query: String,
    rows: Vec<MapInspectionNodeId>,
}

pub struct MapWorkspaceState {
    pub selected: Option<MapInspectionNodeId>,
    pub hovered: Option<MapInspectionNodeId>,
    pub outliner_mode: MapOutlinerMode,
    pub search: String,
    pub filter: MapInspectionFilter,
    pub hidden_filter: MapHiddenFilter,
    pub show_outliner: bool,
    pub show_inspector: bool,
    pub show_diagnostics: bool,
    pub show_spawn_markers: bool,
    pub show_selection_bounds: bool,
    pub show_metadata_origins: bool,
    pub left_width: f32,
    pub right_width: f32,
    pub bubble_browser: BubbleBrowserState,
    show_bubbles: bool,
    view_cache: MapWorkspaceViewCache,
}

impl Default for MapWorkspaceState {
    fn default() -> Self {
        Self {
            selected: None,
            hovered: None,
            outliner_mode: MapOutlinerMode::World,
            search: String::new(),
            filter: MapInspectionFilter::default(),
            hidden_filter: MapHiddenFilter::All,
            show_outliner: true,
            show_inspector: true,
            show_diagnostics: false,
            show_spawn_markers: true,
            show_selection_bounds: true,
            show_metadata_origins: false,
            left_width: 320.0,
            right_width: 340.0,
            bubble_browser: BubbleBrowserState::default(),
            show_bubbles: false,
            view_cache: MapWorkspaceViewCache::default(),
        }
    }
}

impl MapWorkspaceState {
    pub fn select_node(&mut self, id: MapInspectionNodeId) {
        self.selected = Some(id);
    }

    fn rebuild_rows(&mut self, inspection: &ShadowkeepMapInspection, scene: &Scene) {
        self.view_cache.query = self.search.clone();
        self.view_cache.rows = inspection.search(&self.search, self.filter);
        self.view_cache.rows.retain(|id| {
            let Some(node) = inspection.node(*id) else {
                return false;
            };
            match self.hidden_filter {
                MapHiddenFilter::All => true,
                MapHiddenFilter::HiddenOnly => node.world_entity.is_some_and(|entity| {
                    scene
                        .world
                        .get::<&MapEntityVisibility>(entity)
                        .is_ok_and(|visibility| !visibility.visible)
                }),
                MapHiddenFilter::VisibleOnly => !node.world_entity.is_some_and(|entity| {
                    scene
                        .world
                        .get::<&MapEntityVisibility>(entity)
                        .is_ok_and(|visibility| !visibility.visible)
                }),
            }
        });
    }
}

pub enum MapWorkspaceAction {
    None,
    OpenBubble(TagHash),
    OpenTag(TagHash),
}

pub fn show(
    ui: &mut Ui,
    scene: &mut Scene,
    report: &MapLoadReport,
    shared: &Arc<SharedState>,
    state: &mut MapWorkspaceState,
    egui_d3d11: &mut egui_d3d11::D3D11Renderer,
) -> MapWorkspaceAction {
    let inspection = &report.inspection;
    let mut focus = None;
    let mut marker_select = None;
    let mut workspace_action = MapWorkspaceAction::None;

    ui.horizontal(|ui| {
        ui.label(RichText::new(format!("Bubble {}", inspection.bubble)).strong());
        ui.separator();
        if ui.button("Bubbles").clicked() {
            state.show_bubbles = !state.show_bubbles;
        }
        ui.toggle_value(&mut state.show_outliner, "Outliner");
        ui.toggle_value(&mut state.show_inspector, "Inspector");
        ui.toggle_value(&mut state.show_spawn_markers, "Spawns");
        ui.toggle_value(&mut state.show_diagnostics, "Diagnostics");
        if ui
            .add_enabled(
                state.selected.is_some(),
                egui::Button::new("Focus Selected"),
            )
            .clicked()
        {
            focus = state.selected;
        }
        if let Some(selected) = state.selected {
            let visible = inspection
                .node(selected)
                .and_then(|node| node.world_entity)
                .is_some_and(|entity| {
                    scene
                        .world
                        .get::<&MapEntityVisibility>(entity)
                        .is_ok_and(|visibility| visibility.visible)
                });
            if ui
                .button(if visible {
                    "Hide Selected"
                } else {
                    "Show Selected"
                })
                .clicked()
            {
                set_node_visibility(&mut scene.world, inspection, selected, !visible, false);
            }
            if let Some(tag) = inspection.node(selected).and_then(|node| node.tag) {
                if ui.button("Open Tag Inspector").clicked() {
                    workspace_action = MapWorkspaceAction::OpenTag(tag);
                }
            }
        }
        if ui.button("Show All").clicked() {
            for id in inspection
                .nodes
                .iter()
                .filter(|node| node.kind.is_visual_owner())
                .map(|node| node.id)
            {
                set_node_visibility(&mut scene.world, inspection, id, true, false);
            }
        }
    });
    ui.separator();

    state.rebuild_rows(inspection, scene);
    let available = ui.available_size();
    let narrow = available.x < 720.0;
    let left = state
        .show_outliner
        .then_some(state.left_width.clamp(240.0, 520.0));
    let right = (!narrow && state.show_inspector).then_some(state.right_width.clamp(280.0, 520.0));
    let center_width =
        (available.x - left.unwrap_or_default() - right.unwrap_or_default()).max(480.0);

    ui.horizontal_top(|ui| {
        if let Some(width) = left {
            ui.allocate_ui_with_layout(
                vec2(width, available.y),
                egui::Layout::top_down(egui::Align::Min),
                |ui| {
                    outliner(ui, inspection, scene, state, &mut focus);
                },
            );
            ui.separator();
        }
        ui.allocate_ui_with_layout(
            vec2(center_width, available.y),
            egui::Layout::top_down(egui::Align::Min),
            |ui| {
                marker_select = scene
                    .show_with_overlay(
                        ui,
                        ui.available_size(),
                        egui_d3d11,
                        |ui, rect, camera, _response| {
                            let world_to_clip =
                                camera.projection_matrix_standard() * camera.view_matrix();
                            let mut action = None;
                            if state.show_spawn_markers {
                                for id in &inspection.spawn_nodes {
                                    let Some(node) = inspection.node(*id) else {
                                        continue;
                                    };
                                    let Some(transform) = node.transform else {
                                        continue;
                                    };
                                    let clip = world_to_clip * transform.translation.extend(1.0);
                                    if clip.w <= 0.0 {
                                        continue;
                                    }
                                    let ndc = clip.truncate() / clip.w;
                                    if !ndc.is_finite() || ndc.x.abs() > 1.1 || ndc.y.abs() > 1.1 {
                                        continue;
                                    }
                                    let point = rect.left_top()
                                        + vec2(
                                            (ndc.x + 1.0) * 0.5 * rect.width(),
                                            (1.0 - ndc.y) * 0.5 * rect.height(),
                                        );
                                    let selected = state.selected == Some(*id);
                                    let radius = if selected { 7.0 } else { 4.0 };
                                    let color = if selected {
                                        Color32::from_rgb(64, 200, 255)
                                    } else {
                                        Color32::from_rgb(255, 210, 80)
                                    };
                                    ui.painter().circle_filled(point, radius, color);
                                    let response = ui.interact(
                                        egui::Rect::from_center_size(
                                            point,
                                            vec2(radius * 3.0, radius * 3.0),
                                        ),
                                        ui.make_persistent_id(("map_spawn_marker", id.0)),
                                        egui::Sense::click(),
                                    );
                                    let clicked = response.clicked();
                                    let double_clicked = response.double_clicked();
                                    response.on_hover_text(format!(
                                        "{} · 0x{:08X} · {:?}",
                                        node.label,
                                        node.name_hash.unwrap_or_default(),
                                        transform.translation,
                                    ));
                                    if clicked {
                                        action = Some(*id);
                                    }
                                    if double_clicked {
                                        action = Some(*id);
                                        focus = Some(*id);
                                    }
                                }
                            }
                            action
                        },
                    )
                    .flatten();
            },
        );
        if let Some(width) = right {
            ui.separator();
            ui.allocate_ui_with_layout(
                vec2(width, available.y),
                egui::Layout::top_down(egui::Align::Min),
                |ui| {
                    inspector(ui, inspection, scene, state, shared, &mut focus);
                },
            );
        }
    });
    if let Some(id) = marker_select {
        state.select_node(id);
    }
    if state.show_bubbles {
        egui::Window::new("Bubbles")
            .default_width(520.0)
            .show(ui.ctx(), |ui| {
                ui.text_edit_singleline(&mut state.bubble_browser.search);
                let query = state.bubble_browser.search.trim().to_ascii_lowercase();
                ScrollArea::vertical().show(ui, |ui| {
                    for entry in shadowkeep_bubble_catalog()
                        .entries
                        .iter()
                        .filter(|entry| shadowkeep_bubble_catalog_matches(entry, &query))
                    {
                        let response = ui.selectable_label(
                            entry.tag == inspection.bubble,
                            format!(
                                "{} · {} · {} tables",
                                entry.display_name, entry.tag, entry.table_count
                            ),
                        );
                        if response.double_clicked() {
                            workspace_action = MapWorkspaceAction::OpenBubble(entry.tag);
                        }
                    }
                });
            });
    }

    if narrow && state.show_inspector {
        egui::Window::new("Inspector")
            .default_width(state.right_width)
            .show(ui.ctx(), |ui| {
                inspector(ui, inspection, scene, state, shared, &mut focus);
            });
    }
    if state.show_diagnostics {
        egui::Window::new("Diagnostics")
            .default_width(420.0)
            .show(ui.ctx(), |ui| {
                ui.label(format!(
                    "{} tables · {} entries · {} nodes",
                    report.tables,
                    report.entries,
                    inspection.nodes.len()
                ));
                ui.label(format!(
                    "{} deferred · {} skipped",
                    report.deferred_resources, report.skipped_resources
                ));
                for diagnostic in report.diagnostics.iter().take(24) {
                    ui.monospace(format!(
                        "{} @ {:#X}: {}",
                        diagnostic.table, diagnostic.entry_offset, diagnostic.error
                    ));
                }
            });
    }
    if let Some(id) = focus {
        if let Some(node) = inspection.node(id) {
            scene.focus_inspection_node(node);
        }
    }
    workspace_action
}
fn outliner(
    ui: &mut Ui,
    inspection: &ShadowkeepMapInspection,
    scene: &mut Scene,
    state: &mut MapWorkspaceState,
    focus: &mut Option<MapInspectionNodeId>,
) {
    ui.horizontal(|ui| {
        ui.selectable_value(&mut state.outliner_mode, MapOutlinerMode::World, "World");
        ui.selectable_value(&mut state.outliner_mode, MapOutlinerMode::Source, "Source");
        ui.selectable_value(&mut state.outliner_mode, MapOutlinerMode::Spawns, "Spawns");
    });
    egui::ComboBox::from_label("Visibility")
        .selected_text(match state.hidden_filter {
            MapHiddenFilter::All => "All",
            MapHiddenFilter::HiddenOnly => "Hidden",
            MapHiddenFilter::VisibleOnly => "Visible",
        })
        .show_ui(ui, |ui| {
            ui.selectable_value(&mut state.hidden_filter, MapHiddenFilter::All, "All");
            ui.selectable_value(
                &mut state.hidden_filter,
                MapHiddenFilter::HiddenOnly,
                "Hidden",
            );
            ui.selectable_value(
                &mut state.hidden_filter,
                MapHiddenFilter::VisibleOnly,
                "Visible",
            );
        });
    if ui.text_edit_singleline(&mut state.search).changed() {
        state.view_cache.query.clear();
    }
    ScrollArea::vertical()
        .id_salt("map_workspace_outliner")
        .show(ui, |ui| {
            let rows: Vec<_> = match state.outliner_mode {
                MapOutlinerMode::Spawns => inspection.spawn_nodes.clone(),
                MapOutlinerMode::World => state.view_cache.rows.clone(),
                MapOutlinerMode::Source => inspection
                    .nodes
                    .iter()
                    .filter(|node| {
                        matches!(
                            node.kind,
                            MapInspectionNodeKind::BaseContainer
                                | MapInspectionNodeKind::Scenario
                                | MapInspectionNodeKind::Table
                                | MapInspectionNodeKind::Entry
                        )
                    })
                    .map(|node| node.id)
                    .collect(),
            };
            if rows.is_empty() && state.outliner_mode == MapOutlinerMode::Spawns {
                ui.weak("No authored spawn points were discovered in this bubble.");
            }
            for id in rows {
                let Some(node) = inspection.node(id) else {
                    continue;
                };
                let selected = state.selected == Some(id);
                let response = ui.selectable_label(
                    selected,
                    format!("{}  {}", node.kind.icon_name(), node.label),
                );
                if response.clicked() {
                    state.select_node(id);
                }
                if response.double_clicked() {
                    *focus = Some(id);
                }
                if response.hovered() {
                    state.hovered = Some(id);
                }
                if node.kind.is_visual_owner() {
                    let visible = node.world_entity.is_some_and(|entity| {
                        scene
                            .world
                            .get::<&MapEntityVisibility>(entity)
                            .is_ok_and(|v| v.visible)
                    });
                    if ui
                        .small_button(if visible { "Hide" } else { "Show" })
                        .clicked()
                    {
                        set_node_visibility(&mut scene.world, inspection, id, !visible, false);
                    }
                }
            }
        });
}

fn inspector(
    ui: &mut Ui,
    inspection: &ShadowkeepMapInspection,
    scene: &mut Scene,
    state: &mut MapWorkspaceState,
    shared: &SharedState,
    focus: &mut Option<MapInspectionNodeId>,
) {
    let Some(id) = state.selected else {
        ui.weak("No entity selected. Select an entity in the outliner or a spawn marker.");
        return;
    };
    let Some(node) = inspection.node(id) else {
        return;
    };
    ui.heading(&node.label);
    ui.horizontal(|ui| {
        if ui.button("Focus").clicked() {
            *focus = Some(id);
        }
        if ui.button("Copy ID").clicked() {
            ui.ctx().copy_text(format!("{}", id.0));
        }
    });
    ui.separator();
    ui.label(format!(
        "Node {} · {:?} · {}",
        id.0,
        node.kind,
        node.disposition.status_label()
    ));
    if let Some(tag) = node.tag {
        ui.monospace(format!("Tag {tag}"));
    }
    if let Some(class) = node.class {
        ui.monospace(format!("Class 0x{class:08X}"));
    }
    if let Some(world_id) = node.world_id {
        ui.monospace(format!("World ID {world_id}"));
    }
    if let Some(offset) = node.definition_offset {
        ui.monospace(format!("Offset 0x{offset:X}"));
    }
    if let Some(transform) = node.transform {
        ui.monospace(format!("Position {:?}", transform.translation));
    }
    if let Some(hash) = node.name_hash {
        let label = shared.wordlist.get(&hash).map_or_else(
            || format!("Unresolved Spawn 0x{hash:08X}"),
            |name| format!("{name} (0x{hash:08X})"),
        );
        ui.label(label);
    }
    if let Some(entity) = node.world_entity {
        match scene.world.get::<&MapEntityVisibility>(entity) {
            Ok(visibility) => ui.label(format!(
                "Visibility: {}",
                if visibility.visible {
                    "visible"
                } else {
                    "hidden"
                }
            )),
            Err(_) => ui.weak("Entity unavailable"),
        };
    }
    if let Some(error) = &node.error {
        ui.colored_label(Color32::LIGHT_RED, error);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workspace_starts_in_the_visible_world_mode() {
        let state = MapWorkspaceState::default();

        assert_eq!(state.outliner_mode, MapOutlinerMode::World);
        assert!(state.show_outliner);
        assert!(state.show_inspector);
        assert!(state.show_spawn_markers);
        assert!(!state.show_diagnostics);
        assert_eq!(state.hidden_filter, MapHiddenFilter::All);
    }

    #[test]
    fn selection_replaces_the_prior_node() {
        let mut state = MapWorkspaceState::default();
        state.select_node(MapInspectionNodeId(2));
        state.select_node(MapInspectionNodeId(7));

        assert_eq!(state.selected, Some(MapInspectionNodeId(7)));
    }
}

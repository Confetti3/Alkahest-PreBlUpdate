use std::{collections::BTreeSet, sync::Arc};

use egui::{Color32, RichText, ScrollArea, Ui, vec2};
use tiger_pkg::TagHash;

use crate::{
    app::SharedState,
    ui::{
        bubble_browser::{BubbleBrowserState, show as show_bubble_browser},
        scene::Scene,
    },
    world::{
        shadowkeep_inspection::{
            MapEntityVisibility, MapInspectionDispositionFilter, MapInspectionFilter,
            MapInspectionNodeId, MapInspectionNodeKind, MapInspectionSourceFilter,
            MapInspectionTypeFilter, ShadowkeepMapInspection, set_node_visibility,
        },
        shadowkeep_map::MapLoadReport,
    },
};

mod viewport_overlay;

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
struct MapWorkspaceViewCache {
    query: String,
    filter: MapInspectionFilter,
    hidden_filter: MapHiddenFilter,
    valid: bool,
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

    fn invalidate_rows(&mut self) {
        self.view_cache.valid = false;
    }

    fn rebuild_rows(&mut self, inspection: &ShadowkeepMapInspection, scene: &Scene) {
        if self.view_cache.valid
            && self.view_cache.query == self.search
            && self.view_cache.filter == self.filter
            && self.view_cache.hidden_filter == self.hidden_filter
        {
            return;
        }
        self.view_cache.query.clone_from(&self.search);
        self.view_cache.filter = self.filter;
        self.view_cache.hidden_filter = self.hidden_filter;
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
        self.view_cache.valid = true;
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
                state.invalidate_rows();
            }
            if ui.button("Hide Others").clicked() {
                let visible_subtree = std::iter::once(selected)
                    .chain(inspection.descendants(selected))
                    .collect::<BTreeSet<_>>();
                for id in inspection
                    .nodes
                    .iter()
                    .filter(|node| node.kind.is_visual_owner())
                    .map(|node| node.id)
                {
                    if !visible_subtree.contains(&id) {
                        set_node_visibility(&mut scene.world, inspection, id, false, false);
                    }
                }
                state.invalidate_rows();
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
                state.invalidate_rows();
            }
        }
    });

    if !ui.ctx().egui_wants_keyboard_input() {
        let focus_selected = ui.input(|input| input.key_pressed(egui::Key::F));
        let toggle_selected = ui.input(|input| input.key_pressed(egui::Key::H));
        let clear_selected = ui.input(|input| input.key_pressed(egui::Key::Escape));
        let copy_selected =
            ui.input(|input| input.modifiers.ctrl && input.key_pressed(egui::Key::C));
        if focus_selected {
            focus = state.selected;
        }
        if toggle_selected && let Some(selected) = state.selected {
            let visible = inspection
                .node(selected)
                .and_then(|node| node.world_entity)
                .and_then(|entity| {
                    scene
                        .world
                        .get::<&MapEntityVisibility>(entity)
                        .ok()
                        .map(|visibility| visibility.visible)
                });
            if let Some(visible) = visible {
                set_node_visibility(&mut scene.world, inspection, selected, !visible, false);
                state.invalidate_rows();
            }
        }
        if clear_selected {
            state.selected = None;
        }
        if copy_selected
            && let Some(selected) = state.selected
            && let Some(node) = inspection.node(selected)
        {
            let primary = node
                .tag
                .map(|tag| tag.to_string())
                .or_else(|| node.class.map(|class| format!("0x{class:08X}")))
                .unwrap_or_else(|| selected.0.to_string());
            ui.ctx().copy_text(primary);
        }
    }
    ui.separator();

    state.rebuild_rows(inspection, scene);
    let mut available = ui.available_size();
    available.y = (available.y - 28.0).max(0.0);
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
                let selected_hidden = state
                    .selected
                    .and_then(|id| inspection.node(id))
                    .and_then(|node| node.world_entity)
                    .is_some_and(|entity| {
                        scene
                            .world
                            .get::<&MapEntityVisibility>(entity)
                            .is_ok_and(|visibility| !visibility.visible)
                    });
                marker_select = scene
                    .show_with_overlay(
                        ui,
                        ui.available_size(),
                        egui_d3d11,
                        |ui, rect, camera, response| {
                            viewport_overlay::show(
                                ui,
                                rect,
                                camera,
                                response,
                                inspection,
                                state.selected,
                                selected_hidden,
                                state.show_selection_bounds,
                                state.show_spawn_markers,
                                &shared.wordlist,
                            )
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
    let ready = inspection
        .nodes
        .iter()
        .filter(|node| {
            node.disposition
                == crate::world::shadowkeep_inspection::MapInspectionDisposition::Rendering
        })
        .count();
    let failed = inspection
        .nodes
        .iter()
        .filter(|node| {
            node.disposition
                == crate::world::shadowkeep_inspection::MapInspectionDisposition::Failed
        })
        .count();
    let metadata = inspection
        .nodes
        .iter()
        .filter(|node| node.kind.type_group() == MapInspectionTypeFilter::METADATA)
        .count();
    ui.horizontal(|ui| {
        ui.weak(format!(
            "{ready} ready · {failed} failed · {} visual · {metadata} metadata · {} spawns · Ready",
            inspection
                .nodes
                .iter()
                .filter(|node| node.kind.is_visual_owner())
                .count(),
            inspection.spawn_nodes.len(),
        ));
    });
    if let Some(action) = marker_select {
        match action {
            viewport_overlay::ViewportAction::Select(id) => state.select_node(id),
            viewport_overlay::ViewportAction::Focus(id) => {
                state.select_node(id);
                focus = Some(id);
            }
        }
    }
    if state.show_bubbles {
        egui::Window::new("Bubbles")
            .default_width(520.0)
            .show(ui.ctx(), |ui| {
                if let Some(tag) =
                    show_bubble_browser(ui, &mut state.bubble_browser, Some(inspection.bubble))
                {
                    workspace_action = MapWorkspaceAction::OpenBubble(tag);
                }
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
    let mut filters_changed = false;
    ui.menu_button("Filters", |ui| {
        ui.label("Type");
        for (label, flag) in [
            ("Geometry", MapInspectionTypeFilter::GEOMETRY),
            ("Lights", MapInspectionTypeFilter::LIGHTS),
            ("Environment", MapInspectionTypeFilter::ENVIRONMENT),
            ("Spawns", MapInspectionTypeFilter::SPAWNS),
            ("Deferred", MapInspectionTypeFilter::DEFERRED),
            ("Metadata", MapInspectionTypeFilter::METADATA),
        ] {
            let mut enabled = state.filter.types.contains(flag);
            if ui.checkbox(&mut enabled, label).changed() {
                state.filter.types.set(flag, enabled);
                filters_changed = true;
            }
        }
        ui.separator();
        ui.label("Status");
        for (label, flag) in [
            ("Rendering", MapInspectionDispositionFilter::RENDERING),
            (
                "Non-rendering",
                MapInspectionDispositionFilter::NON_RENDERING,
            ),
            ("Deferred", MapInspectionDispositionFilter::DEFERRED),
            ("Failed", MapInspectionDispositionFilter::FAILED),
        ] {
            let mut enabled = state.filter.dispositions.contains(flag);
            if ui.checkbox(&mut enabled, label).changed() {
                state.filter.dispositions.set(flag, enabled);
                filters_changed = true;
            }
        }
        ui.separator();
        ui.label("Source");
        for (label, flag) in [
            ("Base bubble", MapInspectionSourceFilter::BASE),
            ("Freeroam scenario", MapInspectionSourceFilter::SCENARIO),
        ] {
            let mut enabled = state.filter.sources.contains(flag);
            if ui.checkbox(&mut enabled, label).changed() {
                state.filter.sources.set(flag, enabled);
                filters_changed = true;
            }
        }
    });
    if filters_changed {
        state.invalidate_rows();
    }
    if ui.text_edit_singleline(&mut state.search).changed() {
        state.invalidate_rows();
    }
    ScrollArea::vertical()
        .id_salt("map_workspace_outliner")
        .show(ui, |ui| {
            let rows: Vec<_> = match state.outliner_mode {
                MapOutlinerMode::Spawns => inspection
                    .spawn_nodes
                    .iter()
                    .copied()
                    .map(|id| (0, id))
                    .collect(),
                MapOutlinerMode::World => state
                    .view_cache
                    .rows
                    .iter()
                    .copied()
                    .filter(|id| {
                        !state.search.trim().is_empty()
                            || inspection
                                .node(*id)
                                .is_some_and(|node| is_world_node(node.kind))
                    })
                    .map(|id| (0, id))
                    .collect(),
                MapOutlinerMode::Source => source_rows(inspection),
            };
            if rows.is_empty() && state.outliner_mode == MapOutlinerMode::Spawns {
                ui.weak("No authored spawn points were discovered in this bubble.");
            }
            for (depth, id) in rows {
                let Some(node) = inspection.node(id) else {
                    continue;
                };
                let selected = state.selected == Some(id);
                let response = ui
                    .horizontal(|ui| {
                        ui.add_space(depth as f32 * 14.0);
                        let response = ui.selectable_label(
                            selected,
                            format!("{}  {}", node.kind.icon_name(), node.label),
                        );
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
                                set_node_visibility(
                                    &mut scene.world,
                                    inspection,
                                    id,
                                    !visible,
                                    false,
                                );
                                state.invalidate_rows();
                            }
                        }
                        response
                    })
                    .inner;
                if response.clicked() {
                    state.select_node(id);
                }
                if response.double_clicked() {
                    *focus = Some(id);
                }
                if response.hovered() {
                    state.hovered = Some(id);
                }
            }
        });
}

fn source_rows(inspection: &ShadowkeepMapInspection) -> Vec<(usize, MapInspectionNodeId)> {
    let mut rows = vec![(0, inspection.root)];
    for group in inspection.source_groups.values() {
        rows.push((1, group.node));
        for table in &group.tables {
            rows.push((2, *table));
            rows.extend(inspection.descendants(*table).map(|id| (3, id)));
        }
    }
    rows
}

fn is_world_node(kind: MapInspectionNodeKind) -> bool {
    matches!(
        kind,
        MapInspectionNodeKind::StaticGeometry
            | MapInspectionNodeKind::Terrain
            | MapInspectionNodeKind::RigidModel
            | MapInspectionNodeKind::DynamicModel
            | MapInspectionNodeKind::LightCollection
            | MapInspectionNodeKind::Light
            | MapInspectionNodeKind::ShadowingLight
            | MapInspectionNodeKind::Cubemap
            | MapInspectionNodeKind::Atmosphere
            | MapInspectionNodeKind::SkyCollection
            | MapInspectionNodeKind::SkyObject
            | MapInspectionNodeKind::SpawnPoint
            | MapInspectionNodeKind::DeferredResource
            | MapInspectionNodeKind::FailedResource
            | MapInspectionNodeKind::MetadataOnly
    )
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
        if let Some(transform) = node.transform
            && node.kind == MapInspectionNodeKind::SpawnPoint
            && ui.button("Teleport Camera Here").clicked()
        {
            scene.teleport_to_transform(transform);
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
        ui.label("Transform · authored");
        ui.monospace(format!("Position {:?}", transform.translation));
        ui.monospace(format!("Rotation {:?}", transform.rotation));
        ui.monospace(format!("Scale {:?}", transform.scale));
        if ui.button("Copy Position").clicked() {
            ui.ctx().copy_text(format!("{:?}", transform.translation));
        }
    }
    if let Some(hash) = node.name_hash {
        let label = shared.wordlist.get(&hash).map_or_else(
            || format!("Unresolved Spawn 0x{hash:08X}"),
            |name| format!("{name} (0x{hash:08X})"),
        );
        ui.label(label);
        if hash == 0x2EA8_FB98 {
            ui.weak("default");
        }
        if ui.button("Copy FNV1 Hash").clicked() {
            ui.ctx().copy_text(format!("0x{hash:08X}"));
        }
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

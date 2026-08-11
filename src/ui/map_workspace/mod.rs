use std::{collections::BTreeSet, sync::Arc};

use egui::{
    Color32, Pos2, Rect, RichText, ScrollArea, Sense, Shape, Stroke, TextWrapMode, Ui, vec2,
};
use google_material_symbols::GoogleMaterialSymbols;
use tiger_pkg::TagHash;

use crate::{
    app::SharedState,
    ui::{
        bubble_browser::{BubbleBrowserState, show_compact as show_bubble_browser},
        scene::Scene,
    },
    world::{
        shadowkeep_inspection::{
            MapEntityVisibility, MapInspectionDisposition, MapInspectionDispositionFilter,
            MapInspectionFilter, MapInspectionNodeId, MapInspectionNodeKind,
            MapInspectionSourceFilter, MapInspectionTypeFilter, ShadowkeepMapInspection,
            normalize_search, set_node_visibility,
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

fn locator_admitted_by_visibility(filter: MapHiddenFilter, visible: bool) -> bool {
    match filter {
        MapHiddenFilter::All => true,
        MapHiddenFilter::HiddenOnly => !visible,
        MapHiddenFilter::VisibleOnly => visible,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct MapTreeRow {
    depth: usize,
    id: MapInspectionNodeId,
    has_children: bool,
    guide_start: usize,
    guide_len: usize,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct MapWorkspaceSummary {
    ready: usize,
    failed: usize,
    visual: usize,
    metadata: usize,
    spawns: usize,
}

impl MapWorkspaceSummary {
    fn from_inspection(inspection: &ShadowkeepMapInspection) -> Self {
        let mut summary = Self {
            spawns: inspection.spawn_nodes.len(),
            ..Default::default()
        };
        for node in &inspection.nodes {
            match node.disposition {
                MapInspectionDisposition::Rendering => summary.ready += 1,
                MapInspectionDisposition::Failed => summary.failed += 1,
                MapInspectionDisposition::NonRendering | MapInspectionDisposition::Deferred => {}
            }
            summary.visual += usize::from(node.kind.is_visual_owner());
            summary.metadata +=
                usize::from(node.kind.type_group() == MapInspectionTypeFilter::METADATA);
        }
        summary
    }
}

#[derive(Default)]
struct MapWorkspaceViewCache {
    query: String,
    filter: MapInspectionFilter,
    hidden_filter: MapHiddenFilter,
    valid: bool,
    rows: Vec<MapInspectionNodeId>,
    locator_rows: Vec<MapInspectionNodeId>,
    visible_locator_rows: BTreeSet<MapInspectionNodeId>,
    tree_rows: Vec<MapTreeRow>,
    tree_guides: Vec<bool>,
    tree_mode: Option<MapOutlinerMode>,
    tree_valid: bool,
    generation: u64,
    summary: Option<MapWorkspaceSummary>,
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
    pub show_visual_helpers: bool,
    pub show_visual_labels: bool,
    collapsed_nodes: BTreeSet<MapInspectionNodeId>,
    hierarchy_initialized: bool,
    pub left_width: f32,
    pub right_width: f32,
    pub bubble_browser: BubbleBrowserState,
    show_bubbles: bool,
    view_cache: MapWorkspaceViewCache,
    viewport_scratch: viewport_overlay::ViewportScratch,
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
            show_visual_helpers: false,
            show_visual_labels: true,
            collapsed_nodes: BTreeSet::new(),
            hierarchy_initialized: false,
            left_width: 320.0,
            right_width: 340.0,
            bubble_browser: BubbleBrowserState::default(),
            show_bubbles: false,
            view_cache: MapWorkspaceViewCache::default(),
            viewport_scratch: viewport_overlay::ViewportScratch::default(),
        }
    }
}

impl MapWorkspaceState {
    pub fn select_node(&mut self, id: MapInspectionNodeId) {
        self.selected = Some(id);
    }

    fn invalidate_rows(&mut self) {
        self.view_cache.valid = false;
        self.view_cache.tree_valid = false;
    }

    fn rebuild_rows(&mut self, inspection: &ShadowkeepMapInspection, scene: &Scene) {
        let query = normalize_search(&self.search);
        if self.view_cache.valid
            && self.view_cache.query == query
            && self.view_cache.filter == self.filter
            && self.view_cache.hidden_filter == self.hidden_filter
        {
            return;
        }
        self.view_cache.query = query;
        self.view_cache.filter = self.filter;
        self.view_cache.hidden_filter = self.hidden_filter;
        let matching = inspection
            .search(&self.view_cache.query, self.filter)
            .into_iter()
            .collect::<BTreeSet<_>>();
        self.view_cache.rows.clear();
        self.view_cache.rows.extend(matching.iter().copied());
        self.view_cache.rows.retain(|id| {
            let hidden = inspection
                .node(*id)
                .and_then(|node| node.world_entity)
                .is_some_and(|entity| {
                    scene
                        .world
                        .get::<&MapEntityVisibility>(entity)
                        .is_ok_and(|visibility| !visibility.visible)
                });
            match self.hidden_filter {
                MapHiddenFilter::All => true,
                MapHiddenFilter::HiddenOnly => hidden,
                MapHiddenFilter::VisibleOnly => !hidden,
            }
        });
        self.view_cache.locator_rows.clear();
        self.view_cache.visible_locator_rows.clear();
        for &id in inspection.locator_nodes() {
            if !matching.contains(&id) {
                continue;
            }
            let visible = inspection
                .visual_owner(id)
                .and_then(|owner| inspection.node(owner))
                .and_then(|owner| owner.world_entity)
                .is_some_and(|entity| {
                    scene
                        .world
                        .get::<&MapEntityVisibility>(entity)
                        .is_ok_and(|visibility| visibility.visible)
                });
            if visible {
                self.view_cache.visible_locator_rows.insert(id);
            }
            if locator_admitted_by_visibility(self.hidden_filter, visible) {
                self.view_cache.locator_rows.push(id);
            }
        }
        self.view_cache.valid = true;
        self.view_cache.tree_valid = false;
        self.view_cache.generation = self.view_cache.generation.wrapping_add(1);
    }

    fn rebuild_tree(&mut self, inspection: &ShadowkeepMapInspection) {
        if self.view_cache.tree_valid && self.view_cache.tree_mode == Some(self.outliner_mode) {
            return;
        }
        self.view_cache.tree_rows.clear();
        self.view_cache.tree_guides.clear();
        let respect_collapsed = self.view_cache.query.is_empty();
        match self.outliner_mode {
            MapOutlinerMode::Spawns => {
                self.view_cache.tree_rows.extend(
                    self.view_cache
                        .rows
                        .iter()
                        .copied()
                        .filter(|id| inspection.spawn_nodes.contains(id))
                        .map(|id| MapTreeRow {
                            depth: 0,
                            id,
                            has_children: false,
                            guide_start: 0,
                            guide_len: 0,
                        }),
                );
            }
            MapOutlinerMode::World => hierarchy_rows(
                inspection,
                &self.view_cache.rows,
                &self.collapsed_nodes,
                respect_collapsed,
                !self.view_cache.query.is_empty(),
                &mut self.view_cache.tree_rows,
                &mut self.view_cache.tree_guides,
            ),
            MapOutlinerMode::Source => source_rows(
                inspection,
                &self.view_cache.rows,
                &self.collapsed_nodes,
                respect_collapsed,
                &mut self.view_cache.tree_rows,
                &mut self.view_cache.tree_guides,
            ),
        }
        self.view_cache.tree_mode = Some(self.outliner_mode);
        self.view_cache.tree_valid = true;
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
    let summary = *state
        .view_cache
        .summary
        .get_or_insert_with(|| MapWorkspaceSummary::from_inspection(inspection));
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
        egui::containers::menu::MenuButton::new("View")
            .config(
                egui::containers::menu::MenuConfig::new()
                    .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside),
            )
            .ui(ui, |ui| {
                ui.checkbox(&mut state.show_spawn_markers, "Spawn markers");
                ui.checkbox(&mut state.show_visual_helpers, "Visual helpers");
                ui.add_enabled_ui(state.show_visual_helpers, |ui| {
                    ui.checkbox(&mut state.show_visual_labels, "Helper labels");
                });
                ui.checkbox(&mut state.show_selection_bounds, "Selection bounds");
                ui.checkbox(&mut state.show_diagnostics, "Diagnostics");
            });
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
            let visibility_target = inspection.visual_owner(selected).unwrap_or(selected);
            let visible = inspection
                .node(visibility_target)
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
                set_node_visibility(
                    &mut scene.world,
                    inspection,
                    visibility_target,
                    !visible,
                    false,
                );
                state.invalidate_rows();
            }
            if ui.button("Hide Others").clicked() {
                let visible_subtree = std::iter::once(visibility_target)
                    .chain(inspection.descendants(visibility_target))
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
            let visibility_target = inspection.visual_owner(selected).unwrap_or(selected);
            let visible = inspection
                .node(visibility_target)
                .and_then(|node| node.world_entity)
                .and_then(|entity| {
                    scene
                        .world
                        .get::<&MapEntityVisibility>(entity)
                        .ok()
                        .map(|visibility| visibility.visible)
                });
            if let Some(visible) = visible {
                set_node_visibility(
                    &mut scene.world,
                    inspection,
                    visibility_target,
                    !visible,
                    false,
                );
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
    if !state.hierarchy_initialized {
        state.collapsed_nodes.extend(
            inspection
                .nodes
                .iter()
                .filter(|node| {
                    matches!(
                        node.kind,
                        MapInspectionNodeKind::BaseContainer
                            | MapInspectionNodeKind::Scenario
                            | MapInspectionNodeKind::Table
                    )
                })
                .map(|node| node.id),
        );
        state.hierarchy_initialized = true;
    }
    let mut available = ui.available_size();
    available.y = (available.y - 28.0).max(0.0);
    let narrow = available.x < 720.0;

    ui.allocate_ui(available, |ui| {
        if state.show_outliner {
            egui::Panel::left("map_workspace_outliner_panel")
                .resizable(true)
                .default_size(state.left_width)
                .size_range(240.0..=520.0)
                .show(ui, |ui| {
                    state.left_width = ui.available_width();
                    outliner(ui, inspection, scene, state, &mut focus);
                });
        }
        if !narrow && state.show_inspector {
            egui::Panel::right("map_workspace_inspector_panel")
                .resizable(true)
                .default_size(state.right_width)
                .size_range(280.0..=520.0)
                .show(ui, |ui| {
                    state.right_width = ui.available_width();
                    inspector(ui, inspection, scene, state, shared, &mut focus);
                });
        }
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE)
            .show(ui, |ui| {
                let selected = state.selected;
                let selected_hidden = selected
                    .and_then(|id| inspection.visual_owner(id))
                    .and_then(|owner| inspection.node(owner))
                    .and_then(|owner| owner.world_entity)
                    .is_some_and(|entity| {
                        scene
                            .world
                            .get::<&MapEntityVisibility>(entity)
                            .is_ok_and(|visibility| !visibility.visible)
                    });
                let locator_rows = &state.view_cache.locator_rows;
                let visible_locator_rows = &state.view_cache.visible_locator_rows;
                let hovered = &mut state.hovered;
                let viewport_scratch = &mut state.viewport_scratch;
                let show_visual_helpers = state.show_visual_helpers;
                let show_visual_labels = state.show_visual_labels;
                let show_selection_bounds = state.show_selection_bounds;
                let show_spawn_markers = state.show_spawn_markers;
                let workspace_generation = state.view_cache.generation;
                marker_select = scene
                    .show_with_overlay(
                        ui,
                        ui.available_size(),
                        egui_d3d11,
                        |ui, rect, camera, response, depth_sample| {
                            viewport_overlay::show(
                                ui,
                                rect,
                                camera,
                                response,
                                depth_sample,
                                inspection,
                                locator_rows,
                                visible_locator_rows,
                                hovered,
                                selected,
                                selected_hidden,
                                show_selection_bounds,
                                viewport_scratch,
                                show_visual_helpers,
                                show_visual_labels,
                                show_spawn_markers,
                                workspace_generation,
                                &shared.wordlist,
                            )
                        },
                    )
                    .flatten();
            });
    });
    ui.horizontal(|ui| {
        ui.weak(format!(
            "{} ready · {} failed · {} visual · {} metadata · {} spawns · Ready",
            summary.ready, summary.failed, summary.visual, summary.metadata, summary.spawns,
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
                ui.label(format!(
                    "{} activity spawn rules · {} correlated placements",
                    report.activity_spawn_rules, report.activity_correlated_spawns
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
fn map_node_icon(kind: MapInspectionNodeKind) -> GoogleMaterialSymbols {
    match kind {
        MapInspectionNodeKind::Bubble => GoogleMaterialSymbols::Public,
        MapInspectionNodeKind::BaseContainer | MapInspectionNodeKind::Scenario => {
            GoogleMaterialSymbols::AccountTree
        }
        MapInspectionNodeKind::Table => GoogleMaterialSymbols::TableRows,
        MapInspectionNodeKind::Entry => GoogleMaterialSymbols::Dataset,
        MapInspectionNodeKind::StaticGeometry
        | MapInspectionNodeKind::StaticInstance
        | MapInspectionNodeKind::Terrain => GoogleMaterialSymbols::Landscape,
        MapInspectionNodeKind::RigidModel | MapInspectionNodeKind::DynamicModel => {
            GoogleMaterialSymbols::DeployedCode
        }
        MapInspectionNodeKind::LightCollection
        | MapInspectionNodeKind::Light
        | MapInspectionNodeKind::ShadowingLight => GoogleMaterialSymbols::Lightbulb,
        MapInspectionNodeKind::Cubemap => GoogleMaterialSymbols::PanoramaPhotosphere,
        MapInspectionNodeKind::Atmosphere => GoogleMaterialSymbols::Cloud,
        MapInspectionNodeKind::SkyCollection | MapInspectionNodeKind::SkyObject => {
            GoogleMaterialSymbols::WeatherMix
        }
        MapInspectionNodeKind::SpawnPoint => GoogleMaterialSymbols::MyLocation,
        MapInspectionNodeKind::EntityResource | MapInspectionNodeKind::TableResource => {
            GoogleMaterialSymbols::Memory
        }
        MapInspectionNodeKind::DeferredResource => GoogleMaterialSymbols::Schedule,
        MapInspectionNodeKind::FailedResource => GoogleMaterialSymbols::Error,
        MapInspectionNodeKind::MetadataOnly => GoogleMaterialSymbols::DataObject,
    }
}

fn disclosure_points(rect: Rect, open: bool) -> [Pos2; 3] {
    let center = rect.center();
    let points = [
        center + vec2(-3.0, -5.0),
        center + vec2(4.0, 0.0),
        center + vec2(-3.0, 5.0),
    ];
    if !open {
        return points;
    }
    let rotation = egui::emath::Rot2::from_angle(std::f32::consts::FRAC_PI_2);
    points.map(|point| center + rotation * (point - center))
}

fn tree_disclosure(ui: &mut Ui, open: bool) -> egui::Response {
    let (rect, response) =
        ui.allocate_exact_size(vec2(18.0, ui.spacing().interact_size.y), Sense::click());
    let color = if response.hovered() {
        ui.visuals().widgets.hovered.fg_stroke.color
    } else {
        ui.visuals().widgets.inactive.fg_stroke.color
    };
    ui.painter().add(Shape::convex_polygon(
        disclosure_points(rect, open).to_vec(),
        color,
        Stroke::NONE,
    ));
    response
}

fn paint_tree_guides(ui: &Ui, row_rect: Rect, row: MapTreeRow, guides: &[bool]) {
    if row.depth == 0 {
        return;
    }
    let Some(guides) = guides.get(row.guide_start..row.guide_start + row.guide_len) else {
        return;
    };
    let stroke = ui.visuals().widgets.noninteractive.bg_stroke;
    let center_y = row_rect.center().y;
    for (depth, &continues) in guides.iter().enumerate() {
        let x = row_rect.left() + 7.0 + depth as f32 * 14.0;
        let is_current = depth + 1 == row.depth;
        if continues || !is_current {
            if continues {
                ui.painter().line_segment(
                    [
                        Pos2::new(x, row_rect.top()),
                        Pos2::new(x, row_rect.bottom()),
                    ],
                    stroke,
                );
            }
        } else {
            ui.painter().line_segment(
                [Pos2::new(x, row_rect.top()), Pos2::new(x, center_y)],
                stroke,
            );
        }
        if is_current {
            ui.painter().line_segment(
                [
                    Pos2::new(x, center_y),
                    Pos2::new(row_rect.left() + row.depth as f32 * 14.0 + 9.0, center_y),
                ],
                stroke,
            );
        }
    }
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
    let mut filters_changed = false;
    ui.horizontal(|ui| {
        egui::ComboBox::from_id_salt("map_workspace_visibility")
            .selected_text(match state.hidden_filter {
                MapHiddenFilter::All => "All entities",
                MapHiddenFilter::HiddenOnly => "Hidden",
                MapHiddenFilter::VisibleOnly => "Visible",
            })
            .show_ui(ui, |ui| {
                ui.selectable_value(
                    &mut state.hidden_filter,
                    MapHiddenFilter::All,
                    "All entities",
                );
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
        egui::containers::menu::MenuButton::new("Filters")
            .config(
                egui::containers::menu::MenuConfig::new()
                    .close_behavior(egui::PopupCloseBehavior::IgnoreClicks),
            )
            .ui(ui, |ui| {
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
    });
    if filters_changed {
        state.invalidate_rows();
    }
    ui.add(egui::TextEdit::singleline(&mut state.search).hint_text("Search hierarchy…"));
    state.rebuild_rows(inspection, scene);
    state.rebuild_tree(inspection);

    let row_height = ui.spacing().interact_size.y;
    let rows = &state.view_cache.tree_rows;
    let guides = &state.view_cache.tree_guides;
    let mode = state.outliner_mode;
    let selected = state.selected;
    let searching = !state.view_cache.query.is_empty();
    let collapsed = &state.collapsed_nodes;
    let mut disclosures = Vec::new();
    let mut selection = None;
    let mut focus_node = None;
    let mut hover_node = None;
    let mut visibility_changes = Vec::new();

    ui.scope(|ui| {
        ui.style_mut().wrap_mode = Some(TextWrapMode::Extend);
        ScrollArea::vertical()
            .id_salt("map_workspace_outliner")
            .show_rows(ui, row_height, rows.len(), |ui, row_range| {
                if rows.is_empty() && mode == MapOutlinerMode::Spawns {
                    ui.weak("No authored or activity-correlated spawn placements were discovered.");
                }
                for index in row_range {
                    let row = rows[index];
                    let Some(node) = inspection.node(row.id) else {
                        continue;
                    };
                    let row_response = ui.push_id(row.id.0, |ui| {
                        ui.horizontal(|ui| {
                            if mode != MapOutlinerMode::Spawns {
                                ui.add_space(row.depth as f32 * 14.0);
                                if row.has_children {
                                    let open = searching || !collapsed.contains(&row.id);
                                    if tree_disclosure(ui, open).clicked() {
                                        disclosures.push(row.id);
                                    }
                                } else {
                                    ui.allocate_exact_size(
                                        vec2(18.0, ui.spacing().interact_size.y),
                                        Sense::hover(),
                                    );
                                }
                            }
                            ui.label(map_node_icon(node.kind).to_string());
                            let label = ui.selectable_label(selected == Some(row.id), &node.label);
                            if label.clicked() {
                                selection = Some(row.id);
                            }
                            if label.double_clicked() {
                                focus_node = Some(row.id);
                            }
                            if label.hovered() {
                                hover_node = Some(row.id);
                            }
                            if node.kind.is_visual_owner() {
                                let visible = node.world_entity.is_some_and(|entity| {
                                    scene
                                        .world
                                        .get::<&MapEntityVisibility>(entity)
                                        .is_ok_and(|value| value.visible)
                                });
                                if ui
                                    .small_button(if visible { "Hide" } else { "Show" })
                                    .clicked()
                                {
                                    visibility_changes.push((row.id, !visible));
                                }
                            }
                        })
                    });
                    if mode != MapOutlinerMode::Spawns {
                        paint_tree_guides(ui, row_response.inner.response.rect, row, guides);
                    }
                }
            });
    });

    for id in disclosures {
        if !state.collapsed_nodes.remove(&id) {
            state.collapsed_nodes.insert(id);
        }
        state.view_cache.tree_valid = false;
    }
    for (id, visible) in visibility_changes {
        set_node_visibility(&mut scene.world, inspection, id, visible, false);
        state.invalidate_rows();
    }
    if let Some(id) = selection {
        state.select_node(id);
    }
    if let Some(id) = focus_node {
        *focus = Some(id);
    }
    if let Some(id) = hover_node {
        state.hovered = Some(id);
    }
}

fn push_tree_row(
    rows: &mut Vec<MapTreeRow>,
    guide_arena: &mut Vec<bool>,
    guide_path: &[bool],
    depth: usize,
    id: MapInspectionNodeId,
    has_children: bool,
) {
    let guide_start = guide_arena.len();
    guide_arena.extend_from_slice(guide_path);
    rows.push(MapTreeRow {
        depth,
        id,
        has_children,
        guide_start,
        guide_len: guide_path.len(),
    });
}

fn append_graph_rows(
    inspection: &ShadowkeepMapInspection,
    id: MapInspectionNodeId,
    depth: usize,
    admitted: &BTreeSet<MapInspectionNodeId>,
    collapsed: &BTreeSet<MapInspectionNodeId>,
    respect_collapsed: bool,
    rows: &mut Vec<MapTreeRow>,
    guide_arena: &mut Vec<bool>,
    guide_path: &mut Vec<bool>,
) {
    if !admitted.contains(&id) {
        return;
    }
    let Some(node) = inspection.node(id) else {
        return;
    };
    let has_children = node.children.iter().any(|child| admitted.contains(child));
    push_tree_row(rows, guide_arena, guide_path, depth, id, has_children);
    if respect_collapsed && collapsed.contains(&id) {
        return;
    }
    let mut children = node
        .children
        .iter()
        .copied()
        .filter(|child| admitted.contains(child))
        .peekable();
    while let Some(child) = children.next() {
        guide_path.push(children.peek().is_some());
        append_graph_rows(
            inspection,
            child,
            depth + 1,
            admitted,
            collapsed,
            respect_collapsed,
            rows,
            guide_arena,
            guide_path,
        );
        guide_path.pop();
    }
}

fn admitted_with_ancestors(
    inspection: &ShadowkeepMapInspection,
    matching: impl IntoIterator<Item = MapInspectionNodeId>,
) -> BTreeSet<MapInspectionNodeId> {
    let mut admitted = BTreeSet::new();
    for id in matching {
        let mut current = Some(id);
        while let Some(node_id) = current {
            if !admitted.insert(node_id) {
                break;
            }
            current = inspection.node(node_id).and_then(|node| node.parent);
        }
    }
    admitted
}

fn hierarchy_rows(
    inspection: &ShadowkeepMapInspection,
    matching: &[MapInspectionNodeId],
    collapsed: &BTreeSet<MapInspectionNodeId>,
    respect_collapsed: bool,
    include_all_matches: bool,
    rows: &mut Vec<MapTreeRow>,
    guide_arena: &mut Vec<bool>,
) {
    rows.clear();
    guide_arena.clear();
    let admitted = admitted_with_ancestors(
        inspection,
        matching.iter().copied().filter(|id| {
            include_all_matches
                || inspection
                    .node(*id)
                    .is_some_and(|node| is_world_node(node.kind))
        }),
    );
    append_graph_rows(
        inspection,
        inspection.root,
        0,
        &admitted,
        collapsed,
        respect_collapsed,
        rows,
        guide_arena,
        &mut Vec::new(),
    );
}

fn source_rows(
    inspection: &ShadowkeepMapInspection,
    matching: &[MapInspectionNodeId],
    collapsed: &BTreeSet<MapInspectionNodeId>,
    respect_collapsed: bool,
    rows: &mut Vec<MapTreeRow>,
    guide_arena: &mut Vec<bool>,
) {
    rows.clear();
    guide_arena.clear();
    let admitted = admitted_with_ancestors(inspection, matching.iter().copied());
    let group_visible =
        |group: &&crate::world::shadowkeep_inspection::MapInspectionSourceGroupIndex| {
            admitted.contains(&group.node)
                || group.tables.iter().any(|table| admitted.contains(table))
        };
    let root_has_children = inspection.source_groups.values().any(|group| {
        admitted.contains(&group.node) || group.tables.iter().any(|table| admitted.contains(table))
    });
    push_tree_row(
        rows,
        guide_arena,
        &[],
        0,
        inspection.root,
        root_has_children,
    );
    if respect_collapsed && collapsed.contains(&inspection.root) {
        return;
    }
    let mut guide_path = Vec::new();
    let mut groups = inspection
        .source_groups
        .values()
        .filter(group_visible)
        .peekable();
    while let Some(group) = groups.next() {
        guide_path.push(groups.peek().is_some());
        let has_tables = group.tables.iter().any(|table| admitted.contains(table));
        push_tree_row(rows, guide_arena, &guide_path, 1, group.node, has_tables);
        if !(respect_collapsed && collapsed.contains(&group.node)) {
            let mut tables = group
                .tables
                .iter()
                .copied()
                .filter(|table| admitted.contains(table))
                .peekable();
            while let Some(table) = tables.next() {
                guide_path.push(tables.peek().is_some());
                append_graph_rows(
                    inspection,
                    table,
                    2,
                    &admitted,
                    collapsed,
                    respect_collapsed,
                    rows,
                    guide_arena,
                    &mut guide_path,
                );
                guide_path.pop();
            }
        }
        guide_path.pop();
    }
}

fn is_world_node(kind: MapInspectionNodeKind) -> bool {
    matches!(
        kind,
        MapInspectionNodeKind::StaticGeometry
            | MapInspectionNodeKind::StaticInstance
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
    ui.label(RichText::new(&node.label).strong().size(18.0));
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
    if let Some(definition) = node.activity_definition {
        ui.separator();
        ui.strong("Activity spawn evidence");
        ui.monospace(format!("Squad spawn rule {definition}"));
        if let Some(offset) = node.activity_reference_offset {
            ui.monospace(format!("WorldID reference offset 0x{offset:X}"));
        }
        if let Some(count) = node.activity_reference_count {
            ui.label(format!(
                "{count} same-scenario spawn rule{} reference this placement",
                if count == 1 { "" } else { "s" }
            ));
        }
        ui.weak("Exact serialized WorldID match in the bubble's freeroam activity data.");
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
    use crate::world::shadowkeep_inspection::{
        MapInspectionDisposition, MapInspectionGraphBuilder, MapInspectionNode,
        MapInspectionSourceGroup, ShadowkeepTableSources,
    };

    fn collapsible_inspection() -> (
        ShadowkeepMapInspection,
        MapInspectionNodeId,
        MapInspectionNodeId,
        MapInspectionNodeId,
    ) {
        let mut builder = MapInspectionGraphBuilder::new(TagHash(1), TagHash(2));
        let group_key = MapInspectionSourceGroup::BaseContainer(TagHash(3));
        let group = builder.add_source_group(group_key.clone());
        let table = builder.add_node(
            Some(builder.root()),
            MapInspectionNode::new(
                MapInspectionNodeKind::Table,
                MapInspectionDisposition::NonRendering,
                "Table",
                ShadowkeepTableSources::default(),
            ),
        );
        builder.reference_table(&group_key, table);
        let spawn = builder.add_node(
            Some(table),
            MapInspectionNode::new(
                MapInspectionNodeKind::SpawnPoint,
                MapInspectionDisposition::NonRendering,
                "Spawn",
                ShadowkeepTableSources::default(),
            ),
        );
        (builder.finalize(), group, table, spawn)
    }

    fn world_rows(
        inspection: &ShadowkeepMapInspection,
        matching: &[MapInspectionNodeId],
        collapsed: &BTreeSet<MapInspectionNodeId>,
        respect_collapsed: bool,
    ) -> (Vec<MapTreeRow>, Vec<bool>) {
        let mut rows = Vec::new();
        let mut guides = Vec::new();
        hierarchy_rows(
            inspection,
            matching,
            collapsed,
            respect_collapsed,
            false,
            &mut rows,
            &mut guides,
        );
        (rows, guides)
    }

    fn source_tree_rows(
        inspection: &ShadowkeepMapInspection,
        matching: &[MapInspectionNodeId],
        collapsed: &BTreeSet<MapInspectionNodeId>,
        respect_collapsed: bool,
    ) -> (Vec<MapTreeRow>, Vec<bool>) {
        let mut rows = Vec::new();
        let mut guides = Vec::new();
        source_rows(
            inspection,
            matching,
            collapsed,
            respect_collapsed,
            &mut rows,
            &mut guides,
        );
        (rows, guides)
    }

    fn row_data(
        rows: &[MapTreeRow],
        guides: &[bool],
    ) -> Vec<(MapInspectionNodeId, usize, bool, Vec<bool>)> {
        rows.iter()
            .map(|row| {
                (
                    row.id,
                    row.depth,
                    row.has_children,
                    guides[row.guide_start..row.guide_start + row.guide_len].to_vec(),
                )
            })
            .collect()
    }

    #[test]
    fn workspace_starts_in_the_visible_world_mode() {
        let state = MapWorkspaceState::default();
        assert_eq!(state.outliner_mode, MapOutlinerMode::World);
        assert!(state.show_outliner);
        assert!(state.show_inspector);
        assert!(state.show_spawn_markers);
        assert!(!state.show_visual_helpers);
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

    #[test]
    fn world_hierarchy_respects_collapsed_table_state() {
        let (inspection, _, table, spawn) = collapsible_inspection();
        let mut collapsed = BTreeSet::from([table]);
        let (rows, guides) = world_rows(&inspection, &[spawn], &collapsed, true);
        assert_eq!(
            row_data(&rows, &guides),
            vec![
                (inspection.root, 0, true, vec![]),
                (table, 1, true, vec![false]),
            ]
        );

        collapsed.remove(&table);
        let (rows, guides) = world_rows(&inspection, &[spawn], &collapsed, true);
        assert_eq!(
            row_data(&rows, &guides),
            vec![
                (inspection.root, 0, true, vec![]),
                (table, 1, true, vec![false]),
                (spawn, 2, false, vec![false, false]),
            ]
        );
    }

    #[test]
    fn source_hierarchy_expands_group_then_table() {
        let (inspection, group, table, spawn) = collapsible_inspection();
        let mut collapsed = BTreeSet::from([group, table]);
        let (rows, guides) = source_tree_rows(&inspection, &[spawn], &collapsed, true);
        assert_eq!(
            row_data(&rows, &guides),
            vec![
                (inspection.root, 0, true, vec![]),
                (group, 1, true, vec![false]),
            ]
        );

        collapsed.remove(&group);
        let (rows, guides) = source_tree_rows(&inspection, &[spawn], &collapsed, true);
        assert_eq!(
            row_data(&rows, &guides),
            vec![
                (inspection.root, 0, true, vec![]),
                (group, 1, true, vec![false]),
                (table, 2, true, vec![false, false]),
            ]
        );

        collapsed.remove(&table);
        let (rows, guides) = source_tree_rows(&inspection, &[spawn], &collapsed, true);
        assert_eq!(
            row_data(&rows, &guides),
            vec![
                (inspection.root, 0, true, vec![]),
                (group, 1, true, vec![false]),
                (table, 2, true, vec![false, false]),
                (spawn, 3, false, vec![false, false, false]),
            ]
        );
    }

    #[test]
    fn world_search_forces_expansion_without_mutating_collapse_state() {
        let (inspection, _, table, spawn) = collapsible_inspection();
        let collapsed = BTreeSet::from([table]);
        let before = collapsed.clone();
        let (rows, _) = world_rows(&inspection, &[spawn], &collapsed, false);
        assert!(rows.iter().any(|row| row.id == spawn));
        assert_eq!(collapsed, before);
    }

    #[test]
    fn source_search_forces_expansion_without_mutating_collapse_state() {
        let (inspection, group, table, spawn) = collapsible_inspection();
        let collapsed = BTreeSet::from([group, table]);
        let before = collapsed.clone();
        let (rows, _) = source_tree_rows(&inspection, &[spawn], &collapsed, false);
        assert!(rows.iter().any(|row| row.id == spawn));
        assert_eq!(collapsed, before);
    }

    #[test]
    fn source_no_match_retains_leaf_root() {
        let (inspection, _, _, _) = collapsible_inspection();
        let (rows, guides) = source_tree_rows(&inspection, &[], &BTreeSet::new(), false);
        assert_eq!(
            row_data(&rows, &guides),
            vec![(inspection.root, 0, false, vec![])]
        );
    }

    #[test]
    fn admitted_siblings_drive_guide_continuations() {
        let mut builder = MapInspectionGraphBuilder::new(TagHash(1), TagHash(2));
        let mut branches = Vec::new();
        for label in ["First", "Last"] {
            let table = builder.add_node(
                Some(builder.root()),
                MapInspectionNode::new(
                    MapInspectionNodeKind::Table,
                    MapInspectionDisposition::NonRendering,
                    label,
                    ShadowkeepTableSources::default(),
                ),
            );
            let spawn = builder.add_node(
                Some(table),
                MapInspectionNode::new(
                    MapInspectionNodeKind::SpawnPoint,
                    MapInspectionDisposition::NonRendering,
                    format!("{label} spawn"),
                    ShadowkeepTableSources::default(),
                ),
            );
            branches.push((table, spawn));
        }
        let inspection = builder.finalize();
        let matching = [branches[0].1, branches[1].1];
        let (rows, guides) = world_rows(&inspection, &matching, &BTreeSet::new(), true);
        let data = row_data(&rows, &guides);
        assert_eq!(data[1].3, vec![true]);
        assert_eq!(data[2].3, vec![true, false]);
        assert_eq!(data[3].3, vec![false]);

        let (filtered_rows, filtered_guides) =
            world_rows(&inspection, &[branches[1].1], &BTreeSet::new(), true);
        assert_eq!(row_data(&filtered_rows, &filtered_guides)[1].3, vec![false]);
    }

    #[test]
    fn disclosure_geometry_rotates_right_to_down() {
        let rect = Rect::from_min_size(Pos2::ZERO, vec2(18.0, 18.0));
        let closed = disclosure_points(rect, false);
        let open = disclosure_points(rect, true);
        assert!(closed[1].x > closed[0].x && closed[1].x > closed[2].x);
        assert!(open[1].y > open[0].y && open[1].y > open[2].y);
    }

    #[test]
    fn workspace_summary_counts_finalized_inspection_once() {
        let mut builder = MapInspectionGraphBuilder::new(TagHash(1), TagHash(2));
        let root = builder.root();
        builder.add_node(
            Some(root),
            MapInspectionNode::new(
                MapInspectionNodeKind::StaticGeometry,
                MapInspectionDisposition::Rendering,
                "Geometry",
                ShadowkeepTableSources::default(),
            ),
        );
        builder.add_node(
            Some(root),
            MapInspectionNode::new(
                MapInspectionNodeKind::FailedResource,
                MapInspectionDisposition::Failed,
                "Failed",
                ShadowkeepTableSources::default(),
            ),
        );
        builder.add_node(
            Some(root),
            MapInspectionNode::new(
                MapInspectionNodeKind::SpawnPoint,
                MapInspectionDisposition::NonRendering,
                "Spawn",
                ShadowkeepTableSources::default(),
            ),
        );
        let summary = MapWorkspaceSummary::from_inspection(&builder.finalize());
        assert_eq!(
            summary,
            MapWorkspaceSummary {
                ready: 1,
                failed: 1,
                visual: 1,
                metadata: 1,
                spawns: 1,
            }
        );
    }
}

#[cfg(test)]
mod visibility_tests {
    use super::*;

    #[test]
    fn map_workspace_defaults_reduce_helper_noise() {
        let mut state = MapWorkspaceState::default();
        assert!(!state.show_visual_helpers);
        assert!(state.show_visual_labels);
        state.select_node(MapInspectionNodeId(4));
        state.select_node(MapInspectionNodeId(9));
        assert_eq!(state.selected, Some(MapInspectionNodeId(9)));
    }

    #[test]
    fn map_workspace_hidden_filter_treats_unbound_locators_as_hidden() {
        assert!(locator_admitted_by_visibility(MapHiddenFilter::All, true));
        assert!(locator_admitted_by_visibility(MapHiddenFilter::All, false));
        assert!(locator_admitted_by_visibility(
            MapHiddenFilter::HiddenOnly,
            false
        ));
        assert!(!locator_admitted_by_visibility(
            MapHiddenFilter::HiddenOnly,
            true
        ));
        assert!(locator_admitted_by_visibility(
            MapHiddenFilter::VisibleOnly,
            true
        ));
        assert!(!locator_admitted_by_visibility(
            MapHiddenFilter::VisibleOnly,
            false
        ));
    }
}

use egui::{Color32, Key, Response, ScrollArea, TextEdit, Ui, vec2};
use egui_extras::{Column, TableBuilder};
use google_material_symbols::GoogleMaterialSymbols;
use tiger_pkg::TagHash;

use crate::{
    ui::util::DButton,
    world::shadowkeep_map::{
        ShadowkeepBubbleCatalog, ShadowkeepBubbleCatalogEntry, shadowkeep_bubble_catalog,
    },
};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum BubbleSort {
    #[default]
    Name,
    Package,
    Tables,
    Tag,
}

impl BubbleSort {
    fn label(self) -> &'static str {
        match self {
            Self::Name => "Name",
            Self::Package => "Package",
            Self::Tables => "Tables",
            Self::Tag => "Tag",
        }
    }
}

#[derive(Default)]
pub struct BubbleBrowserState {
    pub search: String,
    pub package: String,
    pub selected: Option<TagHash>,
    sort: BubbleSort,
    descending: bool,
    cache_key: Option<(String, String, BubbleSort, bool)>,
    rows: Vec<usize>,
}

impl BubbleBrowserState {
    fn rebuild(&mut self, catalog: &ShadowkeepBubbleCatalog) {
        let query = self.search.trim().to_ascii_lowercase();
        let key = (
            query.clone(),
            self.package.clone(),
            self.sort,
            self.descending,
        );
        if self.cache_key.as_ref() != Some(&key) {
            self.cache_key = Some(key);
            self.rows = catalog
                .entries
                .iter()
                .enumerate()
                .filter_map(|(index, entry)| {
                    ((query.is_empty() || entry.search_text.contains(&query))
                        && (self.package.is_empty() || entry.package_name == self.package))
                        .then_some(index)
                })
                .collect();
            self.rows.sort_unstable_by(|left, right| {
                let left = &catalog.entries[*left];
                let right = &catalog.entries[*right];
                let order = match self.sort {
                    BubbleSort::Name => left.display_name.cmp(&right.display_name),
                    BubbleSort::Package => left.package_name.cmp(&right.package_name),
                    BubbleSort::Tables => left.table_count.cmp(&right.table_count),
                    BubbleSort::Tag => left.tag.cmp(&right.tag),
                }
                .then_with(|| left.tag.cmp(&right.tag));
                if self.descending {
                    order.reverse()
                } else {
                    order
                }
            });
        }

        if self.selected.is_some_and(|selected| {
            !self
                .rows
                .iter()
                .any(|index| catalog.entries[*index].tag == selected)
        }) {
            self.selected = None;
        }
    }
}

fn selected_entry<'a>(
    catalog: &'a ShadowkeepBubbleCatalog,
    state: &BubbleBrowserState,
) -> Option<&'a ShadowkeepBubbleCatalogEntry> {
    state
        .selected
        .and_then(|tag| catalog.entries.iter().find(|entry| entry.tag == tag))
}

fn exact_tag_entry<'a>(
    catalog: &'a ShadowkeepBubbleCatalog,
    query: &str,
) -> Option<&'a ShadowkeepBubbleCatalogEntry> {
    let tag = query.trim().parse::<TagHash>().ok()?;
    catalog.entries.iter().find(|entry| entry.tag == tag)
}

fn admit_entry(entry: Option<&ShadowkeepBubbleCatalogEntry>) -> Option<TagHash> {
    entry.filter(|entry| entry.readable).map(|entry| entry.tag)
}

fn admit_tag(catalog: &ShadowkeepBubbleCatalog, tag: TagHash) -> Option<TagHash> {
    admit_entry(catalog.entries.iter().find(|entry| entry.tag == tag))
}

fn selected_open(catalog: &ShadowkeepBubbleCatalog, state: &BubbleBrowserState) -> Option<TagHash> {
    admit_entry(selected_entry(catalog, state))
}

fn enter_open(catalog: &ShadowkeepBubbleCatalog, state: &BubbleBrowserState) -> Option<TagHash> {
    admit_entry(exact_tag_entry(catalog, &state.search)).or_else(|| selected_open(catalog, state))
}

fn copy_context_actions(response: &Response, entry: &ShadowkeepBubbleCatalogEntry) {
    response.context_menu(|ui| {
        if ui.button("Copy tag").clicked() {
            ui.ctx().copy_text(entry.tag.to_string());
            ui.close();
        }
        if ui.button("Copy package").clicked() {
            ui.ctx().copy_text(entry.package_name.clone());
            ui.close();
        }
        if ui.button("Copy package path").clicked() {
            ui.ctx().copy_text(entry.package_path.clone());
            ui.close();
        }
    });
}

fn selected_details(ui: &mut Ui, entry: Option<&ShadowkeepBubbleCatalogEntry>) {
    let Some(entry) = entry else {
        ui.weak("Select a bubble to inspect its package and map metadata.");
        return;
    };
    ui.separator();
    ui.collapsing("Selected bubble details", |ui| {
        ui.monospace(format!("Tag: {}", entry.tag));
        ui.label(format!("Package: {}", entry.package_name));
        ui.label(format!("Path: {}", entry.package_path));
        ui.label(format!(
            "Child map: {}",
            entry
                .child_map
                .map_or_else(|| "—".to_owned(), |tag| tag.to_string())
        ));
        ui.weak("Freeroam scenario layers are decoded when this bubble opens.");
        ui.label(format!(
            "{} containers · {} tables",
            entry.container_count, entry.table_count
        ));
        if let Some(error) = &entry.error {
            ui.colored_label(Color32::LIGHT_RED, error);
        }
    });
}

fn package_combo(
    ui: &mut Ui,
    id: &'static str,
    state: &mut BubbleBrowserState,
    catalog: &ShadowkeepBubbleCatalog,
) {
    egui::ComboBox::from_id_salt(id)
        .selected_text(if state.package.is_empty() {
            "All packages"
        } else {
            &state.package
        })
        .show_ui(ui, |ui| {
            ui.selectable_value(&mut state.package, String::new(), "All packages");
            for package in &catalog.package_names {
                ui.selectable_value(&mut state.package, package.clone(), package);
            }
        });
}

fn sort_controls(ui: &mut Ui, id: &'static str, state: &mut BubbleBrowserState) {
    egui::ComboBox::from_id_salt(id)
        .selected_text(state.sort.label())
        .show_ui(ui, |ui| {
            ui.selectable_value(&mut state.sort, BubbleSort::Name, "Name");
            ui.selectable_value(&mut state.sort, BubbleSort::Package, "Package");
            ui.selectable_value(&mut state.sort, BubbleSort::Tables, "Tables");
            ui.selectable_value(&mut state.sort, BubbleSort::Tag, "Tag");
        });
    if ui
        .button(if state.descending {
            "Descending"
        } else {
            "Ascending"
        })
        .clicked()
    {
        state.descending = !state.descending;
    }
}

fn search_toolbar(
    ui: &mut Ui,
    package_id: &'static str,
    sort_id: &'static str,
    state: &mut BubbleBrowserState,
    catalog: &ShadowkeepBubbleCatalog,
    include_package: bool,
) {
    ui.horizontal_wrapped(|ui| {
        ui.add(
            TextEdit::singleline(&mut state.search)
                .hint_text("Search bubbles by name, package, or tag…")
                .desired_width(280.0),
        );
        if ui
            .button(GoogleMaterialSymbols::Close.to_string())
            .on_hover_text("Clear search")
            .clicked()
        {
            state.search.clear();
        }
        if include_package {
            package_combo(ui, package_id, state, catalog);
        }
        sort_controls(ui, sort_id, state);
    });
}

fn package_rail(ui: &mut Ui, state: &mut BubbleBrowserState, catalog: &ShadowkeepBubbleCatalog) {
    ScrollArea::vertical().show(ui, |ui| {
        let choices = std::iter::once("").chain(catalog.package_names.iter().map(String::as_str));
        for package in choices {
            let label = if package.is_empty() {
                "All packages"
            } else {
                package
            };
            let button = if state.package == package {
                DButton::new_white(label)
            } else {
                DButton::new(label)
            }
            .min_size(vec2(ui.available_width(), 42.0))
            .padding(vec2(12.0, 8.0));
            if button.ui(ui).clicked() {
                state.package = package.to_owned();
            }
        }
    });
}

fn catalog_results(
    ui: &mut Ui,
    state: &mut BubbleBrowserState,
    catalog: &ShadowkeepBubbleCatalog,
    narrow: bool,
) -> Option<TagHash> {
    search_toolbar(
        ui,
        "bubble_catalog_package",
        "bubble_catalog_sort",
        state,
        catalog,
        narrow,
    );
    state.rebuild(catalog);
    ui.weak(format!("{} visible results", state.rows.len()));

    let mut open = ui
        .input(|input| input.key_pressed(Key::Enter))
        .then(|| enter_open(catalog, state))
        .flatten();
    if state.rows.is_empty() {
        ui.add_space(24.0);
        ui.label("No bubbles match the current search and package filters.");
        if DButton::new("CLEAR FILTERS").ui(ui).clicked() {
            state.search.clear();
            state.package.clear();
            state.cache_key = None;
        }
        return None;
    }

    let list_height = (ui.available_height() - 190.0).max(120.0);
    ScrollArea::vertical().max_height(list_height).show_rows(
        ui,
        60.0,
        state.rows.len(),
        |ui, range| {
            for row in range {
                let entry = &catalog.entries[state.rows[row]];
                let status = if entry.readable {
                    "Ready"
                } else {
                    "Unreadable"
                };
                let subtitle = format!(
                    "{} · {} · {} tables · {status}",
                    entry.package_name, entry.tag, entry.table_count
                );
                let atoms = (
                    GoogleMaterialSymbols::Map.to_string(),
                    entry.display_name.as_str(),
                );
                let button = if state.selected == Some(entry.tag) {
                    DButton::new_white(atoms)
                } else {
                    DButton::new(atoms)
                }
                .subtitle(subtitle)
                .min_size(vec2(ui.available_width(), 60.0));
                let response = button.ui(ui);
                if response.clicked() {
                    state.selected = Some(entry.tag);
                }
                if response.double_clicked() {
                    open = admit_tag(catalog, entry.tag);
                }
                response
                    .clone()
                    .on_hover_text(entry.error.as_deref().unwrap_or(&entry.package_path));
                copy_context_actions(&response, entry);
            }
        },
    );

    selected_details(ui, selected_entry(catalog, state));
    let selected = selected_open(catalog, state);
    ui.add_enabled_ui(selected.is_some(), |ui| {
        if DButton::new(format!("{} OPEN MAP", GoogleMaterialSymbols::Map))
            .min_size(vec2(ui.available_width(), 60.0))
            .ui(ui)
            .clicked()
        {
            open = selected;
        }
    });
    open
}

/// Shows the branded full-page bubble catalog. Returns a readable bubble to open.
pub fn show_catalog(ui: &mut Ui, state: &mut BubbleBrowserState) -> Option<TagHash> {
    let catalog = shadowkeep_bubble_catalog();
    if ui.available_width() < 760.0 {
        return catalog_results(ui, state, catalog, true);
    }

    let mut open = None;
    egui::Panel::left("bubble_package_rail")
        .resizable(true)
        .default_size(280.0)
        .size_range(220.0..=420.0)
        .show(ui, |ui| package_rail(ui, state, catalog));
    egui::CentralPanel::default().show(ui, |ui| {
        open = catalog_results(ui, state, catalog, false);
    });
    open
}

/// Shows the compact table chooser used by the loaded-map workspace.
pub fn show_compact(
    ui: &mut Ui,
    state: &mut BubbleBrowserState,
    current: Option<TagHash>,
) -> Option<TagHash> {
    let catalog = shadowkeep_bubble_catalog();
    search_toolbar(
        ui,
        "bubble_compact_package",
        "bubble_compact_sort",
        state,
        catalog,
        true,
    );
    state.rebuild(catalog);
    ui.weak(format!("{} visible results", state.rows.len()));

    let mut open = ui
        .input(|input| input.key_pressed(Key::Enter))
        .then(|| enter_open(catalog, state))
        .flatten();
    let table_height = (ui.available_height() - 220.0).max(160.0);
    TableBuilder::new(ui)
        .striped(true)
        .max_scroll_height(table_height)
        .column(Column::remainder().at_least(160.0))
        .column(Column::initial(120.0))
        .column(Column::initial(52.0))
        .column(Column::initial(74.0))
        .column(Column::initial(96.0))
        .header(22.0, |mut header| {
            header.col(|ui| {
                ui.strong("Bubble");
            });
            header.col(|ui| {
                ui.strong("Package");
            });
            header.col(|ui| {
                ui.strong("Tables");
            });
            header.col(|ui| {
                ui.strong("Status");
            });
            header.col(|ui| {
                ui.strong("Tag");
            });
        })
        .body(|body| {
            body.rows(22.0, state.rows.len(), |mut row| {
                let entry = &catalog.entries[state.rows[row.index()]];
                let selected = state.selected == Some(entry.tag) || current == Some(entry.tag);
                row.col(|ui| {
                    let response = ui.selectable_label(selected, &entry.display_name);
                    if response.clicked() {
                        state.selected = Some(entry.tag);
                    }
                    if response.double_clicked() {
                        open = admit_tag(catalog, entry.tag);
                    }
                    response
                        .clone()
                        .on_hover_text(entry.error.as_deref().unwrap_or(&entry.package_path));
                    copy_context_actions(&response, entry);
                });
                row.col(|ui| {
                    ui.label(&entry.package_name);
                });
                row.col(|ui| {
                    ui.label(entry.table_count.to_string());
                });
                row.col(|ui| {
                    if entry.readable {
                        ui.colored_label(Color32::LIGHT_GREEN, "Ready");
                    } else {
                        ui.colored_label(Color32::LIGHT_RED, "Unreadable");
                    }
                });
                row.col(|ui| {
                    ui.monospace(entry.tag.to_string());
                });
            });
        });

    selected_details(ui, selected_entry(catalog, state));
    let selected = selected_open(catalog, state);
    ui.add_enabled_ui(selected.is_some(), |ui| {
        if DButton::new(format!("{} OPEN MAP", GoogleMaterialSymbols::Map))
            .min_size(vec2(ui.available_width(), 60.0))
            .ui(ui)
            .clicked()
        {
            open = selected;
        }
    });
    open
}

fn bubble_display_name_from(catalog: &ShadowkeepBubbleCatalog, tag: TagHash) -> String {
    catalog
        .entries
        .iter()
        .find(|entry| entry.tag == tag)
        .map(|entry| entry.display_name.clone())
        .unwrap_or_else(|| format!("Bubble {tag}"))
}

pub fn bubble_display_name(tag: TagHash) -> String {
    bubble_display_name_from(shadowkeep_bubble_catalog(), tag)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(
        tag: u32,
        package: &str,
        name: &str,
        table_count: usize,
        readable: bool,
    ) -> ShadowkeepBubbleCatalogEntry {
        ShadowkeepBubbleCatalogEntry {
            tag: TagHash(tag),
            child_map: None,
            package_name: package.to_owned(),
            package_path: format!("{package}.pkg"),
            map_name_hash: None,
            display_name: name.to_owned(),
            search_text: format!("{} {} {}", name.to_ascii_lowercase(), package, tag),
            container_count: 0,
            table_count,
            scenario: None,
            readable,
            error: (!readable).then(|| "fixture decode error".to_owned()),
        }
    }

    fn catalog() -> ShadowkeepBubbleCatalog {
        ShadowkeepBubbleCatalog {
            entries: vec![
                entry(3, "moon", "Same", 2, true),
                entry(1, "moon", "Same", 1, true),
                entry(2, "keep", "Keep", 1, false),
            ],
            package_names: vec!["keep".to_owned(), "moon".to_owned()],
        }
    }

    #[test]
    fn default_state_builds_rows_and_sorts_equal_names_by_tag() {
        let catalog = catalog();
        let mut state = BubbleBrowserState::default();
        state.rebuild(&catalog);

        assert_eq!(state.rows, vec![2, 1, 0]);
    }

    #[test]
    fn rebuild_normalizes_query_and_filters_package() {
        let catalog = catalog();
        let mut state = BubbleBrowserState {
            search: "  SAME  ".to_owned(),
            package: "moon".to_owned(),
            ..Default::default()
        };
        state.rebuild(&catalog);
        let cache_key = state.cache_key.clone();

        assert_eq!(state.rows, vec![1, 0]);
        state.search = "same".to_owned();
        state.rebuild(&catalog);
        assert_eq!(state.cache_key, cache_key);
        assert_eq!(state.rows, vec![1, 0]);
    }

    #[test]
    fn filtering_out_selection_clears_it_without_replacement() {
        let catalog = catalog();
        let mut state = BubbleBrowserState {
            selected: Some(TagHash(1)),
            package: "keep".to_owned(),
            ..Default::default()
        };
        state.rebuild(&catalog);

        assert_eq!(state.rows, vec![2]);
        assert_eq!(state.selected, None);
    }

    #[test]
    fn open_admission_accepts_readable_exact_and_selected_entries() {
        let catalog = catalog();
        let state = BubbleBrowserState {
            search: "1".to_owned(),
            selected: Some(TagHash(3)),
            ..Default::default()
        };

        assert_eq!(admit_tag(&catalog, TagHash(1)), Some(TagHash(1)));
        assert_eq!(enter_open(&catalog, &state), Some(TagHash(1)));
        assert_eq!(selected_open(&catalog, &state), Some(TagHash(3)));
    }

    #[test]
    fn unreadable_entry_remains_selectable_but_is_never_admitted() {
        let catalog = catalog();
        let state = BubbleBrowserState {
            search: "2".to_owned(),
            selected: Some(TagHash(2)),
            ..Default::default()
        };

        assert!(selected_entry(&catalog, &state).is_some());
        assert_eq!(admit_tag(&catalog, TagHash(2)), None);
        assert_eq!(selected_open(&catalog, &state), None);
        assert_eq!(enter_open(&catalog, &state), None);
    }

    #[test]
    fn absent_display_name_uses_exact_fallback() {
        assert_eq!(
            bubble_display_name_from(&catalog(), TagHash(99)),
            "Bubble 00000063"
        );
    }
}

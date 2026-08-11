use egui::{Color32, Key, Ui};
use egui_extras::{Column, TableBuilder};
use tiger_pkg::TagHash;

use crate::world::shadowkeep_map::{ShadowkeepBubbleCatalog, shadowkeep_bubble_catalog};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum BubbleSort {
    #[default]
    Name,
    Package,
    Tables,
    Tag,
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
        let key = (
            self.search.to_ascii_lowercase(),
            self.package.clone(),
            self.sort,
            self.descending,
        );
        if self.cache_key.as_ref() == Some(&key) {
            return;
        }
        self.cache_key = Some(key);
        let query = self.search.trim().to_ascii_lowercase();
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

    fn sort_button(&mut self, ui: &mut Ui, sort: BubbleSort, label: &str) {
        let label = if self.sort == sort {
            format!("{label} {}", if self.descending { "▼" } else { "▲" })
        } else {
            label.to_owned()
        };
        if ui.small_button(label).clicked() {
            if self.sort == sort {
                self.descending = !self.descending;
            } else {
                self.sort = sort;
                self.descending = false;
            }
            self.cache_key = None;
        }
    }
}

/// Shows the cached catalog in every map entry point. Returns a bubble to open.
pub fn show(
    ui: &mut Ui,
    state: &mut BubbleBrowserState,
    current: Option<TagHash>,
) -> Option<TagHash> {
    let catalog = shadowkeep_bubble_catalog();
    ui.horizontal(|ui| {
        ui.text_edit_singleline(&mut state.search)
            .on_hover_text("Name, package, or exact tag hash");
        egui::ComboBox::from_id_salt("bubble_package")
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
    });
    state.rebuild(catalog);
    if ui.input(|input| input.key_pressed(Key::Enter)) {
        if let Ok(tag) = state.search.trim().parse::<TagHash>() {
            if catalog.entries.iter().any(|entry| entry.tag == tag) {
                return Some(tag);
            }
        }
    }

    let mut open = None;
    TableBuilder::new(ui)
        .striped(true)
        .column(Column::remainder().at_least(180.0))
        .column(Column::initial(150.0))
        .column(Column::initial(56.0))
        .column(Column::initial(96.0))
        .header(22.0, |mut header| {
            header.col(|ui| state.sort_button(ui, BubbleSort::Name, "Bubble"));
            header.col(|ui| state.sort_button(ui, BubbleSort::Package, "Package"));
            header.col(|ui| state.sort_button(ui, BubbleSort::Tables, "Tables"));
            header.col(|ui| state.sort_button(ui, BubbleSort::Tag, "Tag"));
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
                        open = Some(entry.tag);
                    }
                    response.on_hover_text(entry.error.as_deref().unwrap_or(&entry.package_path));
                });
                row.col(|ui| {
                    ui.label(&entry.package_name);
                });
                row.col(|ui| {
                    ui.label(entry.table_count.to_string());
                });
                row.col(|ui| {
                    if entry.readable {
                        ui.monospace(entry.tag.to_string());
                    } else {
                        ui.colored_label(Color32::LIGHT_RED, format!("{} unreadable", entry.tag));
                    }
                });
            });
        });
    open
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::world::shadowkeep_map::ShadowkeepBubbleCatalogEntry;

    fn entry(tag: u32) -> ShadowkeepBubbleCatalogEntry {
        ShadowkeepBubbleCatalogEntry {
            tag: TagHash(tag),
            child_map: None,
            package_name: "pkg".to_owned(),
            package_path: "pkg".to_owned(),
            map_name_hash: None,
            display_name: "Same".to_owned(),
            search_text: "same pkg".to_owned(),
            container_count: 0,
            table_count: 0,
            scenario: None,
            readable: true,
            error: None,
        }
    }

    #[test]
    fn default_state_builds_rows_and_sorts_equal_names_by_tag() {
        let catalog = ShadowkeepBubbleCatalog {
            entries: vec![entry(3), entry(1), entry(2)],
            package_names: vec!["pkg".to_owned()],
        };
        let mut state = BubbleBrowserState::default();
        state.rebuild(&catalog);

        assert_eq!(state.rows, vec![1, 2, 0]);
    }
}

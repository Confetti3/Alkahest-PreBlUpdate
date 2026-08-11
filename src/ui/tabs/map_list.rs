use std::sync::Arc;

use egui::{Margin, Ui};
use tiger_pkg::TagHash;

use super::{Tab, TabResult, map::MapTab};
use crate::{app::SharedState, world::shadowkeep_map::shadowkeep_bubble_catalog};

pub struct MapListTab {
    search: String,
    state: Arc<SharedState>,
}

impl MapListTab {
    pub fn new(state: &Arc<SharedState>) -> Self {
        Self {
            search: String::new(),
            state: state.clone(),
        }
    }

    pub fn ui(&mut self, ui: &mut Ui) -> TabResult {
        let mut open = None::<(TagHash, String)>;
        egui::Frame::new()
            .outer_margin(Margin::same(16))
            .show(ui, |ui| {
                ui.heading("Shadowkeep Bubbles");
                ui.text_edit_singleline(&mut self.search);
                let query = self.search.trim().to_ascii_lowercase();
                egui::ScrollArea::vertical().show(ui, |ui| {
                    for bubble in shadowkeep_bubble_catalog()
                        .entries
                        .iter()
                        .filter(|entry| query.is_empty() || entry.search_text.contains(&query))
                    {
                        let label = format!(
                            "{} · {} · {} tables",
                            bubble.display_name, bubble.tag, bubble.table_count
                        );
                        let response = ui.selectable_label(false, label);
                        if response.double_clicked() {
                            open = Some((bubble.tag, bubble.display_name.clone()));
                        }
                        response.on_hover_text(if bubble.readable {
                            bubble.package_path.clone()
                        } else {
                            bubble
                                .error
                                .clone()
                                .unwrap_or_else(|| "unreadable".to_owned())
                        });
                    }
                });
            });
        match open {
            Some((tag, name)) => match MapTab::new(tag, name, &self.state) {
                Ok(map) => TabResult::Open(Tab::Map(map)),
                Err(error) => {
                    error!("Failed to open Shadowkeep map {tag}: {error:#}");
                    TabResult::Continue
                }
            },
            None => TabResult::Continue,
        }
    }
}

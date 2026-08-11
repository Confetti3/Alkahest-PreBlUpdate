use std::{collections::BTreeMap, sync::Arc};

use egui::{Margin, Ui, vec2};
use tiger_pkg::TagHash;

use super::{Tab, TabResult, map::MapTab};
use crate::{
    app::SharedState,
    ui::util::DButton,
    world::shadowkeep_map::shadowkeep_bubble_catalog,
};

pub struct MapListTab {
    map_tags_by_package: Vec<(String, Vec<(TagHash, String)>)>,
    /// Indexes into `map_tags_by_package`
    current_package_index: Option<usize>,

    state: Arc<SharedState>,
}

impl MapListTab {
    pub fn new(state: &Arc<SharedState>) -> Self {
        let mut by_package = BTreeMap::<String, Vec<(TagHash, String)>>::new();
        for bubble in shadowkeep_bubble_catalog() {
            let name = bubble.map_name_hash.map_or_else(
                || format!("unreadable_{}", bubble.tag),
                |hash| {
                    state
                        .wordlist
                        .get(&hash)
                        .cloned()
                        .unwrap_or_else(|| format!("map_{hash:08X}"))
                },
            );
            by_package
                .entry(bubble.package_name.clone())
                .or_default()
                .push((bubble.tag, name));
        }
        let map_tags_by_package = by_package
            .into_iter()
            .map(|(package, mut bubbles)| {
                bubbles.sort_unstable_by(|left, right| left.1.cmp(&right.1));
                (package, bubbles)
            })
            .collect();

        Self {
            map_tags_by_package,
            current_package_index: None,
            state: state.clone(),
        }
    }

    pub fn ui(&mut self, ui: &mut Ui) -> TabResult {
        let mut result = TabResult::Continue;
        egui::Frame::new()
            .outer_margin(Margin {
                top: 16,
                bottom: 0,
                left: 16,
                right: 16,
            })
            .show(ui, |ui| {
                ui.horizontal_centered(|ui| {
                    ui.vertical(|ui| {
                        egui::ScrollArea::vertical()
                            .auto_shrink([true, false])
                            .id_salt("map_list_packages")
                            .show(ui, |ui| {
                                for (i, (package_name, _map_tags)) in
                                    self.map_tags_by_package.iter().enumerate()
                                {
                                    if if self.current_package_index == Some(i) {
                                        DButton::new_white(package_name)
                                    } else {
                                        DButton::new(package_name)
                                    }
                                    .min_size(vec2(512.0, 32.0))
                                    .ui(ui)
                                    .clicked()
                                    {
                                        self.current_package_index = Some(i);
                                    }
                                }
                            });
                    });

                    ui.separator();

                    ui.vertical(|ui| {
                        egui::ScrollArea::vertical()
                            .id_salt("map_list_maps")
                            .auto_shrink([false, false])
                            .show(ui, |ui| {
                                let current_index = match self.current_package_index {
                                    Some(i) => i,
                                    None => return,
                                };

                                for (tag, name) in self.map_tags_by_package[current_index].1.iter()
                                {
                                    if DButton::new(format!("{name} ({tag})"))
                                        .min_size(vec2(512.0, 32.0))
                                        .ui(ui)
                                        .clicked()
                                    {
                                        match MapTab::new(*tag, name.clone(), &self.state) {
                                            Ok(map) => {
                                                result = TabResult::Open(Tab::Map(map));
                                            }
                                            Err(e) => {
                                                error!("Failed to open map tab: {e}");
                                            }
                                        }
                                    }
                                }
                            });
                    });
                });
            });

        result
    }

}

use std::sync::Arc;

use egui::{Margin, Ui};
use tiger_pkg::TagHash;

use super::{Tab, TabResult, map::MapTab};
use crate::{
    app::SharedState,
    ui::bubble_browser::{BubbleBrowserState, show},
    world::shadowkeep_map::shadowkeep_bubble_catalog,
};

pub struct MapListTab {
    browser: BubbleBrowserState,
    state: Arc<SharedState>,
}

impl MapListTab {
    pub fn new(state: &Arc<SharedState>) -> Self {
        Self {
            browser: BubbleBrowserState::default(),
            state: state.clone(),
        }
    }

    pub fn ui(&mut self, ui: &mut Ui) -> TabResult {
        let mut open = None::<(TagHash, String)>;
        egui::Frame::new()
            .outer_margin(Margin::same(16))
            .show(ui, |ui| {
                ui.heading("Shadowkeep Bubbles");
                if let Some(tag) = show(ui, &mut self.browser, None) {
                    let name = shadowkeep_bubble_catalog()
                        .entries
                        .iter()
                        .find(|entry| entry.tag == tag)
                        .map(|entry| entry.display_name.clone())
                        .unwrap_or_else(|| format!("Bubble {tag}"));
                    open = Some((tag, name));
                }
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

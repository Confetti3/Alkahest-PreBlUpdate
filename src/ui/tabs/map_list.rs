use std::sync::Arc;

use egui::{Margin, Ui};
use tiger_pkg::TagHash;

use super::{Tab, TabResult, map::MapTab};
use crate::{
    app::SharedState,
    ui::bubble_browser::{BubbleBrowserState, bubble_display_name, show_catalog},
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
        let mut open = None::<TagHash>;
        egui::Frame::new()
            .outer_margin(Margin::same(16))
            .show(ui, |ui| {
                ui.heading("Shadowkeep Bubbles");
                open = show_catalog(ui, &mut self.browser);
            });
        match open {
            Some(tag) => match MapTab::new(tag, bubble_display_name(tag), &self.state) {
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

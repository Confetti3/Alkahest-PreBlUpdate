use alkahest_data::shadowkeep::map::{
    SShadowkeepBubbleDefinition, SShadowkeepBubbleParent, SShadowkeepMapDataTable,
};
use egui::{Color32, RichText};
use tiger_parse::PackageManagerExt;
use tiger_pkg::{TagHash, package_manager};

use crate::{
    app::SharedState,
};

#[derive(Default)]
struct MapMetadata {
    containers: usize,
    tables: usize,
    entries: usize,
    unreadable_resources: usize,
}

pub struct MapTab {
    pub tag: TagHash,
    pub name: String,
    metadata: Result<MapMetadata, String>,
    shared: Arc<SharedState>,
}

impl MapTab {
    pub fn new(tag: TagHash, name: String, shared: &Arc<SharedState>) -> anyhow::Result<Self> {
        let metadata = read_metadata(tag).map_err(|error| format!("{error:#}"));
        Ok(Self {
            tag,
            name,
            metadata,
            shared: shared.clone(),
        })
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, _egui_d3d11: &mut egui_d3d11::D3D11Renderer) {
        ui.heading(&self.name);
        ui.label(format!("Legacy Shadowkeep bubble: {}", self.tag));
        ui.add_space(12.0);
        ui.label(RichText::new("3D scene unavailable").color(Color32::from_rgb(224, 128, 96)));
        ui.label(self.shared.renderer_status.read().scene_diagnostic());
        ui.add_space(16.0);

        match &self.metadata {
            Ok(metadata) => {
                ui.label("Read-only map metadata");
                ui.label(format!("containers: {}", metadata.containers));
                ui.label(format!("map tables: {}", metadata.tables));
                ui.label(format!("map entries: {}", metadata.entries));
                ui.label(format!("unreadable referenced resources: {}", metadata.unreadable_resources));
            }
            Err(error) => {
                ui.label(RichText::new("Could not read the legacy map chain").color(Color32::DARK_RED));
                ui.label(error);
            }
        }
    }
}

fn read_metadata(tag: TagHash) -> anyhow::Result<MapMetadata> {
    let parent: SShadowkeepBubbleParent = package_manager().read_tag_struct(tag)?;
    let definition: SShadowkeepBubbleDefinition = package_manager().read_tag_struct(parent.child_map)?;
    let mut metadata = MapMetadata::default();

    for container in &definition.map_resources {
        metadata.containers += 1;
        for table_tag in &container.data_tables {
            let table: SShadowkeepMapDataTable = match package_manager().read_tag_struct(*table_tag) {
                Ok(table) => table,
                Err(_) => {
                    metadata.unreadable_resources += 1;
                    continue;
                }
            };
            metadata.tables += 1;
            metadata.entries += table.data_entries.len();
        }
    }

    Ok(metadata)
}
use std::sync::Arc;

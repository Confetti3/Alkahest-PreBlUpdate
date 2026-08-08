use std::path::Path;

use egui::{Color32, RichText};
use tiger_pkg::TagHash;

use crate::{
    inspection::{InspectionDocument, InspectionKind, format_hex_page},
    ui::tabs::TabResult,
};

pub struct InspectorTab {
    pub tag: TagHash,
    kind: InspectionKind,
    document: Result<InspectionDocument, String>,
    hex_offset: usize,
    export_path: String,
    export_status: Option<String>,
}

impl InspectorTab {
    pub fn new(tag: TagHash, kind: InspectionKind) -> Self {
        let document = InspectionDocument::read(tag, kind).map_err(|error| format!("{error:#}"));
        Self {
            tag,
            kind,
            document,
            hex_offset: 0,
            export_path: format!("exports/{tag}.json"),
            export_status: None,
        }
    }

    pub fn ui(&mut self, ui: &mut egui::Ui) -> TabResult {
        let mut export_requested = false;
        match &self.document {
            Ok(document) => {
                ui.heading(match self.kind {
                    InspectionKind::Tag => format!("Tag Inspector · {}", self.tag),
                    InspectionKind::Activity => format!("Activity Inspector · {}", self.tag),
                });
                ui.label(
                    RichText::new(
                        "Known package metadata is shown below. The complete payload remains available as raw bytes until an era-specific schema proves a field layout.",
                    )
                    .weak(),
                );
                ui.add_space(8.0);

                egui::Grid::new(format!("tag_metadata_{}", self.tag))
                    .num_columns(2)
                    .striped(true)
                    .show(ui, |ui| {
                        metadata_row(
                            ui,
                            "Name",
                            document.record.tag.name.as_deref().unwrap_or("(unnamed)"),
                        );
                        metadata_row(
                            ui,
                            "Package",
                            &format!("{:04X}", document.record.tag.package_id),
                        );
                        metadata_row(
                            ui,
                            "Entry",
                            &format!("{:04X}", document.record.tag.entry_index),
                        );
                        metadata_row(ui, "Class", &document.record.entry.reference);
                        metadata_row(
                            ui,
                            "Type",
                            &format!(
                                "{} / {}",
                                document.record.entry.file_type, document.record.entry.file_subtype
                            ),
                        );
                        metadata_row(
                            ui,
                            "Bytes",
                            &document.record.raw_payload.byte_length.to_string(),
                        );
                        metadata_row(ui, "SHA-256", &document.record.raw_payload.sha256);
                    });

                if !document.record.diagnostics.is_empty() {
                    ui.add_space(8.0);
                    for diagnostic in &document.record.diagnostics {
                        ui.colored_label(Color32::YELLOW, diagnostic);
                    }
                }

                ui.add_space(12.0);
                ui.collapsing("Structural fields", |ui| {
                    for node in &document.record.fields {
                        inspect_node_ui(ui, node);
                    }
                });
                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    ui.label("JSON export");
                    ui.text_edit_singleline(&mut self.export_path);
                    export_requested = ui.button("Export").clicked();
                });
                if let Some(status) = &self.export_status {
                    ui.label(status);
                }

                ui.add_space(12.0);
                ui.separator();
                ui.horizontal(|ui| {
                    ui.heading("Raw payload");
                    ui.label(format!("{} bytes", document.bytes().len()));
                    ui.add_space(8.0);
                    ui.label("Offset");
                    let maximum_offset = document.bytes().len().saturating_sub(1);
                    ui.add(
                        egui::DragValue::new(&mut self.hex_offset)
                            .range(0..=maximum_offset)
                            .speed(16),
                    );
                });
                let page = format_hex_page(document.bytes(), self.hex_offset, 4096);
                egui::ScrollArea::both()
                    .auto_shrink([false, false])
                    .max_height(560.0)
                    .show(ui, |ui| {
                        ui.add(egui::Label::new(RichText::new(page).monospace()).selectable(true));
                    });
            }
            Err(error) => {
                ui.heading(format!("Inspector · {}", self.tag));
                ui.colored_label(Color32::RED, error);
            }
        }

        if export_requested {
            self.export_status = match &self.document {
                Ok(document) => match document.write_json(Path::new(&self.export_path)) {
                    Ok(()) => Some(format!("Exported {}", self.export_path)),
                    Err(error) => Some(format!("Export failed: {error:#}")),
                },
                Err(_) => Some("Export unavailable because the tag could not be read".to_owned()),
            };
        }

        TabResult::Continue
    }
}

fn metadata_row(ui: &mut egui::Ui, label: &str, value: &str) {
    ui.strong(label);
    ui.monospace(value);
    ui.end_row();
}

fn inspect_node_ui(ui: &mut egui::Ui, node: &crate::inspection::InspectNode) {
    let label = format!(
        "{}: {} @ 0x{:X} (0x{:X} bytes, {:?})",
        node.name, node.type_name, node.source_offset, node.encoded_size, node.status
    );
    if node.children.is_empty() {
        ui.monospace(label);
    } else {
        ui.collapsing(label, |ui| {
            for child in &node.children {
                inspect_node_ui(ui, child);
            }
        });
    }
    if let Some(value) = &node.value {
        ui.indent(format!("{}-value", node.name), |ui| {
            ui.monospace(value.to_string());
        });
    }
}

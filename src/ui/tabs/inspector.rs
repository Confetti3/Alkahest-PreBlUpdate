use std::path::Path;

use egui::{Color32, RichText};
use tiger_pkg::TagHash;

use crate::{
    inspection::{InspectionDocument, InspectionKind, format_hex_page},
    task::Task,
    ui::tabs::TabResult,
};

/// Package reads run in a task so opening a tag inspector never stalls a map
/// workspace frame.
pub struct InspectorTab {
    pub tag: TagHash,
    kind: InspectionKind,
    document_task: Option<Task<anyhow::Result<InspectionDocument>>>,
    document: Option<Result<InspectionDocument, String>>,
    hex_offset: usize,
    export_path: String,
    export_status: Option<String>,
}

impl InspectorTab {
    pub fn new(tag: TagHash, kind: InspectionKind) -> Self {
        Self {
            tag,
            kind,
            document_task: Some(Task::new(format!("inspection_{tag}"), move || {
                InspectionDocument::read(tag, kind)
            })),
            document: None,
            hex_offset: 0,
            export_path: format!("exports/{tag}.json"),
            export_status: None,
        }
    }

    fn poll_document(&mut self) {
        let Some(task) = self.document_task.as_mut() else {
            return;
        };
        let Some(result) = task.get() else { return };
        self.document_task = None;
        self.document = Some(
            result
                .map_err(|_| "Inspection task panicked".to_owned())
                .and_then(|result| result.map_err(|error| format!("{error:#}"))),
        );
    }

    fn retry(&mut self) {
        let tag = self.tag;
        let kind = self.kind;
        self.document = None;
        self.document_task = Some(Task::new(format!("inspection_{tag}"), move || {
            InspectionDocument::read(tag, kind)
        }));
    }

    pub fn ui(&mut self, ui: &mut egui::Ui) -> TabResult {
        self.poll_document();
        let Some(document) = self.document.as_ref() else {
            ui.heading(format!("Tag Inspector · {}", self.tag));
            ui.add(egui::Spinner::new());
            ui.weak("Loading package document asynchronously…");
            return TabResult::Continue;
        };
        let mut export_requested = false;
        match document {
            Ok(document) => {
                ui.heading(match self.kind {
                    InspectionKind::Tag => format!("Tag Inspector · {}", self.tag),
                    InspectionKind::Activity => format!("Activity Inspector · {}", self.tag),
                });
                ui.weak("Known package metadata is shown below; raw bytes remain lossless.");
                egui::Grid::new(format!("tag_metadata_{}", self.tag))
                    .num_columns(2)
                    .striped(true)
                    .show(ui, |ui| {
                        row(
                            ui,
                            "Name",
                            document.record.tag.name.as_deref().unwrap_or("(unnamed)"),
                        );
                        row(
                            ui,
                            "Package",
                            &format!("{:04X}", document.record.tag.package_id),
                        );
                        row(
                            ui,
                            "Entry",
                            &format!("{:04X}", document.record.tag.entry_index),
                        );
                        row(ui, "Class", &document.record.entry.reference);
                        row(
                            ui,
                            "Payload",
                            &format!("{} bytes", document.record.raw_payload.byte_length),
                        );
                    });
                ui.separator();
                ui.horizontal(|ui| {
                    ui.label("Export path:");
                    ui.text_edit_singleline(&mut self.export_path);
                    export_requested = ui.button("Export JSON").clicked();
                });
                if let Some(status) = &self.export_status {
                    ui.weak(status);
                }
                ui.collapsing("Raw payload", |ui| {
                    ui.monospace(format_hex_page(document.bytes(), self.hex_offset, 512));
                    if ui.button("Next page").clicked() {
                        self.hex_offset += 512;
                    }
                });
            }
            Err(error) => {
                ui.colored_label(Color32::DARK_RED, "Tag document could not be loaded");
                ui.label(error);
                if ui.button("Retry").clicked() {
                    self.retry();
                }
            }
        }
        if export_requested {
            if let Some(Ok(document)) = self.document.as_ref() {
                match document.write_json(Path::new(&self.export_path)) {
                    Ok(()) => self.export_status = Some(format!("Exported {}", self.export_path)),
                    Err(error) => self.export_status = Some(format!("Export failed: {error:#}")),
                }
            }
        }
        TabResult::Continue
    }
}

fn row(ui: &mut egui::Ui, name: &str, value: &str) {
    ui.label(RichText::new(name).strong());
    ui.monospace(value);
    ui.end_row();
}

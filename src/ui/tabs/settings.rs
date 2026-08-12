use egui::{FontId, TextStyle, Ui};

use crate::app::SharedState;

pub struct SettingsTab;

impl SettingsTab {
    pub fn ui(ui: &mut Ui, state: &SharedState) {
        ui.style_mut()
            .text_styles
            .insert(TextStyle::Button, FontId::proportional(16.0));

        let mut changed = false;
        let mut config = state.config.write();
        changed |= ui.checkbox(&mut config.vsync, "Enable Vsync").changed();

        let mut limiter_enabled = config.framerate_limit.is_some();
        if ui
            .checkbox(&mut limiter_enabled, "Enable Framelimiter")
            .changed()
        {
            config.framerate_limit =
                limiter_enabled.then(|| std::num::NonZeroU16::new(60).unwrap());
            changed = true;
        }
        ui.spacing_mut().slider_width = 384.0;

        if let Some(limit) = config.framerate_limit {
            let mut value = limit.get();
            if ui
                .add(
                    egui::Slider::new(&mut value, 20..=240)
                        .step_by(10.0)
                        .text("Framerate Limit")
                        .custom_formatter(|value, _| format!("{} FPS", value)),
                )
                .changed()
            {
                config.framerate_limit = std::num::NonZeroU16::new(value);
                changed = true;
            }
        }

        changed |= ui
            .add(
                egui::Slider::new(&mut config.resolution_scale, 0.25..=2.0)
                    .step_by(0.25)
                    .text("Resolution Scale")
                    .custom_formatter(|value, _| format!("{:.0}%", value * 100.0)),
            )
            .changed();
        drop(config);
        if changed && let Err(error) = state.save_config() {
            state
                .startup_notices
                .lock()
                .push(format!("Failed to save settings: {error}"));
        }
    }
}

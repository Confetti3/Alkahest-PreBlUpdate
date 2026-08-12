use std::{collections::BTreeMap, mem::discriminant, rc::Rc, sync::Arc};

use alkahest_render::{Gpu, gpu::command_list::CommandList};
use anyhow::Context;
use egui::{Color32, FontId};
use egui_dock::{DockArea, DockState, TabInteractionStyle, TabPath};
use google_material_symbols::GoogleMaterialSymbols;
use tabs::{DockStateExt, Tab, TabViewer};

use crate::app::SharedState;

pub mod bubble_browser;
pub mod colors;
pub mod icons;
pub mod map_workspace;
mod scene;
mod style;
pub mod tabs;
pub mod util;

pub struct Gui {
    window: Rc<sdl3::video::Window>,
    sdl: Rc<sdl3::Sdl>,

    pub egui_d3d11: egui_d3d11::D3D11Renderer,
    pub egui_sdl3: egui_sdl3_platform::Platform,
    tree: DockState<Tab>,

    added_nodes: Vec<Tab>,
}

impl Gui {
    pub fn new(
        gpu: &Gpu,
        sdl: Rc<sdl3::Sdl>,
        window: Rc<sdl3::video::Window>,
    ) -> anyhow::Result<Self> {
        let mut fonts = egui::FontDefinitions::default();
        fonts.font_data.insert(
            "MaterialSymbolsRounded-Medium".into(),
            Arc::new(egui::FontData::from_static(
                GoogleMaterialSymbols::FONT_BYTES,
            )),
        );
        for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
            fonts
                .families
                .entry(family)
                .or_default()
                .push("MaterialSymbolsRounded-Medium".to_owned());
        }

        let egui_sdl3 =
            egui_sdl3_platform::Platform::new(&sdl, &window, gpu.swapchain_resolution())
                .context("Failed to initialize SDL3 Egui platform module")?;
        egui_sdl3.context().set_fonts(fonts);
        egui_sdl3.context().global_style_mut(|s| {
            *s = style::gui_style();
        });

        // Redefine text_styles
        let text_styles: BTreeMap<_, _> = [
            (
                egui::TextStyle::Heading,
                FontId::new(42.0, egui::FontFamily::Proportional),
            ),
            (
                egui::TextStyle::Body,
                FontId::new(18.0, egui::FontFamily::Proportional),
            ),
            (
                egui::TextStyle::Monospace,
                FontId::new(14.0, egui::FontFamily::Monospace),
            ),
            (
                egui::TextStyle::Button,
                FontId::new(24.0, egui::FontFamily::Proportional),
            ),
            (
                egui::TextStyle::Small,
                FontId::new(10.0, egui::FontFamily::Proportional),
            ),
        ]
        .into();

        // Mutate global styles with new text styles
        egui_sdl3
            .context()
            .all_styles_mut(move |style| style.text_styles = text_styles.clone());

        egui_extras::install_image_loaders(egui_sdl3.context());

        let mut tree = DockState::new(vec![Tab::Settings, Tab::Home]);
        if let Some((surface, node, tab)) = tree.find_tab(|t| matches!(t, Tab::Home)) {
            tree.set_active_tab(TabPath::new(surface, node, tab))
                .expect("Home tab path came from the same dock tree");
        }

        Ok(Self {
            window,
            sdl,
            egui_d3d11: egui_d3d11::D3D11Renderer::new(gpu)
                .context("Failed to initialize D3D11 Egui renderer")?,
            egui_sdl3,
            tree,
            added_nodes: Vec::new(),
        })
    }

    pub fn add_tab(&mut self, tab: Tab) {
        self.added_nodes.push(tab);
    }

    pub fn draw(&mut self, cmd: &mut CommandList, shared_state: &Arc<SharedState>) {
        let (ctx, raw_input) = self
            .egui_sdl3
            .begin_frame(self.window.size(), self.window.display_scale());
        ctx.global_style_mut(|s| s.visuals.panel_fill = Color32::from_black_alpha(96));

        let output = ctx.run_ui(raw_input, |ui| {
            DockArea::new(&mut self.tree)
                .show_add_buttons(false)
                .style({
                    let mut style = egui_dock::Style::from_egui(ctx.global_style().as_ref());
                    // style.tab_bar.fill_tab_bar = true;
                    style.tab_bar.height = 32.0;
                    style.tab_bar.bg_fill = Color32::from_gray(4);

                    let inactive = TabInteractionStyle {
                        outline_color: Color32::TRANSPARENT,
                        corner_radius: egui::CornerRadius::ZERO,
                        bg_fill: Color32::BLACK,
                        text_color: Color32::WHITE,
                    };

                    let hovered = TabInteractionStyle {
                        outline_color: Color32::from_gray(127),
                        bg_fill: ctx.global_style().visuals.window_fill().gamma_multiply(0.5),
                        ..inactive.clone()
                    };

                    let active = TabInteractionStyle {
                        bg_fill: ctx.global_style().visuals.window_fill(),
                        ..hovered.clone()
                    };

                    let focused = TabInteractionStyle {
                        outline_color: Color32::WHITE,
                        bg_fill: ctx.global_style().visuals.window_fill(),
                        ..inactive.clone()
                    };

                    style.tab = egui_dock::TabStyle {
                        active: active.clone(),
                        inactive: inactive.clone(),
                        focused: focused.clone(),
                        hovered: hovered.clone(),
                        inactive_with_kb_focus: inactive.clone(),
                        active_with_kb_focus: active.clone(),
                        focused_with_kb_focus: focused.clone(),
                        tab_body: egui_dock::TabBodyStyle {
                            inner_margin: ctx.global_style().spacing.window_margin,
                            stroke: ctx.global_style().visuals.widgets.noninteractive.bg_stroke,
                            corner_radius: ctx.global_style().visuals.widgets.active.corner_radius,
                            bg_fill: ctx.global_style().visuals.window_fill(),
                            // bg_fill: Color32::from_black_alpha(128),
                        },
                        hline_below_active_tab_name: false,
                        ..Default::default()
                    };
                    style
                })
                .show_leaf_collapse_buttons(false)
                .show_leaf_close_all_buttons(false)
                .draggable_tabs(false)
                .show_inside(
                    ui,
                    &mut TabViewer {
                        added_nodes: &mut self.added_nodes,
                        egui_d3d11: &mut self.egui_d3d11,
                        shared_state,
                    },
                );
        });

        for tab in self.added_nodes.drain(..) {
            // Is the tab unique and does it already exist? Then switch to it instead of adding it again.
            if let Some((surface, node, tab)) = self
                .tree
                .find_tab(|t| discriminant(t) == discriminant(&tab) && t.key() == tab.key())
            {
                if let Err(error) = self.tree.set_active_tab(TabPath { surface, node, tab }) {
                    warn!(?error, "Failed to focus existing tab");
                }
            } else {
                self.tree.push_to_focused_leaf(tab);
            }
        }

        let output = self
            .egui_sdl3
            .end_frame(&mut self.sdl.video().unwrap(), output)
            .unwrap();
        if let Err(e) = self.egui_d3d11.paint(cmd, output, &ctx) {
            error!("Failed to paint gui: {}", e);
        }
    }
}

use std::{
    collections::BTreeMap,
    io::{Cursor, Seek, SeekFrom},
    str::FromStr,
    sync::{Arc, atomic::AtomicUsize, mpsc::Receiver},
};

use alkahest_data::{
    pattern::{SComponent, SPattern},
    tfx::sequencer::{NodeKind, SSequence, SSequenceNodeRef, SUnk808091f1Variant},
};
use anyhow::Result;
use egui::{
    AtomExt, Color32, FontId, ImageSource, RichText, TextStyle, Ui, Vec2, Widget, WidgetText,
    include_image, scroll_area::ScrollSource,
};
use rayon::iter::{IntoParallelRefIterator, ParallelIterator};
use tiger_parse::{PackageManagerExt, TigerReadable};
use tiger_pkg::{TagHash, package_manager};

use crate::{
    app::SharedState,
    ui::{icons, tabs::TabResult},
};

pub struct SequenceListTab {
    package_sorting: PackageSorting,
    package_ids: Vec<u16>,

    current_package: u16,
    current_tag: TagHash,

    filter: String,

    provider: SequenceProvider,

    loaded_sequences: BTreeMap<TagHash, Result<SSequence>>,

    tmp_sequence: SSequence,
}

impl SequenceListTab {
    pub fn new(shared: &SharedState) -> Self {
        let package_sorting = PackageSorting::Name;
        let provider = SequenceProvider::new();
        let mut package_ids = provider.package_keys().to_vec();
        package_sorting.sort_package_ids(&provider, &mut package_ids);

        Self {
            package_sorting,
            package_ids,
            current_package: 0,
            current_tag: TagHash::NONE,

            filter: String::new(),
            provider,
            loaded_sequences: BTreeMap::default(),

            tmp_sequence: load_sequence(0x80D8300D.into()).expect("asdf"),
        }
    }

    pub fn ui(&mut self, ui: &mut Ui) -> TabResult {
        subsecond::call(|| self.ui_inner(ui))
    }

    fn ui_inner(&mut self, ui: &mut Ui) -> TabResult {
        self.provider.update();
        if self.provider.package_keys().len() != self.package_ids.len() {
            self.package_ids = self.provider.package_keys().to_vec();
            self.package_sorting
                .sort_package_ids(&self.provider, &mut self.package_ids);
        }

        ui.separator();
        ui.style_mut()
            .text_styles
            .insert(TextStyle::Button, FontId::proportional(16.0));
        ui.style_mut().spacing.button_padding = Vec2::new(8.0, 4.0);

        let (filter_hash, filter_valid) = match TagHash::from_str(&self.filter) {
            Ok(o) => (Some(o), true),
            Err(_) => (None, false),
        };

        egui::SidePanel::left("sequence_packages_list").show_inside(ui, |ui| {
            if let Some(status) = self.provider.load_status() {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(status);
                });
            }

            egui::TextEdit::singleline(&mut self.filter)
                .text_color_opt((!filter_valid).then_some(Color32::RED))
                .hint_text(
                    RichText::new("Search by Hash (8XXXXXXX)...")
                        .color(Color32::GRAY)
                        .italics(),
                )
                .ui(ui);

            ui.horizontal(|ui| {
                ui.label(RichText::new("Sort by:").color(Color32::GRAY));
                egui::ComboBox::new("sorting_mode", "")
                    .selected_text(format!("{:?}", self.package_sorting))
                    .show_ui(ui, |ui| {
                        ui.style_mut()
                            .text_styles
                            .insert(TextStyle::Button, FontId::proportional(16.0));

                        let mut clicked = ui
                            .selectable_value(&mut self.package_sorting, PackageSorting::Id, "Id")
                            .clicked();
                        clicked |= ui
                            .selectable_value(
                                &mut self.package_sorting,
                                PackageSorting::Name,
                                "Name",
                            )
                            .clicked();
                        clicked |= ui
                            .selectable_value(
                                &mut self.package_sorting,
                                PackageSorting::Count,
                                "Count",
                            )
                            .clicked();

                        if clicked {
                            (self.package_sorting)
                                .sort_package_ids(&self.provider, &mut self.package_ids);
                        }
                    });
            });

            egui::ScrollArea::vertical()
                .auto_shrink([false; 2])
                .show(ui, |ui| {
                    let mut load_package = None;
                    ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Extend);

                    for pkg_id in self.package_ids.iter() {
                        if let Some(hash) = filter_hash
                            && *pkg_id != hash.pkg_id()
                        {
                            continue;
                        }

                        let path = &package_manager().package_paths[pkg_id];
                        if ui
                            .selectable_label(
                                *pkg_id == self.current_package,
                                (
                                    RichText::new(format!("{pkg_id:04x} -")).weak().italics(),
                                    path.name.clone(),
                                    RichText::new(format!(
                                        "({})",
                                        self.provider.num_sequences(*pkg_id)
                                    ))
                                    .weak()
                                    .italics()
                                    .small(),
                                ),
                            )
                            .clicked()
                        {
                            self.current_package = *pkg_id;
                            load_package = Some(*pkg_id);
                        }
                    }

                    if let Some(components) =
                        load_package.and_then(|pkg_id| self.provider.package(pkg_id))
                    {
                        self.loaded_sequences.clear();
                        for c in components {
                            let res = load_sequence(c.component);
                            if let Err(e) = &res {
                                error!(
                                    "Failed to load sequence {} (pattern {}): {e:?}",
                                    c.component, c.pattern
                                );
                            }
                            self.loaded_sequences
                                .insert(c.pattern, load_sequence(c.component));
                        }
                    }
                });
        });

        egui::SidePanel::right("sequence_viewer")
            .default_width(ui.ctx().content_rect().width() * 0.5)
            .show_inside(ui, |ui| {
                self.draw_sequence_node_tree(
                    ui,
                    &self.tmp_sequence,
                    &SSequenceNodeRef {
                        kind: NodeKind::Flow,
                        index: 0,
                    },
                );
            });

        egui::CentralPanel::default().show_inside(ui, |ui| {
            egui::ScrollArea::vertical()
                .auto_shrink([false; 2])
                .scroll_source(ScrollSource::MOUSE_WHEEL | ScrollSource::SCROLL_BAR)
                .show(ui, |ui| {
                    let Some(pkg) = self.provider.package(self.current_package) else {
                        return;
                    };

                    for r in pkg {
                        if let Some(hash) = filter_hash
                            && r.pattern != hash
                        {
                            continue;
                        }

                        if ui
                            .selectable_label(r.pattern == self.current_tag, r.pattern.to_string())
                            .clicked()
                        {
                            self.current_tag = r.pattern;
                        }
                    }
                });
        });

        TabResult::Continue
    }

    fn draw_sequence_node_tree(&self, ui: &mut Ui, seq: &SSequence, node_ref: &SSequenceNodeRef) {
        let node = match node_ref.kind {
            NodeKind::Flow => &seq.m_flow_nodes[node_ref.index as usize],
            NodeKind::Work => &seq.m_work_nodes[node_ref.index as usize],
        };

        match &*node.unk18 {
            SUnk808091f1Variant::SSequenceGlobalChannel(_) => {
                ui.label("[TODO: SSequenceGlobalChannel]");
            }
            SUnk808091f1Variant::SSequenceFlowParallel(p) => {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing = Vec2::new(4.0, 0.0);
                    egui::Image::new(icons::sequencer::FLOW_PARALLEL)
                        .fit_to_exact_size(Vec2::splat(32.0))
                        .ui(ui);
                    ui.label("Parallel");
                });

                for child in &p.children {
                    self.draw_sequence_node_tree(ui, seq, child);
                }
            }
            SUnk808091f1Variant::SUnk808091df(_) => {
                ui.label("[TODO: SUnk808091df]");
            }
            SUnk808091f1Variant::SUnk808091e5(_) => {
                ui.label("[TODO: SUnk808091e5]");
            }
            SUnk808091f1Variant::SUnk808091db(_) => {
                ui.label("[TODO: SUnk808091db]");
            }
            SUnk808091f1Variant::SUnk808091dd(_) => {
                ui.label("[TODO: SUnk808091dd]");
            }
            SUnk808091f1Variant::SSequenceScreenAreaFx(_) => {
                ui.label("[TODO: SSequenceScreenAreaFx]");
            }
            SUnk808091f1Variant::SSequenceLight(_) => {
                ui.label("[TODO: SSequenceLight]");
            }
            SUnk808091f1Variant::SSequenceLensFlare(_) => {
                ui.label("[TODO: SSequenceLensFlare]");
            }
            SUnk808091f1Variant::SSequenceEmbeddedParticleSystem(_) => {
                ui.label("[TODO: SSequenceEmbeddedParticleSystem]");
            }
            SUnk808091f1Variant::SSequenceAudioEvent(_) => {
                ui.label("[TODO: SSequenceAudioEvent]");
            }
            SUnk808091f1Variant::Unknown { class, .. } => {
                ui.label(format!("[TODO: 0x{class:08X}]"));
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PackageSorting {
    Id,
    Name,
    Count,
}

impl PackageSorting {
    fn sort_package_ids(&self, provider: &SequenceProvider, packages: &mut [u16]) {
        match self {
            PackageSorting::Id => packages.sort_by_key(|id| *id),
            PackageSorting::Name => packages
                .sort_by_cached_key(|id| (package_manager().package_paths[id].name.clone(), *id)),
            PackageSorting::Count => {
                packages.sort_by_cached_key(|id| provider.num_sequences(*id));
                packages.reverse();
            }
        }
    }
}

pub struct SequenceProvider {
    package_keys: Vec<u16>,
    packages: BTreeMap<u16, Vec<ComponentRef>>,
    packages_left: Arc<AtomicUsize>,

    package_rx: Receiver<(u16, Vec<ComponentRef>)>,
}

impl SequenceProvider {
    fn new() -> Self {
        let (package_tx, package_rx) = std::sync::mpsc::channel();

        let packages_left = Arc::new(AtomicUsize::new(package_manager().package_paths.len()));

        let packages_left_clone = packages_left.clone();
        std::thread::spawn(move || {
            package_manager()
                .package_paths
                .par_iter()
                .for_each(|(pkg_id, _)| {
                    let entries = get_entities_with_sequence(*pkg_id);
                    if !entries.is_empty() {
                        let _ = package_tx.send((*pkg_id, entries));
                    }
                    packages_left_clone.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
                });
        });

        Self {
            package_keys: Default::default(),
            packages: Default::default(),
            packages_left,
            package_rx,
        }
    }

    fn update(&mut self) {
        while let Ok((pkg_id, entries)) = self.package_rx.try_recv() {
            self.packages.insert(pkg_id, entries);
            self.package_keys.push(pkg_id);
        }
    }

    fn load_status(&self) -> Option<String> {
        let left = self.packages_left.load(std::sync::atomic::Ordering::SeqCst);
        if left == 0 {
            None
        } else {
            Some(format!("Loading {left} packages..."))
        }
    }

    fn package_keys(&self) -> &[u16] {
        &self.package_keys
    }

    fn package(&self, pkg_id: u16) -> Option<&[ComponentRef]> {
        self.packages.get(&pkg_id).map(|entries| entries.as_slice())
    }

    fn num_sequences(&self, pkg_id: u16) -> usize {
        self.packages
            .get(&pkg_id)
            .map_or(0, |entries| entries.len())
    }
}

fn get_entities_with_sequence(pkg_id: u16) -> Vec<ComponentRef> {
    let Some(entries) = package_manager()
        .lookup
        .tag32_entries_by_pkg
        .get(&pkg_id)
        .cloned()
    else {
        return vec![];
    };

    let mut entities = vec![];
    for (i, _entry) in entries
        .into_iter()
        .enumerate()
        .filter(|(_, e)| Some(e.reference) == SPattern::ID)
    {
        let tag = TagHash::new(pkg_id, i as u16);
        let Ok(pattern) = package_manager().read_tag_struct::<SPattern>(tag) else {
            continue;
        };

        for c in pattern.m_interface_map.m_accessor_list {
            if c.class_id == 0x80809479 {
                entities.push(ComponentRef {
                    pattern: tag,
                    component: c.component,
                });
                break;
            }
        }
    }

    entities
}

struct ComponentRef {
    pattern: TagHash,
    component: TagHash,
}

fn load_sequence(component_handle: TagHash) -> Result<SSequence> {
    let data = package_manager().read_tag(component_handle)?;
    let mut f = Cursor::new(data);
    let component: SComponent = TigerReadable::read_ds(&mut f)?;

    if component.default_instance.resource_type != 0x80809479 {
        anyhow::bail!(
            "Invalid component type (expected 0x80809479, got {:X})",
            component.default_instance.resource_type
        );
    }

    f.seek(SeekFrom::Start(component.definition.offset))?;

    Ok(SSequence::read_ds(&mut f)?)
}

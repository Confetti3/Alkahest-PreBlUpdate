use std::{
    collections::BTreeMap,
    io::{Cursor, Seek, SeekFrom},
    str::FromStr,
    sync::{Arc, atomic::AtomicUsize, mpsc::Receiver},
};

use alkahest_data::{
    hash::FNV1_BASE,
    pattern::{SComponent, SPattern},
    tfx::sequencer::{NodeKind, SSequence, SSequenceNodeRef, SUnk808091f1Variant},
};
use anyhow::Result;
use egui::{
    Color32, FontId, RichText, TextStyle, Ui, Vec2, Widget, scroll_area::ScrollSource,
};
use egui_ltreeview::{NodeBuilder, TreeView, TreeViewBuilder};
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

    shared: Arc<SharedState>,
}

impl SequenceListTab {
    pub fn new(shared: Arc<SharedState>) -> Self {
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

            tmp_sequence: load_sequence(0x80C02107.into()).expect("asdf"),

            shared,
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

        egui::Panel::left("sequence_packages_list").show(ui, |ui| {
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

        egui::Panel::right("sequence_viewer")
            // .default_width(ui.ctx().content_rect().width() * 0.5)
            .show(ui, |ui| {
                egui::ScrollArea::vertical().auto_shrink([false; 2]).show(ui, |ui| {
                    TreeView::new("sequence nodes".into()).override_striped(Some(true)).show(ui, |builder| {
                        builder.dir(SSequenceNodeRef { kind: NodeKind::Flow, index: u16::MAX }, RichText::new("NODES").underline());
                        self.draw_sequence_node_tree(
                            builder,
                            &self.tmp_sequence,
                            &SSequenceNodeRef {
                                kind: NodeKind::Flow,
                                index: 0,
                            },
                        );
                        builder.close_dir();
                    });
                });
            });

        egui::CentralPanel::default().show(ui, |ui| {
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

                        let label = if let Some(Ok(seq)) = self.loaded_sequences.get(&r.pattern) {
                            format!(
                                "{} ({}f{}w)",
                                r.pattern,
                                seq.m_flow_nodes.len(),
                                seq.m_work_nodes.len()
                            )
                        } else {
                            r.pattern.to_string()
                        };

                        let label = ui.selectable_label(r.pattern == self.current_tag, label);

                        label.context_menu(|ui| {
                            ui.style_mut()
                                .text_styles
                                .insert(TextStyle::Button, FontId::proportional(16.0));
                            if ui.button("Copy tag").clicked() {
                                ui.ctx().copy_text(r.pattern.to_string());
                                ui.close();
                            }
                        });

                        if label.clicked() {
                            self.current_tag = r.pattern;
                            if let Some(Ok(seq)) = self.loaded_sequences.get(&r.pattern) {
                                self.tmp_sequence = seq.clone();
                            }
                        }
                    }
                });
        });

        TabResult::Continue
    }

    fn draw_sequence_node_tree(
        &self,
        tree: &mut TreeViewBuilder<SSequenceNodeRef>,
        seq: &SSequence,
        node_ref: &SSequenceNodeRef,
    ) {
        let node = match node_ref.kind {
            NodeKind::Flow => &seq.m_flow_nodes[node_ref.index as usize],
            NodeKind::Work => &seq.m_work_nodes[node_ref.index as usize],
        };

        macro_rules! node {
            ($kind:ident, $label:expr, $icon:expr) => {
                tree.node(NodeBuilder::$kind(*node_ref).label_ui(|ui| {
                    ui.spacing_mut().item_spacing.x = 4.0;
                    egui::Image::new($icon)
                        .fit_to_exact_size(Vec2::splat(24.0))
                        .ui(ui);

                    if let Some(base) = node.owner_pointer.base()
                        && base.name != FNV1_BASE
                    {
                        ui.label(self.shared.get_wordlist_string(base.name));
                        ui.weak(RichText::new($label).italics());
                    } else {
                        ui.label($label);
                    }
                }));
            };
        }

        match &*node.owner_pointer {
            SUnk808091f1Variant::SSequenceGlobalChannel(_) => {
                node!(
                    leaf,
                    "[TODO: SSequenceGlobalChannel]",
                    icons::sequencer::UNKNOWN
                );
            }
            SUnk808091f1Variant::SSequenceFlowParallel(p) => {
                node!(dir, "Parallel", icons::sequencer::FLOW_PARALLEL);
                for child in &p.children {
                    self.draw_sequence_node_tree(tree, seq, child);
                }
                tree.close_dir();
            }
            SUnk808091f1Variant::SUnk808091df(f) => {
                node!(dir, "[TODO: SUnk808091df]", icons::sequencer::FLOW_UNKNOWN);
                for child in &f.children {
                    self.draw_sequence_node_tree(tree, seq, child);
                }
                tree.close_dir();
            }
            SUnk808091f1Variant::SUnk808091e1(f) => {
                node!(dir, "[TODO: SUnk808091e1]", icons::sequencer::FLOW_UNKNOWN);
                for child in &f.children {
                    self.draw_sequence_node_tree(tree, seq, child);
                }
                tree.close_dir();
            }
            SUnk808091f1Variant::SUnk808091e5(f) => {
                node!(dir, "[TODO: SUnk808091e5]", icons::sequencer::FLOW_SERIAL);
                for child in &f.children {
                    self.draw_sequence_node_tree(tree, seq, child);
                }
                tree.close_dir();
            }
            SUnk808091f1Variant::SUnk808091db(f) => {
                node!(dir, "[TODO: SUnk808091db]", icons::sequencer::FLOW_UNKNOWN);
                for child in &f.children {
                    self.draw_sequence_node_tree(tree, seq, child);
                }
                tree.close_dir();
            }
            SUnk808091f1Variant::SUnk808091dd(f) => {
                node!(dir, "[TODO: SUnk808091dd]", icons::sequencer::FLOW_UNKNOWN);
                for child in &f.children {
                    self.draw_sequence_node_tree(tree, seq, child);
                }
                tree.close_dir();
            }
            SUnk808091f1Variant::SUnk808091d9(f) => {
                node!(dir, "[TODO: SUnk808091d9]", icons::sequencer::FLOW_UNKNOWN);
                for child in &f.children {
                    self.draw_sequence_node_tree(tree, seq, child);
                }
                tree.close_dir();
            }
            SUnk808091f1Variant::SSequenceDelay(_) => {
                node!(leaf, "Delay", icons::sequencer::DELAY);
            }
            SUnk808091f1Variant::SSequenceScreenAreaFx(_) => {
                node!(leaf, "ScreenAreaFx", icons::sequencer::FX);
            }
            SUnk808091f1Variant::SSequenceLight(_) => {
                node!(leaf, "Light", icons::sequencer::LIGHT);
            }
            SUnk808091f1Variant::SSequenceLensFlare(_) => {
                node!(leaf, "SSequenceLensFlare", icons::sequencer::UNKNOWN);
            }
            SUnk808091f1Variant::SSequenceEmbeddedParticleSystem(p) => {
                if p.unk28
                    .iter()
                    .any(|u| u.particle_system.as_ref().map_or_default(|s| s.is_gpu()))
                {
                    node!(
                        leaf,
                        "ParticleSystemGpu",
                        icons::sequencer::PARTICLE_SYSTEM_GPU
                    );
                } else {
                    node!(leaf, "ParticleSystem", icons::sequencer::PARTICLE_SYSTEM);
                }
            }
            SUnk808091f1Variant::SSequenceAudioEvent(_) => {
                node!(leaf, "AudioEvent", icons::sequencer::AUDIO);
            }
            SUnk808091f1Variant::SUnk80802636Animation(_) => {
                node!(leaf, "(Unknown)Animation", icons::sequencer::ANIMATION);
            }
            SUnk808091f1Variant::SSequenceDamageImpulse(_) => {
                node!(leaf, "DamageImpulse", icons::sequencer::DAMAGE_IMPULSE);
            }
            SUnk808091f1Variant::SSequenceAreaImpulse(_) => {
                node!(leaf, "AreaImpulse", icons::sequencer::AREA_IMPULSE);
            }
            SUnk808091f1Variant::Unknown { class, .. } => {
                node!(
                    leaf,
                    format!("[TODO: 0x{class:08X}]"),
                    icons::sequencer::UNKNOWN
                );
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

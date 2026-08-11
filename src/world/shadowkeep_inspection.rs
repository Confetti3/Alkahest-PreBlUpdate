//! Indexed, renderer-independent Shadowkeep map inspection graph.
//!
//! The graph records authored provenance even when a resource cannot create a
//! renderer object. ECS bindings are deliberately components owned here so the
//! UI, world extraction, and loader share one stable selection identity.

use std::collections::{BTreeMap, BTreeSet, HashSet};

use alkahest_data::tfx::common::AxisAlignedBBox;
use anyhow::Context;
use bitflags::bitflags;
use tiger_pkg::TagHash;

use crate::world::transform::Transform;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MapInspectionNodeId(pub u32);

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum MapInspectionNodeKind {
    Bubble,
    BaseContainer,
    Scenario,
    Table,
    Entry,
    StaticGeometry,
    StaticInstance,
    Terrain,
    RigidModel,
    DynamicModel,
    LightCollection,
    Light,
    ShadowingLight,
    Cubemap,
    Atmosphere,
    SkyCollection,
    SkyObject,
    SpawnPoint,
    EntityResource,
    TableResource,
    DeferredResource,
    FailedResource,
    MetadataOnly,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MapInspectionDisposition {
    Rendering,
    NonRendering,
    Deferred,
    Failed,
}

impl MapInspectionDisposition {
    pub const fn status_label(self) -> &'static str {
        match self {
            Self::Rendering => "Rendering",
            Self::NonRendering => "Non-rendering",
            Self::Deferred => "Deferred",
            Self::Failed => "Failed",
        }
    }
}

impl MapInspectionNodeKind {
    pub const fn is_visual_owner(self) -> bool {
        matches!(
            self,
            Self::StaticGeometry
                | Self::Terrain
                | Self::RigidModel
                | Self::DynamicModel
                | Self::Light
                | Self::ShadowingLight
                | Self::Cubemap
                | Self::Atmosphere
                | Self::SkyObject
        )
    }

    pub const fn type_group(self) -> MapInspectionTypeFilter {
        match self {
            Self::StaticGeometry
            | Self::StaticInstance
            | Self::Terrain
            | Self::RigidModel
            | Self::DynamicModel => MapInspectionTypeFilter::GEOMETRY,
            Self::LightCollection | Self::Light | Self::ShadowingLight => {
                MapInspectionTypeFilter::LIGHTS
            }
            Self::Cubemap | Self::Atmosphere | Self::SkyCollection | Self::SkyObject => {
                MapInspectionTypeFilter::ENVIRONMENT
            }
            Self::SpawnPoint => MapInspectionTypeFilter::SPAWNS,
            Self::DeferredResource | Self::FailedResource => MapInspectionTypeFilter::DEFERRED,
            _ => MapInspectionTypeFilter::METADATA,
        }
    }

    pub const fn is_visual_locator(self) -> bool {
        self.is_visual_owner()
            || matches!(
                self,
                Self::StaticInstance
                    | Self::SpawnPoint
                    | Self::DeferredResource
                    | Self::FailedResource
            )
    }
}

bitflags! {
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct MapInspectionTypeFilter: u8 {
        const GEOMETRY = 1 << 0;
        const LIGHTS = 1 << 1;
        const ENVIRONMENT = 1 << 2;
        const SPAWNS = 1 << 3;
        const DEFERRED = 1 << 4;
        const METADATA = 1 << 5;
        const ALL = Self::GEOMETRY.bits() | Self::LIGHTS.bits() | Self::ENVIRONMENT.bits()
            | Self::SPAWNS.bits() | Self::DEFERRED.bits() | Self::METADATA.bits();
    }
}

bitflags! {
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct MapInspectionDispositionFilter: u8 {
        const RENDERING = 1 << 0;
        const NON_RENDERING = 1 << 1;
        const DEFERRED = 1 << 2;
        const FAILED = 1 << 3;
        const ALL = Self::RENDERING.bits() | Self::NON_RENDERING.bits()
            | Self::DEFERRED.bits() | Self::FAILED.bits();
    }
}

bitflags! {
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct MapInspectionSourceFilter: u8 {
        const BASE = 1 << 0;
        const SCENARIO = 1 << 1;
        const BOTH = Self::BASE.bits() | Self::SCENARIO.bits();
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MapInspectionFilter {
    pub types: MapInspectionTypeFilter,
    pub dispositions: MapInspectionDispositionFilter,
    pub sources: MapInspectionSourceFilter,
}

impl Default for MapInspectionFilter {
    fn default() -> Self {
        Self {
            types: MapInspectionTypeFilter::ALL,
            dispositions: MapInspectionDispositionFilter::ALL,
            sources: MapInspectionSourceFilter::BOTH,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ShadowkeepTableSources {
    pub base_containers: BTreeSet<TagHash>,
    /// Package decoding retains only a proven scenario relationship, not the
    /// discarded scenario wrapper chain.
    pub referenced_by_freeroam_scenario: bool,
    pub scenario: Option<TagHash>,
}

impl ShadowkeepTableSources {
    pub fn is_base(&self) -> bool {
        !self.base_containers.is_empty()
    }

    pub const fn is_scenario(&self) -> bool {
        self.referenced_by_freeroam_scenario || self.scenario.is_some()
    }

    pub fn label(&self) -> &'static str {
        match (self.is_base(), self.is_scenario()) {
            (true, true) => "base bubble + freeroam scenario",
            (true, false) => "base bubble",
            (false, true) => "freeroam scenario",
            (false, false) => "unknown",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum MapInspectionSourceGroup {
    BaseContainer(TagHash),
    Scenario(TagHash),
}

#[derive(Clone, Debug)]
pub struct MapInspectionNode {
    pub id: MapInspectionNodeId,
    pub parent: Option<MapInspectionNodeId>,
    pub children: Vec<MapInspectionNodeId>,
    pub kind: MapInspectionNodeKind,
    pub disposition: MapInspectionDisposition,
    pub label: String,
    pub tag: Option<TagHash>,
    pub class: Option<u32>,
    pub entry_index: Option<usize>,
    pub element_index: Option<usize>,
    pub world_id: Option<u64>,
    pub definition_offset: Option<u64>,
    pub transform: Option<Transform>,
    pub bounds: Option<AxisAlignedBBox>,
    pub visual_owner: Option<MapInspectionNodeId>,
    pub world_entity: Option<hecs::Entity>,
    pub source: ShadowkeepTableSources,
    pub name_hash: Option<u32>,
    pub linked_node: Option<MapInspectionNodeId>,
    /// Activity entity definition whose serialized WorldID identifies this placement.
    pub activity_definition: Option<TagHash>,
    /// Byte offset of the exact serialized WorldID reference in that definition.
    pub activity_reference_offset: Option<u64>,
    /// Number of same-scenario spawn-rule definitions correlated to this placement.
    pub activity_reference_count: Option<usize>,
    pub error: Option<String>,
    pub search_text: String,
}

impl MapInspectionNode {
    pub fn new(
        kind: MapInspectionNodeKind,
        disposition: MapInspectionDisposition,
        label: impl Into<String>,
        source: ShadowkeepTableSources,
    ) -> Self {
        Self {
            id: MapInspectionNodeId(u32::MAX),
            parent: None,
            children: Vec::new(),
            kind,
            disposition,
            label: label.into(),
            tag: None,
            class: None,
            entry_index: None,
            element_index: None,
            world_id: None,
            definition_offset: None,
            transform: None,
            bounds: None,
            visual_owner: None,
            world_entity: None,
            source,
            name_hash: None,
            linked_node: None,
            activity_definition: None,
            activity_reference_offset: None,
            activity_reference_count: None,
            error: None,
            search_text: String::new(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct MapInspectionSourceGroupIndex {
    pub node: MapInspectionNodeId,
    pub tables: Vec<MapInspectionNodeId>,
}

#[derive(Clone, Debug)]
pub struct ShadowkeepMapInspection {
    pub bubble: TagHash,
    pub child_map: TagHash,
    pub root: MapInspectionNodeId,
    pub nodes: Vec<MapInspectionNode>,
    pub node_index: Vec<Option<usize>>,
    pub world_groups: BTreeMap<MapInspectionNodeKind, Vec<MapInspectionNodeId>>,
    pub source_groups: BTreeMap<MapInspectionSourceGroup, MapInspectionSourceGroupIndex>,
    pub spawn_nodes: Vec<MapInspectionNodeId>,
    pub(crate) locator_nodes: Vec<MapInspectionNodeId>,
}

impl Default for ShadowkeepMapInspection {
    fn default() -> Self {
        Self {
            bubble: TagHash::NONE,
            child_map: TagHash::NONE,
            root: MapInspectionNodeId(0),
            nodes: Vec::new(),
            node_index: Vec::new(),
            world_groups: BTreeMap::new(),
            source_groups: BTreeMap::new(),
            spawn_nodes: Vec::new(),
            locator_nodes: Vec::new(),
        }
    }
}

impl ShadowkeepMapInspection {
    pub fn node(&self, id: MapInspectionNodeId) -> Option<&MapInspectionNode> {
        self.node_index
            .get(id.0 as usize)
            .and_then(|index| index.and_then(|index| self.nodes.get(index)))
    }

    pub fn node_mut(&mut self, id: MapInspectionNodeId) -> Option<&mut MapInspectionNode> {
        let index = self
            .node_index
            .get(id.0 as usize)
            .and_then(|index| *index)?;
        self.nodes.get_mut(index)
    }

    pub fn locator_nodes(&self) -> &[MapInspectionNodeId] {
        &self.locator_nodes
    }

    pub fn visual_owner(&self, id: MapInspectionNodeId) -> Option<MapInspectionNodeId> {
        let node = self.node(id)?;
        if node.kind.is_visual_owner() {
            Some(id)
        } else {
            let owner = node.visual_owner?;
            self.node(owner)
                .is_some_and(|owner| owner.kind.is_visual_owner())
                .then_some(owner)
        }
    }

    pub fn bind_world_entity(
        &mut self,
        id: MapInspectionNodeId,
        entity: hecs::Entity,
    ) -> anyhow::Result<()> {
        let node = self.node_mut(id).context("inspection node is missing")?;
        anyhow::ensure!(
            node.world_entity.is_none(),
            "inspection node {id:?} is already bound"
        );
        node.world_entity = Some(entity);
        Ok(())
    }

    pub fn descendants(
        &self,
        id: MapInspectionNodeId,
    ) -> impl Iterator<Item = MapInspectionNodeId> + '_ {
        let mut pending = self
            .node(id)
            .map(|node| node.children.iter().rev().copied().collect::<Vec<_>>())
            .unwrap_or_default();
        std::iter::from_fn(move || {
            let id = pending.pop()?;
            if let Some(node) = self.node(id) {
                pending.extend(node.children.iter().rev().copied());
            }
            Some(id)
        })
    }

    pub fn ancestors(
        &self,
        id: MapInspectionNodeId,
    ) -> impl Iterator<Item = MapInspectionNodeId> + '_ {
        let mut next = self.node(id).and_then(|node| node.parent);
        std::iter::from_fn(move || {
            let id = next?;
            next = self.node(id).and_then(|node| node.parent);
            Some(id)
        })
    }

    pub fn breadcrumb(&self, id: MapInspectionNodeId) -> String {
        let mut labels = self
            .ancestors(id)
            .filter_map(|ancestor| self.node(ancestor).map(|node| node.label.as_str()))
            .collect::<Vec<_>>();
        labels.reverse();
        if let Some(node) = self.node(id) {
            labels.push(node.label.as_str());
        }
        labels.join(" / ")
    }

    pub fn search(
        &self,
        normalized_query: &str,
        filter: MapInspectionFilter,
    ) -> Vec<MapInspectionNodeId> {
        let query = normalize_search(normalized_query);
        self.nodes
            .iter()
            .filter(|node| node_matches_filter(node, filter))
            .filter(|node| query.is_empty() || node.search_text.contains(&query))
            .map(|node| node.id)
            .collect()
    }

    pub fn validate(&self, world: &hecs::World) -> anyhow::Result<()> {
        anyhow::ensure!(!self.nodes.is_empty(), "inspection graph has no root");
        anyhow::ensure!(self.node(self.root).is_some(), "inspection root is missing");
        anyhow::ensure!(
            self.node(self.root).and_then(|node| node.parent).is_none(),
            "inspection root has a parent"
        );
        anyhow::ensure!(
            self.node_index.len() >= self.nodes.len(),
            "node index is incomplete"
        );

        let mut ids = HashSet::new();
        for (index, node) in self.nodes.iter().enumerate() {
            anyhow::ensure!(
                ids.insert(node.id),
                "duplicate inspection node id {:#?}",
                node.id
            );
            anyhow::ensure!(
                self.node_index.get(node.id.0 as usize).and_then(|id| *id) == Some(index),
                "node index does not point to {:#?}",
                node.id
            );
            if node.id == self.root {
                anyhow::ensure!(node.parent.is_none(), "root has parent");
            } else {
                let parent = node
                    .parent
                    .context("non-root inspection node has no parent")?;
                let parent_node = self
                    .node(parent)
                    .context("inspection node parent is missing")?;
                anyhow::ensure!(
                    parent_node
                        .children
                        .iter()
                        .filter(|child| **child == node.id)
                        .count()
                        == 1,
                    "parent-child edge is not unique and reciprocal"
                );
            }
            let unique = node.children.iter().copied().collect::<HashSet<_>>();
            anyhow::ensure!(unique.len() == node.children.len(), "duplicate child edge");
            for child in &node.children {
                anyhow::ensure!(
                    self.node(*child).and_then(|child| child.parent) == Some(node.id),
                    "child-parent edge is not reciprocal"
                );
            }
            if let Some(entity) = node.world_entity {
                let binding = world
                    .get::<&MapInspectionBinding>(entity)
                    .context("bound inspection entity is unavailable")?;
                anyhow::ensure!(
                    binding.node == node.id,
                    "entity binding points at another node"
                );
                if node.kind.is_visual_owner() {
                    world
                        .get::<&MapEntityVisibility>(entity)
                        .context("visual inspection node has no visibility component")?;
                }
            }
            if let Some(owner) = node.visual_owner {
                let owner_node = self.node(owner).context("visual proxy owner is missing")?;
                anyhow::ensure!(
                    self.ancestors(node.id).any(|ancestor| ancestor == owner),
                    "visual proxy owner is not an ancestor"
                );
                anyhow::ensure!(
                    owner_node.kind.is_visual_owner(),
                    "visual proxy owner is not an ECS-owning visual kind"
                );
                anyhow::ensure!(
                    owner_node.world_entity.is_some(),
                    "visual proxy owner has no world entity"
                );
                anyhow::ensure!(
                    node.world_entity.is_none(),
                    "visual proxy must not own a world entity"
                );
                anyhow::ensure!(
                    node.bounds.is_none_or(|bounds| {
                        bounds.min.is_finite()
                            && bounds.max.is_finite()
                            && bounds.min.cmple(bounds.max).all()
                    }),
                    "visual proxy bounds are invalid"
                );
                anyhow::ensure!(
                    node.transform.is_none_or(|transform| {
                        transform.translation.is_finite()
                            && transform.rotation.is_finite()
                            && transform.scale.is_finite()
                    }),
                    "visual proxy transform is invalid"
                );
            }
            anyhow::ensure!(
                !(matches!(
                    node.disposition,
                    MapInspectionDisposition::Deferred | MapInspectionDisposition::Failed
                ) && node.world_entity.is_some()),
                "failed or deferred node claims a world entity"
            );
        }

        let mut visiting = HashSet::new();
        let mut visited = HashSet::new();
        let mut stack = vec![(self.root, false)];
        while let Some((id, closing)) = stack.pop() {
            if closing {
                visiting.remove(&id);
                visited.insert(id);
                continue;
            }
            anyhow::ensure!(!visiting.contains(&id), "inspection graph contains a cycle");
            if visited.contains(&id) {
                continue;
            }
            visiting.insert(id);
            stack.push((id, true));
            let node = self
                .node(id)
                .context("graph traversal reached missing node")?;
            stack.extend(node.children.iter().rev().map(|child| (*child, false)));
        }
        anyhow::ensure!(
            visited.len() == self.nodes.len(),
            "inspection graph contains an orphan"
        );

        let mut bound_entities = 0u32;
        for (entity, binding) in world.query::<&MapInspectionBinding>().iter() {
            bound_entities += 1;
            let node = self
                .node(binding.node)
                .context("world binding points to missing node")?;
            anyhow::ensure!(
                node.world_entity == Some(entity),
                "world binding does not round-trip"
            );
        }
        anyhow::ensure!(
            bound_entities == world.len(),
            "every map-world entity must have exactly one inspection binding"
        );
        for (group, index) in &self.source_groups {
            let group_node = self
                .node(index.node)
                .context("source group node is missing")?;
            anyhow::ensure!(
                matches!(
                    (group, group_node.kind),
                    (
                        MapInspectionSourceGroup::BaseContainer(_),
                        MapInspectionNodeKind::BaseContainer
                    ) | (
                        MapInspectionSourceGroup::Scenario(_),
                        MapInspectionNodeKind::Scenario
                    )
                ),
                "source group kind does not match source descriptor"
            );
            for table in &index.tables {
                anyhow::ensure!(
                    self.node(*table)
                        .is_some_and(|node| node.kind == MapInspectionNodeKind::Table),
                    "source group references a non-table node"
                );
            }
        }
        Ok(())
    }
}

#[derive(Default)]
pub(crate) struct MapInspectionGraphBuilder {
    next_id: u32,
    inspection: Option<ShadowkeepMapInspection>,
    // Holds the root only for `new`; keeping this isolated prevents an invalid
    // graph state from escaping the builder.
    pending_root: Option<MapInspectionNode>,
}

impl MapInspectionGraphBuilder {
    pub(crate) fn new(bubble: TagHash, child_map: TagHash) -> Self {
        let mut builder = Self::default();
        let root = builder.add_node(
            None,
            MapInspectionNode::new(
                MapInspectionNodeKind::Bubble,
                MapInspectionDisposition::NonRendering,
                format!("Bubble {bubble}"),
                ShadowkeepTableSources::default(),
            ),
        );
        builder.inspection = Some(ShadowkeepMapInspection {
            bubble,
            child_map,
            root,
            nodes: vec![builder.pending_root.take().expect("root is initialized")],
            node_index: vec![Some(0)],
            world_groups: BTreeMap::new(),
            source_groups: BTreeMap::new(),
            spawn_nodes: Vec::new(),
            locator_nodes: Vec::new(),
        });
        // `add_node` above allocated the stable root identity before the graph
        // backing storage existed. All later additions use the normal branch.
        debug_assert_eq!(root, MapInspectionNodeId(0));
        builder
    }

    pub(crate) fn root(&self) -> MapInspectionNodeId {
        self.inspection
            .as_ref()
            .expect("builder is initialized")
            .root
    }

    pub(crate) fn add_node(
        &mut self,
        parent: Option<MapInspectionNodeId>,
        mut node: MapInspectionNode,
    ) -> MapInspectionNodeId {
        let id = MapInspectionNodeId(self.next_id);
        self.next_id = self
            .next_id
            .checked_add(1)
            .expect("inspection node ID overflow");
        node.id = id;
        node.parent = parent;
        node.search_text = node_search_text(&node);
        let Some(inspection) = self.inspection.as_mut() else {
            self.pending_root = Some(node);
            return id;
        };
        if let Some(parent) = parent {
            inspection
                .node_mut(parent)
                .expect("inspection parent must be added before child")
                .children
                .push(id);
        }
        let index = inspection.nodes.len();
        inspection.nodes.push(node);
        inspection.node_index.push(Some(index));
        id
    }

    pub(crate) fn node_mut(&mut self, id: MapInspectionNodeId) -> &mut MapInspectionNode {
        self.inspection
            .as_mut()
            .and_then(|inspection| inspection.node_mut(id))
            .expect("inspection node must exist")
    }

    pub(crate) fn bind_world_entity(
        &mut self,
        id: MapInspectionNodeId,
        entity: hecs::Entity,
    ) -> anyhow::Result<()> {
        self.inspection
            .as_mut()
            .expect("builder is initialized")
            .bind_world_entity(id, entity)
    }

    pub(crate) fn add_source_group(
        &mut self,
        group: MapInspectionSourceGroup,
    ) -> MapInspectionNodeId {
        let (kind, tag, label) = match &group {
            MapInspectionSourceGroup::BaseContainer(tag) => (
                MapInspectionNodeKind::BaseContainer,
                *tag,
                format!("Base Container {tag}"),
            ),
            MapInspectionSourceGroup::Scenario(tag) => (
                MapInspectionNodeKind::Scenario,
                *tag,
                format!("Freeroam Scenario {tag}"),
            ),
        };
        let root = self.root();
        let mut node = MapInspectionNode::new(
            kind,
            MapInspectionDisposition::NonRendering,
            label,
            ShadowkeepTableSources::default(),
        );
        node.tag = Some(tag);
        let id = self.add_node(Some(root), node);
        self.inspection
            .as_mut()
            .expect("builder is initialized")
            .source_groups
            .insert(
                group,
                MapInspectionSourceGroupIndex {
                    node: id,
                    tables: Vec::new(),
                },
            );
        id
    }

    pub(crate) fn reference_table(
        &mut self,
        group: &MapInspectionSourceGroup,
        table: MapInspectionNodeId,
    ) {
        let index = self
            .inspection
            .as_mut()
            .expect("builder is initialized")
            .source_groups
            .get_mut(group)
            .expect("source group must be added before table reference");
        if !index.tables.contains(&table) {
            index.tables.push(table);
        }
    }

    pub(crate) fn finalize(mut self) -> ShadowkeepMapInspection {
        let mut inspection = self.inspection.take().expect("builder is initialized");
        debug_assert!(self.pending_root.is_none());
        inspection.world_groups.clear();
        inspection.spawn_nodes.clear();
        inspection.locator_nodes.clear();
        for node in &mut inspection.nodes {
            node.search_text = node_search_text(node);
            inspection
                .world_groups
                .entry(node.kind)
                .or_default()
                .push(node.id);
            if node.kind == MapInspectionNodeKind::SpawnPoint {
                inspection.spawn_nodes.push(node.id);
            }
            if node.kind.is_visual_locator()
                && (node.bounds.is_some_and(|bounds| {
                    bounds.min.is_finite()
                        && bounds.max.is_finite()
                        && bounds.min.cmple(bounds.max).all()
                }) || node
                    .transform
                    .is_some_and(|transform| transform.translation.is_finite()))
            {
                inspection.locator_nodes.push(node.id);
            }
        }
        inspection
    }
}

fn node_search_text(node: &MapInspectionNode) -> String {
    let mut values = vec![
        node.label.clone(),
        format!("{:?}", node.kind),
        node.disposition.status_label().to_owned(),
        node.source.label().to_owned(),
    ];
    if let Some(tag) = node.tag {
        let raw = format!("{tag}");
        values.push(raw.clone());
        values.push(format!("0x{raw}"));
    }
    if let Some(owner) = node.visual_owner {
        values.push(format!("owner {}", owner.0));
    }
    for source_tag in node
        .source
        .base_containers
        .iter()
        .copied()
        .chain(node.source.scenario)
    {
        let raw = format!("{source_tag}");
        values.push(raw.clone());
        values.push(format!("0x{raw}"));
    }
    if let Some(class) = node.class {
        values.push(format!("{class:08X}"));
        values.push(format!("0x{class:08X}"));
    }
    for value in [
        node.entry_index.map(|value| value.to_string()),
        node.element_index.map(|value| value.to_string()),
        node.world_id.map(|value| value.to_string()),
        node.definition_offset.map(|value| format!("{value:X}")),
        node.name_hash.map(|value| format!("{value:08X}")),
        node.activity_definition.map(|value| value.to_string()),
        node.activity_reference_offset
            .map(|value| format!("{value:X}")),
        node.activity_reference_count.map(|value| value.to_string()),
    ]
    .into_iter()
    .flatten()
    {
        values.push(value);
    }
    values.join(" ").to_ascii_lowercase()
}

pub fn normalize_search(query: &str) -> String {
    query.trim().to_ascii_lowercase()
}

fn disposition_filter(disposition: MapInspectionDisposition) -> MapInspectionDispositionFilter {
    match disposition {
        MapInspectionDisposition::Rendering => MapInspectionDispositionFilter::RENDERING,
        MapInspectionDisposition::NonRendering => MapInspectionDispositionFilter::NON_RENDERING,
        MapInspectionDisposition::Deferred => MapInspectionDispositionFilter::DEFERRED,
        MapInspectionDisposition::Failed => MapInspectionDispositionFilter::FAILED,
    }
}

fn node_matches_filter(node: &MapInspectionNode, filter: MapInspectionFilter) -> bool {
    let source = (node.source.is_base() as u8 * MapInspectionSourceFilter::BASE.bits())
        | (node.source.is_scenario() as u8 * MapInspectionSourceFilter::SCENARIO.bits());
    filter.types.intersects(node.kind.type_group())
        && filter
            .dispositions
            .intersects(disposition_filter(node.disposition))
        && (source == 0 || filter.sources.bits() & source != 0)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MapInspectionBinding {
    pub node: MapInspectionNodeId,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MapEntityVisibility {
    pub visible: bool,
}

impl Default for MapEntityVisibility {
    fn default() -> Self {
        Self { visible: true }
    }
}

pub fn is_map_entity_visible(visibility: Option<&MapEntityVisibility>) -> bool {
    visibility.is_none_or(|visibility| visibility.visible)
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MapVisibilityChange {
    pub affected: usize,
    pub unchanged: usize,
    pub stale: usize,
}

pub fn set_node_visibility(
    world: &mut hecs::World,
    inspection: &ShadowkeepMapInspection,
    node: MapInspectionNodeId,
    visible: bool,
    recursive: bool,
) -> MapVisibilityChange {
    let targets = std::iter::once(node).chain(
        recursive
            .then(|| inspection.descendants(node))
            .into_iter()
            .flatten(),
    );
    let mut change = MapVisibilityChange::default();
    for id in targets {
        let Some(node) = inspection.node(id) else {
            continue;
        };
        let Some(entity) = node.world_entity else {
            continue;
        };
        match world.get::<&mut MapEntityVisibility>(entity) {
            Ok(mut visibility) if visibility.visible != visible => {
                visibility.visible = visible;
                change.affected += 1;
            }
            Ok(_) => change.unchanged += 1,
            Err(_) if node.kind.is_visual_owner() => change.stale += 1,
            Err(_) => {}
        }
    }
    change
}

#[cfg(test)]
mod tests {
    use super::*;

    fn graph() -> (
        MapInspectionGraphBuilder,
        MapInspectionNodeId,
        MapInspectionNodeId,
    ) {
        let mut builder =
            MapInspectionGraphBuilder::new(TagHash(0x8080_0001), TagHash(0x8080_0002));
        let root = builder.root();
        let table = builder.add_node(
            Some(root),
            MapInspectionNode::new(
                MapInspectionNodeKind::Table,
                MapInspectionDisposition::NonRendering,
                "Table",
                ShadowkeepTableSources::default(),
            ),
        );
        let rigid = builder.add_node(
            Some(table),
            MapInspectionNode::new(
                MapInspectionNodeKind::RigidModel,
                MapInspectionDisposition::Rendering,
                "Rigid",
                ShadowkeepTableSources::default(),
            ),
        );
        (builder, table, rigid)
    }

    #[test]
    fn map_inspection_ids_are_deterministic_and_unique() {
        let (builder, _, _) = graph();
        let inspection = builder.finalize();
        assert_eq!(
            inspection
                .nodes
                .iter()
                .map(|node| node.id.0)
                .collect::<Vec<_>>(),
            vec![0, 1, 2]
        );
    }

    #[test]
    fn map_inspection_parent_child_edges_are_reciprocal() {
        let (builder, table, rigid) = graph();
        let inspection = builder.finalize();
        assert_eq!(inspection.node(rigid).unwrap().parent, Some(table));
        assert_eq!(inspection.node(table).unwrap().children, vec![rigid]);
    }

    #[test]
    fn map_inspection_rejects_cycles_and_orphans() {
        let (builder, table, rigid) = graph();
        let mut inspection = builder.finalize();
        inspection.node_mut(table).unwrap().parent = Some(rigid);
        assert!(inspection.validate(&hecs::World::new()).is_err());
    }

    #[test]
    fn map_inspection_world_bindings_round_trip() {
        let (mut builder, _, rigid) = graph();
        let mut world = hecs::World::new();
        let entity = world.spawn((
            MapInspectionBinding { node: rigid },
            MapEntityVisibility::default(),
        ));
        builder.bind_world_entity(rigid, entity).unwrap();
        builder.finalize().validate(&world).unwrap();
    }

    #[test]
    fn map_inspection_descendants_are_iterative() {
        let (builder, table, rigid) = graph();
        let inspection = builder.finalize();
        assert_eq!(
            inspection.descendants(table).collect::<Vec<_>>(),
            vec![rigid]
        );
    }

    #[test]
    fn map_inspection_visibility_hides_only_bound_visual_nodes() {
        let (mut builder, table, rigid) = graph();
        let mut world = hecs::World::new();
        let entity = world.spawn((
            MapInspectionBinding { node: rigid },
            MapEntityVisibility::default(),
        ));
        builder.bind_world_entity(rigid, entity).unwrap();
        let inspection = builder.finalize();

        let change = set_node_visibility(&mut world, &inspection, table, false, true);
        assert_eq!(change.affected, 1);
        assert!(!world.get::<&MapEntityVisibility>(entity).unwrap().visible);
    }
    #[test]
    fn map_inspection_missing_visibility_preserves_extraction_eligibility() {
        assert!(is_map_entity_visible(None));
        assert!(is_map_entity_visible(Some(&MapEntityVisibility::default())));
        assert!(!is_map_entity_visible(Some(&MapEntityVisibility {
            visible: false,
        })));
    }

    #[test]
    fn map_inspection_requires_one_binding_per_world_entity() {
        let (builder, _, _) = graph();
        let mut world = hecs::World::new();
        world.spawn((Transform::default(),));
        assert!(builder.finalize().validate(&world).is_err());
    }

    #[test]
    fn map_inspection_visual_owners_require_visibility() {
        let (mut builder, _, rigid) = graph();
        let mut world = hecs::World::new();
        let entity = world.spawn((MapInspectionBinding { node: rigid },));
        builder.bind_world_entity(rigid, entity).unwrap();
        assert!(builder.finalize().validate(&world).is_err());
    }

    #[test]
    fn map_inspection_metadata_nodes_need_no_world_entity() {
        let mut builder = MapInspectionGraphBuilder::new(TagHash(1), TagHash(2));
        let root = builder.root();
        builder.add_node(
            Some(root),
            MapInspectionNode::new(
                MapInspectionNodeKind::MetadataOnly,
                MapInspectionDisposition::NonRendering,
                "Metadata",
                ShadowkeepTableSources::default(),
            ),
        );
        builder.finalize().validate(&hecs::World::new()).unwrap();
    }

    #[test]
    fn map_inspection_source_groups_reference_canonical_nodes() {
        let mut builder = MapInspectionGraphBuilder::new(TagHash(1), TagHash(2));
        let group = MapInspectionSourceGroup::BaseContainer(TagHash(3));
        builder.add_source_group(group.clone());
        let table = builder.add_node(
            Some(builder.root()),
            MapInspectionNode::new(
                MapInspectionNodeKind::Table,
                MapInspectionDisposition::NonRendering,
                "Table",
                ShadowkeepTableSources::default(),
            ),
        );
        builder.reference_table(&group, table);
        builder.finalize().validate(&hecs::World::new()).unwrap();
    }

    #[test]
    fn map_inspection_search_matches_raw_tags_and_metadata() {
        let mut builder = MapInspectionGraphBuilder::new(TagHash(1), TagHash(2));
        let root = builder.root();
        let mut node = MapInspectionNode::new(
            MapInspectionNodeKind::RigidModel,
            MapInspectionDisposition::Rendering,
            "Resolved Rigid",
            ShadowkeepTableSources {
                base_containers: BTreeSet::from([TagHash(0x80AB_CDEF)]),
                ..Default::default()
            },
        );
        node.tag = Some(TagHash(0x8080_1234));
        node.class = Some(0x8080_72B8);
        node.world_id = Some(42);
        let id = builder.add_node(Some(root), node);
        let inspection = builder.finalize();
        for query in [
            "80801234",
            "0x80801234",
            "RESOLVED",
            "72b8",
            "42",
            "80abcdef",
        ] {
            assert_eq!(
                inspection.search(query, MapInspectionFilter::default()),
                vec![id]
            );
        }
    }

    #[test]
    fn map_inspection_large_search_is_stable_without_recursion() {
        let mut builder = MapInspectionGraphBuilder::new(TagHash(1), TagHash(2));
        let root = builder.root();
        for index in 0..10_000 {
            builder.add_node(
                Some(root),
                MapInspectionNode::new(
                    MapInspectionNodeKind::DeferredResource,
                    MapInspectionDisposition::Deferred,
                    format!("Deferred {index}"),
                    ShadowkeepTableSources::default(),
                ),
            );
        }
        let inspection = builder.finalize();
        let first = inspection.search("deferred 9999", MapInspectionFilter::default());
        let second = inspection.search("deferred 9999", MapInspectionFilter::default());
        assert_eq!(first, second);
        assert_eq!(first, vec![MapInspectionNodeId(10_000)]);
    }

    #[test]
    fn map_inspection_proxy_owners_and_locator_order_are_validated() {
        let (mut builder, table, owner) = graph();
        builder.node_mut(owner).bounds = Some(AxisAlignedBBox::from_center_extents(
            glam::Vec3::ZERO,
            glam::Vec3::splat(10.0),
        ));
        let sibling_owner = builder.add_node(
            Some(table),
            MapInspectionNode::new(
                MapInspectionNodeKind::RigidModel,
                MapInspectionDisposition::Rendering,
                "Sibling",
                ShadowkeepTableSources::default(),
            ),
        );
        let mut proxy = MapInspectionNode::new(
            MapInspectionNodeKind::StaticInstance,
            MapInspectionDisposition::Rendering,
            "Instance",
            ShadowkeepTableSources::default(),
        );
        proxy.visual_owner = Some(owner);
        proxy.transform = Some(Transform::default());
        proxy.bounds = Some(AxisAlignedBBox::from_center_extents(
            glam::Vec3::ZERO,
            glam::Vec3::ONE,
        ));
        let proxy = builder.add_node(Some(owner), proxy);
        let mut world = hecs::World::new();
        for id in [owner, sibling_owner] {
            let entity = world.spawn((
                MapInspectionBinding { node: id },
                MapEntityVisibility::default(),
            ));
            builder.bind_world_entity(id, entity).unwrap();
        }
        let inspection = builder.finalize();
        inspection.validate(&world).unwrap();
        assert_eq!(inspection.visual_owner(proxy), Some(owner));
        assert_eq!(inspection.locator_nodes(), &[owner, proxy]);

        let mut missing = inspection.clone();
        missing.node_mut(proxy).unwrap().visual_owner = Some(MapInspectionNodeId(999));
        assert!(missing.validate(&world).is_err());

        let mut non_owner = inspection.clone();
        non_owner.node_mut(proxy).unwrap().visual_owner = Some(table);
        assert!(non_owner.validate(&world).is_err());

        let mut non_ancestor = inspection;
        non_ancestor.node_mut(proxy).unwrap().visual_owner = Some(sibling_owner);
        assert!(non_ancestor.validate(&world).is_err());
    }
}

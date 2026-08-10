//! Shadowkeep map-chain decoding and scene normalization.
//!
//! The reader intentionally treats each table entry as an isolation boundary:
//! corrupt or unrecognised data is recorded in the report and does not prevent
//! the rest of a bubble from becoming viewable.

use std::{
    collections::HashSet,
    io::{Cursor, Seek, SeekFrom},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

use alkahest_data::{
    shadowkeep::{
        SShadowkeepBubbleDefinition, SShadowkeepBubbleParent, SShadowkeepCubemapPlacement,
        SShadowkeepEntity, SShadowkeepLightCollection, SShadowkeepMapDataTable,
        SShadowkeepOcclusionBounds, SShadowkeepShadowingLight, SShadowkeepStaticPlacement,
        SShadowkeepTerrainPlacement,
    },
    tfx::{
        TfxFeatureRenderer, atmosphere::SShadowkeepAtmospherePlacement, common::AxisAlignedBBox,
    },
};
use anyhow::Context;
use glam::{Vec3, Vec4Swizzles};
use parking_lot::Mutex;
use tiger_parse::{PackageManagerExt, TigerReadable};
use tiger_pkg::{TagHash, package_manager};

use crate::world::{
    render_objects::{DynamicRenderObject, StaticRenderObject},
    transform::Transform,
};
use alkahest_render::{
    Renderer,
    asset::texture::Texture,
    feature::{
        cubemap::CubemapRenderer, light::LightRenderer, rigid_model::DynamicModel,
        static_geometry::StaticInstancesRenderer, terrain_patches::TerrainPatchesRenderer,
    },
    object::RenderObject,
    renderer::submit::atmosphere::AtmosphereData,
};

const STATIC_PLACEMENT: u32 = 0x8080_71B3;
const TERRAIN_PLACEMENT: u32 = 0x8080_714B;
const LIGHT_COLLECTION: u32 = 0x8080_6F5A;
const SHADOWING_LIGHT: u32 = 0x8080_7133;
const CUBEMAP_VOLUME: u32 = 0x8080_6B7F;
const ATMOSPHERE_PLACEMENT: u32 = 0x8080_7086;
const SHADOWKEEP_LOOKUP_TABLE_BYTES: usize = 64 * 64 * 4;

fn bounded_offset(table_len: usize, offset: u64, required: usize) -> anyhow::Result<usize> {
    let offset = usize::try_from(offset).context("resource offset exceeds addressable memory")?;
    let end = offset
        .checked_add(required)
        .context("resource offset overflows while checking table bounds")?;
    anyhow::ensure!(
        end <= table_len,
        "resource range {offset:#X}..{end:#X} exceeds table length {table_len:#X}"
    );
    Ok(offset)
}
fn referenced_tags_with_class(bytes: &[u8], reference: u32) -> HashSet<TagHash> {
    let manager = package_manager();
    bytes
        .chunks_exact(4)
        .map(|chunk| TagHash(u32::from_le_bytes(chunk.try_into().unwrap())))
        .filter(|tag| {
            tag.is_some()
                && manager
                    .get_entry(*tag)
                    .is_some_and(|entry| entry.reference == reference)
        })
        .collect()
}

fn shadowkeep_scenario_tables(map: TagHash) -> anyhow::Result<(Option<TagHash>, Vec<TagHash>)> {
    let manager = package_manager();
    let package_name = &manager.package_paths[&map.pkg_id()].name;
    let scenario_name = format!("{package_name}_freeroam:scenario_client");
    let Some(scenario) = manager
        .get_named_tags_by_class(0x8080_9994)
        .into_iter()
        .find_map(|(name, tag)| (name == scenario_name).then_some(tag))
    else {
        return Ok((None, Vec::new()));
    };

    let phase_records = referenced_tags_with_class(&manager.read_tag(scenario)?, 0x8080_925B);
    let mut phase_roots = HashSet::new();
    for tag in phase_records {
        phase_roots.extend(referenced_tags_with_class(
            &manager.read_tag(tag)?,
            0x8080_925E,
        ));
    }
    let mut entity_resources = HashSet::new();
    for tag in phase_roots {
        entity_resources.extend(referenced_tags_with_class(
            &manager.read_tag(tag)?,
            0x8080_9462,
        ));
    }
    let mut wrappers = HashSet::new();
    for tag in entity_resources {
        wrappers.extend(referenced_tags_with_class(
            &manager.read_tag(tag)?,
            0x8080_9468,
        ));
    }
    let mut tables = HashSet::new();
    for tag in wrappers {
        tables.extend(referenced_tags_with_class(
            &manager.read_tag(tag)?,
            0x8080_99D6,
        ));
    }
    let mut tables = tables.into_iter().collect::<Vec<_>>();
    tables.sort_unstable();
    Ok((Some(scenario), tables))
}

/// Thread-safe map-load counters, updated by both decoding and asset setup.
#[derive(Clone, Default)]
pub struct MapLoadProgress {
    total_tables: Arc<AtomicUsize>,
    tables_seen: Arc<AtomicUsize>,
    entries_seen: Arc<AtomicUsize>,
    visual_resources_loaded: Arc<AtomicUsize>,
    gpu_assets_requested: Arc<AtomicUsize>,
    diagnostics: Arc<Mutex<Vec<String>>>,
    cancelled: Arc<AtomicBool>,
}

impl MapLoadProgress {
    pub fn snapshot(&self) -> MapLoadProgressSnapshot {
        MapLoadProgressSnapshot {
            total_tables: self.total_tables.load(Ordering::Relaxed),
            tables_seen: self.tables_seen.load(Ordering::Relaxed),
            entries_seen: self.entries_seen.load(Ordering::Relaxed),
            visual_resources_loaded: self.visual_resources_loaded.load(Ordering::Relaxed),
            gpu_assets_requested: self.gpu_assets_requested.load(Ordering::Relaxed),
            diagnostics: self.diagnostics.lock().len(),
            cancelled: self.cancelled.load(Ordering::Relaxed),
        }
    }

    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }

    fn diagnostic(&self, message: String) {
        self.diagnostics.lock().push(message);
    }
}

#[derive(Debug, Clone, Default)]
pub struct MapLoadProgressSnapshot {
    pub total_tables: usize,
    pub tables_seen: usize,
    pub entries_seen: usize,
    pub visual_resources_loaded: usize,
    pub gpu_assets_requested: usize,
    pub diagnostics: usize,
    pub cancelled: bool,
}

#[derive(Debug, Clone)]
pub struct MapLoadDiagnostic {
    pub table: TagHash,
    pub entry_offset: u64,
    pub resource_class: u32,
    pub error: String,
}

#[derive(Debug, Clone, Default)]
pub struct MapLoadReport {
    pub map: TagHash,
    pub containers: usize,
    pub scenario: Option<TagHash>,
    pub activity_tables: usize,
    pub tables: usize,
    pub entries: usize,
    pub static_placements: usize,
    pub terrain_placements: usize,
    pub entity_entries: usize,
    pub rigid_entities: usize,
    pub light_collections: usize,
    pub light_render_objects: usize,
    pub shadowing_lights: usize,
    pub cubemap_volumes: usize,
    pub atmosphere_placements: usize,
    pub static_render_objects: usize,
    pub terrain_render_objects: usize,
    pub rigid_render_objects: usize,
    pub cubemap_render_objects: usize,
    pub deduplicated_resources: usize,
    pub deferred_resources: usize,
    pub skipped_resources: usize,
    pub world_bounds: Option<AxisAlignedBBox>,
    /// Bounds from table placement transforms, retained separately from the
    /// reconstructed renderable bounds for a stable initial map frame.
    pub placement_bounds: Option<AxisAlignedBBox>,
    pub spawn_points: Vec<Vec3>,
    pub diagnostics: Vec<MapLoadDiagnostic>,
    pub cancelled: bool,
    pub elapsed: Duration,
}

impl MapLoadReport {
    pub fn is_degraded(&self) -> bool {
        !self.diagnostics.is_empty()
    }

    fn diagnostic(&mut self, progress: &MapLoadProgress, diagnostic: MapLoadDiagnostic) {
        progress.diagnostic(format!(
            "table {} offset {:#X} class {:08X}: {}",
            diagnostic.table, diagnostic.entry_offset, diagnostic.resource_class, diagnostic.error
        ));
        self.diagnostics.push(diagnostic);
    }
}

/// Decode a Shadowkeep bubble into the modern world. Static geometry is
/// immediately normalized into render objects; unimplemented visual families
/// remain explicit report diagnostics so they cannot silently disappear.
pub fn load_shadowkeep_map_into_world(
    tag: TagHash,
    renderer: &Arc<Renderer>,
    progress: &MapLoadProgress,
) -> anyhow::Result<(hecs::World, MapLoadReport)> {
    let started = Instant::now();
    let parent: SShadowkeepBubbleParent = package_manager()
        .read_tag_struct(tag)
        .context("Failed to read Shadowkeep bubble parent")?;
    let definition: SShadowkeepBubbleDefinition = package_manager()
        .read_tag_struct(parent.child_map)
        .context("Failed to read Shadowkeep bubble definition")?;
    let mut report = MapLoadReport {
        map: tag,
        ..Default::default()
    };
    report.containers = definition.map_resources.len();
    let mut table_hashes = definition
        .map_resources
        .iter()
        .flat_map(|container| container.data_tables.iter().copied())
        .collect::<Vec<_>>();
    match shadowkeep_scenario_tables(tag) {
        Ok((scenario, tables)) => {
            report.scenario = scenario;
            report.activity_tables = tables.len();
            table_hashes.extend(tables);
        }
        Err(error) => report.diagnostic(
            progress,
            MapLoadDiagnostic {
                table: tag,
                entry_offset: 0,
                resource_class: 0x8080_9994,
                error: format!("could not decode freeroam scenario layers: {error:#}"),
            },
        ),
    }
    let mut unique_tables = HashSet::new();
    table_hashes.retain(|table| unique_tables.insert(*table));
    progress
        .total_tables
        .store(table_hashes.len(), Ordering::Relaxed);
    let mut world = hecs::World::new();
    let mut bound_points = Vec::new();
    let mut loaded_static_collections = HashSet::new();
    let mut loaded_terrain_resources = HashSet::new();
    let mut visual_bounds: Option<AxisAlignedBBox> = None;

    for table_hash in table_hashes {
        if progress.is_cancelled() {
            report.cancelled = true;
            break;
        }
        let table: SShadowkeepMapDataTable = match package_manager().read_tag_struct(table_hash) {
            Ok(table) => table,
            Err(error) => {
                report.skipped_resources += 1;
                report.diagnostic(
                    progress,
                    MapLoadDiagnostic {
                        table: table_hash,
                        entry_offset: 0,
                        resource_class: 0,
                        error: format!("could not decode map data table: {error:#}"),
                    },
                );
                continue;
            }
        };
        let table_bytes = match package_manager().read_tag(table_hash) {
            Ok(bytes) => bytes,
            Err(error) => {
                report.skipped_resources += table.data_entries.len();
                report.diagnostic(
                    progress,
                    MapLoadDiagnostic {
                        table: table_hash,
                        entry_offset: 0,
                        resource_class: 0,
                        error: format!("could not read map data table bytes: {error:#}"),
                    },
                );
                continue;
            }
        };
        report.tables += 1;
        progress.tables_seen.fetch_add(1, Ordering::Relaxed);
        for entry in &table.data_entries {
            if progress.is_cancelled() {
                report.cancelled = true;
                break;
            }
            report.entries += 1;
            progress.entries_seen.fetch_add(1, Ordering::Relaxed);
            if !entry.data_resource.is_valid {
                report.skipped_resources += 1;
                continue;
            }
            bound_points.push(entry.translation.xyz());
            let transform = Transform::new(
                entry.translation.xyz(),
                entry.rotation,
                Vec3::splat(entry.translation.w),
            );
            if !entry.entity.is_none() {
                report.entity_entries += 1;
                if let Ok(entity) =
                    package_manager().read_tag_struct::<SShadowkeepEntity>(entry.entity)
                {
                    for resource_ref in &entity.entity_resources {
                        if progress.is_cancelled() {
                            report.cancelled = true;
                            break;
                        }
                        let resource = &*resource_ref.resource;
                        if resource.resource.resource_type != 0x8080_72B8
                            || !resource.definition.is_valid
                        {
                            continue;
                        }
                        report.rigid_entities += 1;
                        let result = (|| -> anyhow::Result<()> {
                            let bytes =
                                package_manager().read_tag(resource_ref.resource.taghash())?;
                            let mut cursor = Cursor::new(bytes);
                            cursor.seek(SeekFrom::Start(resource.definition.offset))?;
                            let component =
                                alkahest_data::shadowkeep::SShadowkeepRigidModelComponent::read_ds(
                                    &mut cursor,
                                )?;
                            let model = DynamicModel::load_shadowkeep(
                                component.model,
                                component.material_variants,
                                component.techniques,
                            )?;
                            progress
                                .gpu_assets_requested
                                .fetch_add(1, Ordering::Relaxed);
                            world.spawn((
                                transform,
                                DynamicRenderObject::new(renderer.add_object(RenderObject::new(
                                    TfxFeatureRenderer::DynamicObjects,
                                    model,
                                ))),
                            ));
                            progress
                                .visual_resources_loaded
                                .fetch_add(1, Ordering::Relaxed);
                            report.rigid_render_objects += 1;
                            Ok(())
                        })();
                        if let Err(error) = result {
                            report.skipped_resources += 1;
                            report.diagnostic(
                                progress,
                                MapLoadDiagnostic {
                                    table: table_hash,
                                    entry_offset: entry.data_resource.offset,
                                    resource_class: 0x8080_72B8,
                                    error: format!("rigid model: {error:#}"),
                                },
                            );
                        }
                    }
                }
            }

            match entry.data_resource.resource_type {
                ATMOSPHERE_PLACEMENT => {
                    report.atmosphere_placements += 1;
                    if world.query::<&AtmosphereData>().iter().next().is_some() {
                        report.deduplicated_resources += 1;
                        continue;
                    }
                    let result = (|| -> anyhow::Result<()> {
                        bounded_offset(table_bytes.len(), entry.data_resource.offset, 0xF8)?;
                        let mut cursor = Cursor::new(&table_bytes[..]);
                        cursor.seek(SeekFrom::Start(entry.data_resource.offset))?;
                        let placement = SShadowkeepAtmospherePlacement::read_ds(&mut cursor)?;
                        for (name, tag) in [
                            ("lookup_volume_0", placement.lookup_volume_0),
                            ("lookup_volume_1", placement.lookup_volume_1),
                            ("lookup_vertical", placement.lookup_vertical),
                            ("lookup_table", placement.lookup_table),
                        ] {
                            anyhow::ensure!(tag.is_some(), "{name} tag is absent");
                            anyhow::ensure!(
                                package_manager().get_entry(tag).is_some(),
                                "{name} tag {tag} is not present in the package set"
                            );
                        }

                        let lookup_table_bytes =
                            package_manager().read_tag(placement.lookup_table)?;
                        anyhow::ensure!(
                            lookup_table_bytes.len() == SHADOWKEEP_LOOKUP_TABLE_BYTES,
                            "lookup table {} has {} bytes; expected {}",
                            placement.lookup_table,
                            lookup_table_bytes.len(),
                            SHADOWKEEP_LOOKUP_TABLE_BYTES,
                        );
                        let lookup_table = Texture::load_2d_raw(
                            &renderer.gpu.device,
                            64,
                            64,
                            &lookup_table_bytes,
                            d3d11::dxgi::Format::R8g8b8a8UnormSrgb,
                            Some("Shadowkeep atmosphere lookup table"),
                            false,
                        )?;
                        let atmosphere = AtmosphereData {
                            shadowkeep_lookup_volume_0: renderer
                                .asset_manager
                                .load(placement.lookup_volume_0),
                            shadowkeep_lookup_volume_1: renderer
                                .asset_manager
                                .load(placement.lookup_volume_1),
                            shadowkeep_lookup_vertical: renderer
                                .asset_manager
                                .load(placement.lookup_vertical),
                            shadowkeep_lookup_table: Some(lookup_table),
                            shadowkeep_lookup_parameters: placement.lookup_parameters,
                            ..Default::default()
                        };
                        world.spawn((atmosphere,));
                        progress
                            .gpu_assets_requested
                            .fetch_add(4, Ordering::Relaxed);
                        progress
                            .visual_resources_loaded
                            .fetch_add(1, Ordering::Relaxed);
                        Ok(())
                    })();
                    if let Err(error) = result {
                        report.skipped_resources += 1;
                        report.diagnostic(
                            progress,
                            MapLoadDiagnostic {
                                table: table_hash,
                                entry_offset: entry.data_resource.offset,
                                resource_class: ATMOSPHERE_PLACEMENT,
                                error: format!("atmosphere placement: {error:#}"),
                            },
                        );
                    }
                }
                CUBEMAP_VOLUME => {
                    report.cubemap_volumes += 1;
                    let result = (|| -> anyhow::Result<()> {
                        bounded_offset(table_bytes.len(), entry.data_resource.offset, 0x1A4)?;
                        let mut cursor = Cursor::new(&table_bytes[..]);
                        cursor.seek(SeekFrom::Start(entry.data_resource.offset))?;
                        let placement = SShadowkeepCubemapPlacement::read_ds(&mut cursor)?;
                        let component = placement.normalized();
                        let cubemap = CubemapRenderer::load(&renderer.gpu, &component)?;
                        progress
                            .gpu_assets_requested
                            .fetch_add(1, Ordering::Relaxed);
                        let (volume_scale, volume_rotation, volume_translation) =
                            component.unkb0.to_scale_rotation_translation();
                        world.spawn((
                            Transform::new(volume_translation, volume_rotation, volume_scale),
                            DynamicRenderObject::new(renderer.add_object(RenderObject::new(
                                TfxFeatureRenderer::Cubemaps,
                                Box::new(cubemap),
                            ))),
                        ));
                        progress
                            .visual_resources_loaded
                            .fetch_add(1, Ordering::Relaxed);
                        report.cubemap_render_objects += 1;
                        Ok(())
                    })();
                    if let Err(error) = result {
                        report.skipped_resources += 1;
                        report.diagnostic(
                            progress,
                            MapLoadDiagnostic {
                                table: table_hash,
                                entry_offset: entry.data_resource.offset,
                                resource_class: CUBEMAP_VOLUME,
                                error: format!("cubemap volume: {error:#}"),
                            },
                        );
                    }
                }
                LIGHT_COLLECTION => {
                    report.light_collections += 1;
                    let result = (|| -> anyhow::Result<()> {
                        bounded_offset(table_bytes.len(), entry.data_resource.offset, 20)?;
                        let mut cursor = Cursor::new(&table_bytes[..]);
                        cursor.seek(SeekFrom::Start(entry.data_resource.offset + 16))?;
                        let light_tag = TagHash::read_ds(&mut cursor)?;
                        anyhow::ensure!(light_tag.is_some(), "light collection tag is absent");
                        tracing::warn!(table = %table_hash, tag = %light_tag, "reading Shadowkeep light collection");
                        let collection: SShadowkeepLightCollection =
                            package_manager().read_tag_struct(light_tag)?;
                        tracing::warn!(
                            table = %table_hash,
                            tag = %light_tag,
                            lights = collection.lights.len(),
                            transforms = collection.transforms.len(),
                            "decoded Shadowkeep light collection"
                        );
                        let bounds = package_manager()
                            .read_tag_struct::<SShadowkeepOcclusionBounds>(
                                collection.occlusion_bounds.taghash(),
                            )
                            .ok();
                        for (index, light) in collection.lights.iter().enumerate() {
                            if progress.is_cancelled() {
                                report.cancelled = true;
                                break;
                            }
                            let transform = collection.transforms.get(index).copied().unwrap_or(
                                alkahest_data::shadowkeep::SShadowkeepRotationTranslation {
                                    rotation: glam::Quat::IDENTITY,
                                    translation: glam::Vec4::ZERO,
                                },
                            );
                            let _bounds = bounds
                                .as_ref()
                                .and_then(|value| value.bounds.get(index))
                                .map(|value| value.bb);
                            tracing::debug!(
                                table = %table_hash,
                                light = index,
                                technique = %light.technique_shading,
                                volumetrics = %light.unk84,
                                "decoded Shadowkeep light"
                            );
                            let renderer_object = LightRenderer::new_shadowkeep(
                                renderer,
                                light.technique_shading,
                                light.unk84,
                                light.light_to_world,
                                // The preserved occlusion boxes are in
                                // collection-local space.  Applying them
                                // before `prepare` would cull every light
                                // against the world camera; LightRenderer
                                // derives the world-space volume on its
                                // first prepare instead.
                                None,
                            )?;
                            world.spawn((
                                Transform::new(
                                    transform.translation.xyz(),
                                    transform.rotation,
                                    Vec3::ONE,
                                ),
                                DynamicRenderObject::new(renderer.add_object(RenderObject::new(
                                    TfxFeatureRenderer::DeferredLights,
                                    renderer_object,
                                ))),
                            ));
                            report.light_render_objects += 1;
                        }
                        Ok(())
                    })();
                    if let Err(error) = result {
                        tracing::warn!(table = %table_hash, class = %format_args!("{LIGHT_COLLECTION:08X}"), error = %format_args!("{error:#}"), "Shadowkeep light collection skipped");
                        report.skipped_resources += 1;
                        report.diagnostic(
                            progress,
                            MapLoadDiagnostic {
                                table: table_hash,
                                entry_offset: entry.data_resource.offset,
                                resource_class: LIGHT_COLLECTION,
                                error: format!("light collection: {error:#}"),
                            },
                        );
                    }
                }
                SHADOWING_LIGHT => {
                    report.shadowing_lights += 1;
                    let result = (|| -> anyhow::Result<()> {
                        bounded_offset(table_bytes.len(), entry.data_resource.offset, 20)?;
                        let mut cursor = Cursor::new(&table_bytes[..]);
                        cursor.seek(SeekFrom::Start(entry.data_resource.offset + 16))?;
                        let light_tag = TagHash::read_ds(&mut cursor)?;
                        anyhow::ensure!(light_tag.is_some(), "shadowing light tag is absent");
                        let light: SShadowkeepShadowingLight =
                            package_manager().read_tag_struct(light_tag)?;
                        let renderer_object = LightRenderer::new_shadowkeep_shadowing(
                            renderer,
                            light.technique_shading,
                            light.technique_shading_shadowing,
                            light.technique_volumetrics,
                            light.technique_volumetrics_shadowing,
                            light.light_to_world,
                            glam::Mat4::IDENTITY,
                        )?;
                        world.spawn((
                            Transform::new(
                                entry.translation.xyz(),
                                entry.rotation,
                                Vec3::splat(entry.translation.w),
                            ),
                            DynamicRenderObject::new(renderer.add_object(RenderObject::new(
                                TfxFeatureRenderer::DeferredLights,
                                renderer_object,
                            ))),
                        ));
                        Ok(())
                    })();
                    if let Err(error) = result {
                        tracing::warn!(table = %table_hash, class = %format_args!("{SHADOWING_LIGHT:08X}"), error = %format_args!("{error:#}"), "Shadowkeep shadowing light skipped");
                        report.skipped_resources += 1;
                        report.diagnostic(
                            progress,
                            MapLoadDiagnostic {
                                table: table_hash,
                                entry_offset: entry.data_resource.offset,
                                resource_class: SHADOWING_LIGHT,
                                error: format!("shadowing light: {error:#}"),
                            },
                        );
                    }
                }
                STATIC_PLACEMENT => {
                    report.static_placements += 1;
                    let result = (|| -> anyhow::Result<()> {
                        bounded_offset(table_bytes.len(), entry.data_resource.offset, 20)?;
                        let mut cursor = Cursor::new(&table_bytes[..]);
                        cursor.seek(SeekFrom::Start(entry.data_resource.offset + 16))?;
                        let header_tag = TagHash::read_ds(&mut cursor)?;
                        let placement: SShadowkeepStaticPlacement =
                            package_manager().read_tag_struct(header_tag)?;
                        let instances_hash = placement.instances.taghash();
                        if !loaded_static_collections.insert(instances_hash) {
                            report.deduplicated_resources += 1;
                            return Ok(());
                        }
                        let feature = StaticInstancesRenderer::load_shadowkeep(
                            &renderer.gpu,
                            instances_hash,
                        )?;
                        let feature_bounds = feature.bounds();
                        if feature_bounds.is_valid() {
                            visual_bounds = Some(match visual_bounds {
                                Some(bounds) => bounds.union(&feature_bounds),
                                None => feature_bounds,
                            });
                        }
                        progress
                            .gpu_assets_requested
                            .fetch_add(1, Ordering::Relaxed);
                        world.spawn((StaticRenderObject::new(renderer.add_object(
                            RenderObject::new(
                                TfxFeatureRenderer::ChunkedInstanceObjects,
                                Box::new(feature),
                            ),
                        )),));
                        progress
                            .visual_resources_loaded
                            .fetch_add(1, Ordering::Relaxed);
                        report.static_render_objects += 1;
                        Ok(())
                    })();
                    if let Err(error) = result {
                        report.skipped_resources += 1;
                        report.diagnostic(
                            progress,
                            MapLoadDiagnostic {
                                table: table_hash,
                                entry_offset: entry.data_resource.offset,
                                resource_class: STATIC_PLACEMENT,
                                error: format!("static placement: {error:#}"),
                            },
                        );
                    }
                }
                TERRAIN_PLACEMENT => {
                    report.terrain_placements += 1;
                    let result = (|| -> anyhow::Result<()> {
                        bounded_offset(table_bytes.len(), entry.data_resource.offset, 0x20)?;
                        let mut cursor = Cursor::new(&table_bytes[..]);
                        cursor.seek(SeekFrom::Start(entry.data_resource.offset))?;
                        let placement = SShadowkeepTerrainPlacement::read_ds(&mut cursor)?;
                        if !loaded_terrain_resources.insert(placement.terrain) {
                            report.deduplicated_resources += 1;
                            return Ok(());
                        }
                        let terrain = TerrainPatchesRenderer::load_shadowkeep(
                            &renderer.gpu,
                            placement.terrain,
                            placement.identifier as u64,
                        )?;
                        progress
                            .gpu_assets_requested
                            .fetch_add(1, Ordering::Relaxed);
                        world.spawn((StaticRenderObject::new(renderer.add_object(
                            RenderObject::new(TfxFeatureRenderer::TerrainPatch, terrain),
                        )),));
                        progress
                            .visual_resources_loaded
                            .fetch_add(1, Ordering::Relaxed);
                        report.terrain_render_objects += 1;
                        Ok(())
                    })();
                    if let Err(error) = result {
                        report.skipped_resources += 1;
                        report.diagnostic(
                            progress,
                            MapLoadDiagnostic {
                                table: table_hash,
                                entry_offset: entry.data_resource.offset,
                                resource_class: TERRAIN_PLACEMENT,
                                error: format!("terrain placement: {error:#}"),
                            },
                        );
                    }
                }
                _ => {}
            }
        }
    }
    report.placement_bounds =
        (!bound_points.is_empty()).then(|| AxisAlignedBBox::from_points(&bound_points));
    report.world_bounds = visual_bounds.or(report.placement_bounds);
    tracing::info!(
        map = %tag,
        scenario = ?report.scenario,
        activity_tables = report.activity_tables,
        light_collections = report.light_collections,
        light_render_objects = report.light_render_objects,
        cubemap_volumes = report.cubemap_volumes,
        atmosphere_placements = report.atmosphere_placements,
        cubemap_render_objects = report.cubemap_render_objects,
        shadowing_lights = report.shadowing_lights,
        skipped_resources = report.skipped_resources,
        ?report.world_bounds,
        "completed Shadowkeep map normalization"
    );
    report.elapsed = started.elapsed();
    Ok((world, report))
}

#[cfg(test)]
mod tests {
    use super::{MapLoadProgress, bounded_offset};

    #[test]
    fn bounded_resource_offsets_reject_overflow_and_truncation() {
        assert_eq!(bounded_offset(0x40, 0x20, 0x10).unwrap(), 0x20);
        assert!(bounded_offset(0x40, 0x31, 0x10).is_err());
        assert!(bounded_offset(0x40, u64::MAX, 1).is_err());
    }

    #[test]
    fn cancellation_is_shared_with_progress_observers() {
        let progress = MapLoadProgress::default();
        let worker_view = progress.clone();
        assert!(!worker_view.is_cancelled());
        progress.cancel();
        assert!(worker_view.is_cancelled());
        assert!(worker_view.snapshot().cancelled);
    }
}

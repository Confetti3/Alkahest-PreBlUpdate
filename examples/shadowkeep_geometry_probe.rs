//! Read-only structural validator for the Shadowkeep core geometry families.
//!
//! Usage: `cargo run --example shadowkeep_geometry_probe -- <packages-dir>`.

use std::{
    collections::BTreeMap,
    io::{Cursor, Seek, SeekFrom},
};

use alkahest_data::{
    shadowkeep::{
        SShadowkeepBubbleDefinition, SShadowkeepBubbleParent, SShadowkeepDynamicModel,
        SShadowkeepEntity, SShadowkeepMapDataTable, SShadowkeepRigidModelComponent,
        SShadowkeepStaticMesh, SShadowkeepStaticMeshInstances, SShadowkeepStaticPlacement,
        SShadowkeepTerrain, SShadowkeepTerrainPlacement, lod_category_from_legacy,
        primitive_type_from_legacy, render_stage_from_legacy,
    },
    tfx::shadowkeep::ShadowkeepEraProfile,
};
use anyhow::{Context, Result, bail};
use tiger_parse::{PackageManagerExt, TigerReadable};
use tiger_pkg::{TagHash, package_manager};

#[derive(Default)]
struct Scan {
    bubbles: usize,
    tables: usize,
    entities: usize,
    statics: usize,
    instances: usize,
    terrains: usize,
    dynamics: usize,
    explicit_nulls: usize,
    max_layout: u16,
    resource_types: BTreeMap<u32, usize>,
    first_error: Option<String>,
}

impl Scan {
    fn check(&mut self, result: Result<()>) {
        if let Err(error) = result
            && self.first_error.is_none()
        {
            self.first_error = Some(format!("{error:#}"));
        }
    }

    fn finish(&self) -> Result<()> {
        println!(
            "bubbles={} tables={} entities={} statics={} instances={} terrains={} dynamics={} explicit_nulls={} max_layout={}",
            self.bubbles,
            self.tables,
            self.entities,
            self.statics,
            self.instances,
            self.terrains,
            self.dynamics,
            self.explicit_nulls,
            self.max_layout,
        );
        println!("map resource types:");
        for (resource_type, count) in &self.resource_types {
            println!("  0x{resource_type:08X}: {count}");
        }
        if let Some(error) = &self.first_error {
            bail!("Shadowkeep core geometry scan failed: {error}");
        }
        Ok(())
    }
}

fn validate_static(tag: TagHash, model: &SShadowkeepStaticMesh, layout_count: usize) -> Result<()> {
    let mesh = &*model.opaque_meshes;
    for (index, group) in mesh.mesh_groups.iter().enumerate() {
        let part = mesh.parts.get(group.part_index as usize).with_context(|| {
            format!(
                "static {tag} group {index} references part {}",
                group.part_index
            )
        })?;
        if part.buffer_index as usize >= mesh.buffers.len() {
            bail!(
                "static {tag} part {} buffer index {} >= {}",
                group.part_index,
                part.buffer_index,
                mesh.buffers.len()
            );
        }
        render_stage_from_legacy(group.render_stage).with_context(|| {
            format!(
                "static {tag} group {index} invalid render stage {}",
                group.render_stage
            )
        })?;
        primitive_type_from_legacy(part.primitive_type).with_context(|| {
            format!(
                "static {tag} part {} invalid primitive {}",
                group.part_index, part.primitive_type
            )
        })?;
        lod_category_from_legacy(part.lod_category).with_context(|| {
            format!(
                "static {tag} part {} invalid lod {}",
                group.part_index, part.lod_category
            )
        })?;
        if group.input_layout_index as usize >= layout_count {
            bail!(
                "static {tag} group {index} layout {} >= {layout_count}",
                group.input_layout_index
            );
        }
    }
    Ok(())
}

fn validate_instances(tag: TagHash, instances: &SShadowkeepStaticMeshInstances) -> Result<()> {
    for (index, group) in instances.instance_groups.iter().enumerate() {
        if group.static_index as usize >= instances.statics.len() {
            bail!(
                "instances {tag} group {index} static index {} >= {}",
                group.static_index,
                instances.statics.len()
            );
        }
        let range = group.transform_range();
        if range.end > instances.transforms.len() {
            bail!(
                "instances {tag} group {index} transform range {range:?} exceeds {}",
                instances.transforms.len()
            );
        }
    }
    Ok(())
}

fn validate_terrain(tag: TagHash, terrain: &SShadowkeepTerrain) -> Result<()> {
    for (index, part) in terrain.mesh_parts.iter().enumerate() {
        if part.group_index as usize >= terrain.mesh_groups.len() {
            bail!(
                "terrain {tag} part {index} group {} >= {}",
                part.group_index,
                terrain.mesh_groups.len()
            );
        }
        if part.technique.is_none() {
            bail!("terrain {tag} part {index} has a null technique");
        }
    }
    Ok(())
}

fn validate_dynamic(
    tag: TagHash,
    model: &SShadowkeepDynamicModel,
    layout_count: usize,
) -> Result<()> {
    for (mesh_index, mesh) in model.meshes.iter().enumerate() {
        for stage in 0..23u8 {
            let range = mesh
                .range_for_stage(stage)
                .expect("fixed legacy stage range");
            if range.start > range.end || range.end > mesh.parts.len() {
                bail!(
                    "dynamic {tag} mesh {mesh_index} stage {stage} invalid part range {range:?} / {}",
                    mesh.parts.len()
                );
            }
            if range.start != range.end {
                let layout = mesh.input_layout_per_render_stage[stage as usize];
                if layout as usize >= layout_count {
                    bail!(
                        "dynamic {tag} mesh {mesh_index} stage {stage} layout {layout} >= {layout_count}"
                    );
                }
            }
        }
        for (part_index, part) in mesh.parts.iter().enumerate() {
            primitive_type_from_legacy(part.primitive_type).with_context(|| {
                format!(
                    "dynamic {tag} mesh {mesh_index} part {part_index} invalid primitive {}",
                    part.primitive_type
                )
            })?;
            lod_category_from_legacy(part.lod_category).with_context(|| {
                format!(
                    "dynamic {tag} mesh {mesh_index} part {part_index} invalid lod {}",
                    part.lod_category
                )
            })?;
        }
    }
    Ok(())
}

fn scan_map_table(scan: &mut Scan, table_hash: TagHash, layout_count: usize) -> Result<()> {
    let table: SShadowkeepMapDataTable = package_manager().read_tag_struct(table_hash)?;
    scan.tables += 1;
    let bytes = package_manager().read_tag(table_hash)?;
    for (entry_index, entry) in table.data_entries.iter().enumerate() {
        if !entry.data_resource.is_valid {
            scan.explicit_nulls += 1;
            continue;
        }
        let offset = entry.data_resource.offset;
        *scan
            .resource_types
            .entry(entry.data_resource.resource_type)
            .or_default() += 1;
        let mut cursor = Cursor::new(bytes.as_slice());
        match entry.data_resource.resource_type {
            0x808071B3 => {
                cursor.seek(SeekFrom::Start(offset + 16))?;
                let placement = TagHash::read_ds(&mut cursor)?;
                if placement.is_none() {
                    scan.explicit_nulls += 1;
                    continue;
                }
                let placement: SShadowkeepStaticPlacement =
                    package_manager().read_tag_struct(placement)?;
                validate_instances(placement.instances.taghash(), &placement.instances)
                    .with_context(|| format!("map table {table_hash} entry {entry_index}"))?;
            }
            0x8080714B => {
                cursor.seek(SeekFrom::Start(offset))?;
                let terrain = SShadowkeepTerrainPlacement::read_ds(&mut cursor)?;
                if terrain.terrain.is_none() {
                    scan.explicit_nulls += 1;
                } else {
                    let terrain_data: SShadowkeepTerrain =
                        package_manager().read_tag_struct(terrain.terrain)?;
                    validate_terrain(terrain.terrain, &terrain_data)
                        .with_context(|| format!("map table {table_hash} entry {entry_index}"))?;
                }
            }
            _ if entry.entity.is_some() => {
                let entity: SShadowkeepEntity = package_manager().read_tag_struct(entry.entity)?;
                scan.entities += 1;
                for resource_ref in &entity.entity_resources {
                    let resource = &*resource_ref.resource;
                    if resource.resource.resource_type != 0x808072B8
                        || !resource.definition.is_valid
                    {
                        continue;
                    }
                    let resource_tag = resource_ref.resource.taghash();
                    let resource_bytes = package_manager().read_tag(resource_tag)?;
                    let mut component = Cursor::new(resource_bytes);
                    component.seek(SeekFrom::Start(resource.definition.offset))?;
                    let component = SShadowkeepRigidModelComponent::read_ds(&mut component)?;
                    if component.model.is_none() {
                        scan.explicit_nulls += 1;
                    } else {
                        let dynamic: SShadowkeepDynamicModel =
                            package_manager().read_tag_struct(component.model)?;
                        validate_dynamic(component.model, &dynamic, layout_count).with_context(
                            || format!("map table {table_hash} entry {entry_index}"),
                        )?;
                    }
                }
            }
            _ => {}
        }
    }
    Ok(())
}

fn main() -> Result<()> {
    let packages = std::env::args()
        .nth(1)
        .context("usage: shadowkeep_geometry_probe <packages-dir>")?;
    alkahest_core::initialize_package_manager(None, Some(packages.as_str()))?;

    let bootstrap = ShadowkeepEraProfile.load_bootstrap()?;
    let mut scan = Scan::default();
    let manager = package_manager();

    for (tag, _) in manager.get_all_by_reference(SShadowkeepBubbleParent::ID.unwrap()) {
        scan.bubbles += 1;
        let result = (|| -> Result<()> {
            let bubble: SShadowkeepBubbleParent = manager.read_tag_struct(tag)?;
            if bubble.child_map.is_none() {
                scan.explicit_nulls += 1;
                return Ok(());
            }
            let definition: SShadowkeepBubbleDefinition =
                manager.read_tag_struct(bubble.child_map)?;
            for container in &definition.map_resources {
                for table in &container.data_tables {
                    scan_map_table(&mut scan, *table, bootstrap.input_layout_count)?;
                }
            }
            Ok(())
        })()
        .with_context(|| format!("bubble {tag}"));
        scan.check(result);
    }

    for (tag, _) in manager.get_all_by_reference(SShadowkeepStaticMesh::ID.unwrap()) {
        scan.statics += 1;
        let mut max_layout = 0;
        let result =
            (|| -> Result<()> {
                let model: SShadowkeepStaticMesh = manager.read_tag_struct(tag)?;
                max_layout = model.opaque_meshes.mesh_groups.iter()
                    .map(|group| group.input_layout_index as u16).max().unwrap_or(0);
                validate_static(tag, &model, bootstrap.input_layout_count)
            })()
            .with_context(|| format!("static {tag}"));
        if result.is_ok() { scan.max_layout = scan.max_layout.max(max_layout); }
        scan.check(result);
    }
    for (tag, _) in manager.get_all_by_reference(SShadowkeepStaticMeshInstances::ID.unwrap()) {
        scan.instances += 1;
        scan.check(
            (|| -> Result<()> {
                let instances: SShadowkeepStaticMeshInstances = manager.read_tag_struct(tag)?;
                validate_instances(tag, &instances)
            })()
            .with_context(|| format!("instances {tag}")),
        );
    }
    for (tag, _) in manager.get_all_by_reference(SShadowkeepTerrain::ID.unwrap()) {
        scan.terrains += 1;
        scan.check(
            (|| -> Result<()> {
                let terrain: SShadowkeepTerrain = manager.read_tag_struct(tag)?;
                validate_terrain(tag, &terrain)
            })()
            .with_context(|| format!("terrain {tag}")),
        );
    }
    for (tag, _) in manager.get_all_by_reference(SShadowkeepDynamicModel::ID.unwrap()) {
        scan.dynamics += 1;
        let mut max_layout = 0;
        let result =
            (|| -> Result<()> {
                let dynamic: SShadowkeepDynamicModel = manager.read_tag_struct(tag)?;
                max_layout = dynamic.meshes.iter()
                    .flat_map(|mesh| mesh.input_layout_per_render_stage)
                    .max().unwrap_or(0);
                validate_dynamic(tag, &dynamic, bootstrap.input_layout_count)
            })()
            .with_context(|| format!("dynamic {tag}"));
        if result.is_ok() { scan.max_layout = scan.max_layout.max(max_layout); }
        scan.check(result);
    }

    scan.finish()
}

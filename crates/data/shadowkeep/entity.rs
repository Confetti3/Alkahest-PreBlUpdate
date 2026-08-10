use tiger_parse::{tiger_type, ResourcePointer, ResourcePointerWithClass};
use tiger_pkg::TagHash;

use crate::{shadowkeep::geometry::SShadowkeepDynamicMaterialVariant, tag::Tag};

#[derive(Clone, Debug)]
#[tiger_type(id = 0x80809C0F)]
pub struct SShadowkeepEntity {
    pub file_size: u64,
    pub unk8: [u32; 2],
    pub entity_resources: Vec<SShadowkeepEntityResourceRef>,
}

#[derive(Clone, Debug)]
#[tiger_type(id = 0x80809C04)]
pub struct SShadowkeepEntityResourceRef {
    pub resource: Tag<SShadowkeepEntityResource>,
    pub unk4: u32,
    pub unk8: u32,
}

/// Entity-resource header used by the Arrivals entity graph.
#[derive(Clone, Debug)]
#[tiger_type(id = 0x80809C36, size = 0x88)]
pub struct SShadowkeepEntityResource {
    pub file_size: u64,
    pub unk8: ResourcePointer,
    pub resource: ResourcePointer,
    pub definition: ResourcePointer,
    pub resource_table: Vec<ResourcePointerWithClass>,
    #[tiger(offset = 0x30)]
    pub resource_table1: Vec<()>,
    #[tiger(offset = 0x80)]
    pub unk80: TagHash,
    pub unk84: TagHash,
}

/// The `0x808072B8` entity-local rigid-model component.  The fields below
/// replace the legacy anonymous reads from 0x1dc, 0x2d0 and 0x310.
#[derive(Debug, Clone)]
#[tiger_type(id = 0x808072B8, size = 0x320)]
pub struct SShadowkeepRigidModelComponent {
    #[tiger(offset = 0x1DC)]
    pub model: TagHash,
    #[tiger(offset = 0x2D0)]
    pub material_variants: Vec<SShadowkeepDynamicMaterialVariant>,
    #[tiger(offset = 0x310)]
    pub techniques: Vec<TagHash>,
}

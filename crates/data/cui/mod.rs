use std::fmt::Debug;

use glam::Vec4;
use tiger_parse::{
    tiger_type, tiger_variant_enum, FnvHash, OptionalVariantPointer, Padding, Pointer,
    TigerReadable, TigerReader,
};
use tiger_pkg::TagHash;

use crate::tag::WideHash;

#[derive(Debug)]
#[tiger_type(id = 0x80803DB2)]
pub struct SCuiScreen {
    pub file_size: u64,
    pub object_parents: Vec<SCuiObjectParent>,
    pub unk18: u16,
    pub unk1a: u16,
    pub unk1c: TagHash,
    pub unk20: u32,
}

#[derive(Debug)]
#[tiger_type(id = 0x80803A38)]
pub struct SCuiObjectParent {
    pub parent: i16,
    pub object: i16,
}

#[derive(Debug)]
#[tiger_type(id = 0x80803C6F)]
pub struct S80803C6F {
    pub file_size: u64,
    pub unk8: Vec<S80803A29>,
    pub objects: Vec<u32>,
    pub unk28: Vec<()>,
    pub components: Vec<SCuiComponent>,
    pub overlays: Vec<SCuiOverlay>,
    pub binding_set: Vec<S80803B10>,
    pub unk68: Vec<()>,
    pub unk78: Vec<S80803D8B>,
    // value_pool
    pub bool_values: VecPointer<bool>,
    pub int32_values: VecPointer<i32>,
    pub uint64_values: VecPointer<u64>,
    pub real32_values: VecPointer<S80803ABB>,
    pub prop_values_float2: VecPointer<S80803ABB>,
    pub vector4_values: VecPointer<Vec4>,
    pub string_hash_values: VecPointer<S80803AB9>,
    pub string_reference_values: VecPointer<()>,
    pub handle_values: VecPointer<S8080AF04>,
    pub component_values: VecPointer<S80803AB2>,
    pub object_values: Vec<()>,
    pub unk138_unlock_expression_values: Vec<()>,
    pub owner_pointer_values: Vec<S80803AAF>,
    pub unk158: Vec<()>,
}

#[derive(Debug)]
#[tiger_type(id = 0x80803A29)]
pub struct S80803A29 {
    pub sub_component: TagHash, // S80803C6F
    pub unk4: u16,
    pub unk6: u16,
    pub unk8: u16,
    pub unka: u16,
}

#[derive(Debug)]
#[tiger_type(id = 0x80803A44)]
pub struct SCuiComponent {
    pub unk0: u32,
    pub unk4: u32,
    pub unk8: u16,
    pub unka: u16,
    pub unkc: u32,
}

#[derive(Debug)]
#[tiger_type(id = 0x80803B33)]
pub struct S80803B33 {
    pub unk0: u64,
    pub unk8: TagHash, // 80809A31
    pub unkc: u32,
}

#[derive(Debug)]
#[tiger_type(id = 0x80803B2F)]
pub struct S80803B2F {
    pub unk: [u32; 8],
    pub unk0: Vec<S80803B33>,
}

#[derive(Debug)]
#[tiger_type(id = 0x80803B2D)]
pub struct S80803B2D {
    pub unk0: u32,
    padding: Padding<4>,
    pub properties: Vec<S80803B2F>,
    pub unk8: u64,
}

#[derive(Debug)]
#[tiger_type(id = 0x80803B2B)]
pub struct S80803B2B {
    pub hash: u32,
    padding: Padding<4>,
    pub components: Vec<S80803B2D>,
}

#[derive(Debug)]
#[tiger_type(id = 0x80803A4C)]
pub struct SCuiOverlay {
    pub unk0: [u32; 8],
    pub unk20: Vec<S80803B0C>,
    pub animations: Vec<S80803B2B>,
}

#[derive(Debug)]
#[tiger_type(id = 0x80803B0C)]
pub struct S80803B0C {
    pub unk0: u64,
    pub property_value_entries: Vec<S80803CA3>,
}

#[derive(Debug)]
#[tiger_type(id = 0x80803CA3)]
pub struct S80803CA3 {
    pub unk0: Pointer<S80803D8B>,
    pub unk8: u64,
    pub unk10: Pointer<()>, // TODO(cohae): This value can be several things, usually a float or a tag. The values themselves are located within an existing array
}

#[derive(Debug)]
#[tiger_type(id = 0x80803B10, size = 0x38)]
pub struct S80803B10 {
    pub unk0: u16,
    pub unk2: u16,
    pub unk4: u32,
    pub unk8: Pointer<S80803D8B>,
    pub unk10: u32,
    pub unk14: u32,
    pub unk18: u16,
    pub unk1a: u16,

    pub unk1c: u32,
    pub unk20: Pointer<S80803D8B>,
    pub unk28: u32,
    pub unk2c: u32,
    pub conversion: Pointer<()>,
}

#[derive(Debug)]
#[tiger_type(id = 0x80803ABB)]
pub struct S80803ABB {
    pub value: f32,
}

#[derive(Debug)]
#[tiger_type(id = 0x80803AB2)]
pub struct S80803AB2 {
    pub unk0: u16,
}

#[derive(Debug)]
#[tiger_type(id = 0x80803AB9)]
pub struct S80803AB9 {
    pub unk0: FnvHash,
}

#[derive(Debug)]
#[tiger_type(id = 0x80803D91)]
pub struct S80803D91 {
    pub unk0: u32,
}

#[derive(Debug)]
#[tiger_type(id = 0x80803D8B)]
pub struct S80803D8B {
    pub elements: Vec<S80803D91>,
}

#[derive(Debug)]
#[tiger_type(id = 0x8080AF04)]
pub struct S8080AF04 {
    pub h: WideHash, // bitmap (80804A69), STechnique or 80802A38 (contains localized strings and bitmaps?)
}

#[derive(Debug)]
#[tiger_type(id = 0x8080468d)]
pub struct S8080468D {}

#[derive(Debug)]
#[tiger_type(id = 0x80804834)]
pub struct S80804834 {
    pub columns: Vec<S80804837>,
    pub rows: Vec<S808048D1>,
}

#[derive(Debug)]
#[tiger_type(id = 0x8080490D)]
pub struct S8080490D {
    pub unk0: u64,
    pub steps: Vec<S8080490F>,
}

#[derive(Debug)]
#[tiger_type(id = 0x8080490F)]
pub struct S8080490F {
    pub unk0: u16,
    pub unk2: u16,
    pub unk4: f32,
    pub unk8: u32,
}

tiger_variant_enum! {
    #[derive(Debug)]
    [Unknown(true)]
    enum VisualComponentVariant {
    }
}

#[derive(Debug)]
#[tiger_type(id = 0x80803AAF)]
pub struct S80803AAF {
    pub unk0: u64,
    pub value: OptionalVariantPointer<VisualComponentVariant>,
}

#[derive(Debug)]
#[tiger_type(id = 0x80804837)]
pub struct S80804837 {
    pub unk0: [u32; 6],
}

#[derive(Debug)]
#[tiger_type(id = 0x808048df)]
pub struct S808048DF {}

#[derive(Debug)]
#[tiger_type(id = 0x808048d9)]
pub struct S808048D9 {
    pub unk0: [u32; 4],
    pub unk10: [u32; 4],
    pub color: Vec4,
}

#[derive(Debug)]
#[tiger_type(id = 0x808048d7)]
pub struct S808048D7 {}

#[derive(Debug)]
#[tiger_type(id = 0x808048d1)]
pub struct S808048D1 {
    pub unk0: Vec<S808048DF>,
    pub unk10: [u32; 4],
    pub unk20: [u32; 4],
    pub unk30: Vec<S808048D9>,
    pub unk40: Vec<S808048D7>,
    pub unk50: [u32; 4],
    pub unk60: [u32; 4],
}

/// Convenience wrapper for tiger-parse Vecs that expose the absolute address of the vector
pub struct VecPointer<T> {
    vec: Vec<T>,
    addr: u64,
}

impl<T> VecPointer<T> {
    pub fn addr(&self) -> u64 {
        self.addr
    }

    pub fn len(&self) -> usize {
        self.vec.len()
    }

    pub fn is_empty(&self) -> bool {
        self.vec.is_empty()
    }
}

impl<T: TigerReadable> TigerReadable for VecPointer<T> {
    const SIZE: usize = 16;

    fn read_ds_endian(
        reader: &mut dyn TigerReader,
        endian: tiger_parse::Endian,
    ) -> tiger_parse::Result<Self> {
        reader.seek(std::io::SeekFrom::Current(8))?;
        let addr = reader.stream_position()? + u64::read_ds_endian(reader, endian)?;
        reader.seek(std::io::SeekFrom::Current(-16))?;

        let vec = Vec::read_ds_endian(reader, endian)?;
        Ok(VecPointer { vec, addr })
    }
}

impl<T: Debug> Debug for VecPointer<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.vec.fmt(f)
    }
}

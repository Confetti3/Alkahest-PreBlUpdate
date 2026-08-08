//! Texture layouts used by the Shadowkeep / Arrivals package format.
//!
//! These headers predate the `0x40` post-BL header.  Keeping the layout here
//! prevents legacy package bytes from being interpreted as later-era fields.

use tiger_parse::tiger_type;
use tiger_pkg::TagHash;

use crate::tfx::texture::DxgiFormat;

#[derive(Debug)]
#[tiger_type(etype = 32, size = 0x28)]
pub struct SShadowkeepTextureHeader {
    pub data_size: u32,
    pub format: DxgiFormat,
    pub _unk8: u32,

    pub cafe: u16,
    pub width: u16,
    pub height: u16,
    pub depth: u16,
    pub array_size: u16,
    pub _unk16: u8,
    pub mip_count: u8,
    pub _unk18: [u8; 12],

    /// Optional high-resolution mip data.
    pub large_buffer: TagHash,
}

#[cfg(test)]
mod tests {
    use super::SShadowkeepTextureHeader;

    #[test]
    fn header_keeps_the_preserved_shadowkeep_size() {
        assert_eq!(std::mem::size_of::<SShadowkeepTextureHeader>(), 0x28);
    }
}

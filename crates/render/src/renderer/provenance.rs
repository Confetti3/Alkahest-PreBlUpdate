use std::{fs, path::Path};

use anyhow::{Context, bail};
use d3d11::{BindFlags, CpuAccessFlags, Texture2dDesc, dxgi};
use serde::Serialize;
use sha2::{Digest, Sha256};

use super::surface::Surface;
use crate::gpu::command_list::CommandList;

#[derive(Debug, Serialize)]
pub struct SurfaceProvenance {
    pub surface: String,
    pub file: String,
    pub format: String,
    pub resource_format: String,
    pub width: u32,
    pub height: u32,
    pub finite_pixel_count: u64,
    pub nonzero_rgb_pixel_count: u64,
    pub minimum_rgb: Option<[f64; 3]>,
    pub maximum_rgb: Option<[f64; 3]>,
    pub mean_rgb: Option<[f64; 3]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nonzero_alpha_count: Option<u64>,
    pub sha256: String,
}

#[derive(Debug, Serialize)]
pub struct DeferredShadingProvenance {
    pub technique: String,
    pub vertex_shader: Option<String>,
    pub pixel_shader: Option<String>,
    pub draw_6_reached: bool,
    pub vertex_expression: Option<String>,
    pub pixel_expression: Option<String>,
    pub vertex_constant_buffer_slot: Option<u32>,
    pub vertex_constant_buffer_len: Option<usize>,
    pub pixel_constant_buffer_slot: Option<u32>,
    pub pixel_constant_buffer_len: Option<usize>,
    pub bound_deferred_srvs: Vec<String>,
    pub output_rtv_format: String,
}

#[derive(Debug, Serialize)]
pub struct ProvenanceManifest {
    pub schema: &'static str,
    pub deferred_shading: DeferredShadingProvenance,
    pub captures: Vec<SurfaceProvenance>,
}

impl ProvenanceManifest {
    pub fn write(&self, directory: &Path) -> anyhow::Result<()> {
        let json =
            serde_json::to_vec_pretty(self).context("Failed to serialize provenance manifest")?;
        fs::write(directory.join("manifest.json"), json)
            .context("Failed to write provenance manifest")
    }
}

pub fn capture_surface(
    cmd: &CommandList,
    surface: &Surface,
    directory: &Path,
) -> anyhow::Result<SurfaceProvenance> {
    fs::create_dir_all(directory).context("Failed to create provenance directory")?;

    let desc = surface.texture.get_desc();
    let format = desc.format;
    let bytes_per_pixel = bytes_per_pixel(format)?;
    let staging = cmd.gpu().device.create_texture2d(
        &Texture2dDesc::builder()
            .width(desc.width)
            .height(desc.height)
            .mip_levels(1)
            .array_size(1)
            .format(format)
            .usage(d3d11::Usage::Staging)
            .bind_flags(BindFlags::empty())
            .cpu_access_flags(CpuAccessFlags::READ)
            .build(),
        None,
    )?;
    cmd.copy_resource(&surface.texture, &staging);

    let map = cmd
        .map(&staging, 0, d3d11::MapType::Read, false)
        .context("Failed to map provenance staging texture")?;
    let tight_row_pitch = desc.width as usize * bytes_per_pixel;
    let mut bytes = Vec::with_capacity(tight_row_pitch * desc.height as usize);
    for y in 0..desc.height as usize {
        let row = unsafe {
            std::slice::from_raw_parts(
                map.data.cast::<u8>().add(y * map.row_pitch as usize),
                tight_row_pitch,
            )
        };
        bytes.extend_from_slice(row);
    }
    drop(map);

    let filename = format!("{}.bin", surface.name());
    fs::write(directory.join(&filename), &bytes)
        .with_context(|| format!("Failed to write provenance capture {filename}"))?;
    let stats = compute_stats(format, &bytes)?;

    Ok(SurfaceProvenance {
        surface: surface.name().to_owned(),
        file: filename,
        format: format!(
            "{:?}",
            surface
                .desc()
                .depth_format
                .unwrap_or(surface.desc().view_format)
        ),
        resource_format: format!("{format:?}"),
        width: desc.width,
        height: desc.height,
        finite_pixel_count: stats.finite_pixel_count,
        nonzero_rgb_pixel_count: stats.nonzero_rgb_pixel_count,
        minimum_rgb: stats.minimum_rgb,
        maximum_rgb: stats.maximum_rgb,
        mean_rgb: stats.mean_rgb,
        nonzero_alpha_count: stats.nonzero_alpha_count,
        sha256: format!("{:x}", Sha256::digest(&bytes)),
    })
}

#[derive(Debug, PartialEq)]
struct PixelStats {
    finite_pixel_count: u64,
    nonzero_rgb_pixel_count: u64,
    minimum_rgb: Option<[f64; 3]>,
    maximum_rgb: Option<[f64; 3]>,
    mean_rgb: Option<[f64; 3]>,
    nonzero_alpha_count: Option<u64>,
}

fn compute_stats(format: dxgi::Format, bytes: &[u8]) -> anyhow::Result<PixelStats> {
    let bytes_per_pixel = bytes_per_pixel(format)?;
    if !bytes.len().is_multiple_of(bytes_per_pixel) {
        bail!("Capture byte length is not aligned to format {format:?}");
    }

    let has_alpha = format_has_alpha(format);
    let mut finite_pixel_count = 0u64;
    let mut nonzero_rgb_pixel_count = 0u64;
    let mut nonzero_alpha_count = 0u64;
    let mut minimum_rgb = [f64::INFINITY; 3];
    let mut maximum_rgb = [f64::NEG_INFINITY; 3];
    let mut sum_rgb = [0.0f64; 3];

    for encoded in bytes.chunks_exact(bytes_per_pixel) {
        let [r, g, b, a] = decode_pixel(format, encoded)?;
        if (r.is_finite() && r != 0.0) || (g.is_finite() && g != 0.0) || (b.is_finite() && b != 0.0)
        {
            nonzero_rgb_pixel_count += 1;
        }
        if has_alpha && a.is_finite() && a != 0.0 {
            nonzero_alpha_count += 1;
        }
        if r.is_finite() && g.is_finite() && b.is_finite() {
            finite_pixel_count += 1;
            for (index, value) in [r, g, b].into_iter().enumerate() {
                let value = value as f64;
                minimum_rgb[index] = minimum_rgb[index].min(value);
                maximum_rgb[index] = maximum_rgb[index].max(value);
                sum_rgb[index] += value;
            }
        }
    }

    let (minimum_rgb, maximum_rgb, mean_rgb) = if finite_pixel_count == 0 {
        (None, None, None)
    } else {
        (
            Some(minimum_rgb),
            Some(maximum_rgb),
            Some(sum_rgb.map(|sum| sum / finite_pixel_count as f64)),
        )
    };

    Ok(PixelStats {
        finite_pixel_count,
        nonzero_rgb_pixel_count,
        minimum_rgb,
        maximum_rgb,
        mean_rgb,
        nonzero_alpha_count: has_alpha.then_some(nonzero_alpha_count),
    })
}

fn bytes_per_pixel(format: dxgi::Format) -> anyhow::Result<usize> {
    match format {
        dxgi::Format::R8g8b8a8Typeless
        | dxgi::Format::R8g8b8a8Unorm
        | dxgi::Format::R8g8b8a8UnormSrgb
        | dxgi::Format::R10g10b10a2Typeless
        | dxgi::Format::R10g10b10a2Unorm
        | dxgi::Format::R11g11b10Float => Ok(4),
        dxgi::Format::R16g16b16a16Typeless | dxgi::Format::R16g16b16a16Float => Ok(8),
        dxgi::Format::R32g8x24Typeless => Ok(8),
        _ => bail!("Unsupported provenance format {format:?}"),
    }
}

fn format_has_alpha(format: dxgi::Format) -> bool {
    matches!(
        format,
        dxgi::Format::R8g8b8a8Typeless
            | dxgi::Format::R8g8b8a8Unorm
            | dxgi::Format::R8g8b8a8UnormSrgb
            | dxgi::Format::R10g10b10a2Typeless
            | dxgi::Format::R10g10b10a2Unorm
            | dxgi::Format::R16g16b16a16Typeless
            | dxgi::Format::R16g16b16a16Float
    )
}

fn decode_pixel(format: dxgi::Format, bytes: &[u8]) -> anyhow::Result<[f32; 4]> {
    let packed = || u32::from_le_bytes(bytes[..4].try_into().unwrap());
    match format {
        dxgi::Format::R8g8b8a8Typeless
        | dxgi::Format::R8g8b8a8Unorm
        | dxgi::Format::R8g8b8a8UnormSrgb => Ok([
            bytes[0] as f32 / 255.0,
            bytes[1] as f32 / 255.0,
            bytes[2] as f32 / 255.0,
            bytes[3] as f32 / 255.0,
        ]),
        dxgi::Format::R10g10b10a2Typeless | dxgi::Format::R10g10b10a2Unorm => {
            let value = packed();
            Ok([
                (value & 0x3ff) as f32 / 1023.0,
                ((value >> 10) & 0x3ff) as f32 / 1023.0,
                ((value >> 20) & 0x3ff) as f32 / 1023.0,
                ((value >> 30) & 0x3) as f32 / 3.0,
            ])
        }
        dxgi::Format::R11g11b10Float => {
            let value = packed();
            Ok([
                decode_unsigned_float(value & 0x7ff, 6),
                decode_unsigned_float((value >> 11) & 0x7ff, 6),
                decode_unsigned_float((value >> 22) & 0x3ff, 5),
                0.0,
            ])
        }
        dxgi::Format::R16g16b16a16Typeless | dxgi::Format::R16g16b16a16Float => Ok([
            decode_f16(u16::from_le_bytes(bytes[0..2].try_into().unwrap())),
            decode_f16(u16::from_le_bytes(bytes[2..4].try_into().unwrap())),
            decode_f16(u16::from_le_bytes(bytes[4..6].try_into().unwrap())),
            decode_f16(u16::from_le_bytes(bytes[6..8].try_into().unwrap())),
        ]),
        dxgi::Format::R32g8x24Typeless => {
            let depth = f32::from_le_bytes(bytes[..4].try_into().unwrap());
            Ok([depth, depth, depth, 0.0])
        }
        _ => bail!("Unsupported provenance format {format:?}"),
    }
}

fn decode_unsigned_float(bits: u32, mantissa_bits: u32) -> f32 {
    let mantissa_mask = (1 << mantissa_bits) - 1;
    let exponent = bits >> mantissa_bits;
    let mantissa = bits & mantissa_mask;
    match exponent {
        0 if mantissa == 0 => 0.0,
        0 => (mantissa as f32 / (1 << mantissa_bits) as f32) * 2f32.powi(-14),
        31 if mantissa == 0 => f32::INFINITY,
        31 => f32::NAN,
        _ => {
            (1.0 + mantissa as f32 / (1 << mantissa_bits) as f32) * 2f32.powi(exponent as i32 - 15)
        }
    }
}

fn decode_f16(bits: u16) -> f32 {
    let sign = ((bits >> 15) as u32) << 31;
    let exponent = ((bits >> 10) & 0x1f) as u32;
    let mantissa = (bits & 0x3ff) as u32;
    let converted = match exponent {
        0 if mantissa == 0 => sign,
        0 => {
            let shift = mantissa.leading_zeros() - 21;
            let normalized = (mantissa << shift) & 0x3ff;
            sign | ((113 - shift) << 23) | (normalized << 13)
        }
        31 => sign | 0x7f80_0000 | (mantissa << 13),
        _ => sign | ((exponent + 112) << 23) | (mantissa << 13),
    };
    f32::from_bits(converted)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn computes_unorm_statistics_and_alpha_count() {
        let stats =
            compute_stats(dxgi::Format::R8g8b8a8Unorm, &[0, 0, 0, 0, 255, 128, 0, 255]).unwrap();
        assert_eq!(stats.finite_pixel_count, 2);
        assert_eq!(stats.nonzero_rgb_pixel_count, 1);
        assert_eq!(stats.minimum_rgb, Some([0.0; 3]));
        assert_eq!(
            stats.maximum_rgb,
            Some([1.0, (128.0f32 / 255.0) as f64, 0.0])
        );
        assert_eq!(stats.nonzero_alpha_count, Some(1));
    }

    #[test]
    fn decodes_half_float_alpha() {
        let pixel = decode_pixel(
            dxgi::Format::R16g16b16a16Float,
            &[0x00, 0x3c, 0x00, 0xc0, 0x00, 0x00, 0x00, 0x38],
        )
        .unwrap();
        assert_eq!(pixel, [1.0, -2.0, 0.0, 0.5]);
    }

    #[test]
    fn decodes_r11g11b10_one() {
        let one_r11 = 15u32 << 6;
        let one_g11 = (15u32 << 6) << 11;
        let one_b10 = (15u32 << 5) << 22;
        let pixel = decode_pixel(
            dxgi::Format::R11g11b10Float,
            &(one_r11 | one_g11 | one_b10).to_le_bytes(),
        )
        .unwrap();
        assert_eq!(pixel, [1.0, 1.0, 1.0, 0.0]);
    }
}

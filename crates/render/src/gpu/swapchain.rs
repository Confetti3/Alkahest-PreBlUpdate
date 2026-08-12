use std::time::Duration;

use anyhow::Context;
use d3d11::dxgi::{self, PresentFlags, SwapChainFlags, SwapChainStatus};

pub const SWAPCHAIN_FORMAT: dxgi::Format = dxgi::Format::B8g8r8a8Unorm;
pub const BUFFER_COUNT: u32 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PresentOutcome {
    Presented,
    Occluded,
}

pub struct Swapchain {
    pub swapchain: d3d11::dxgi::SwapChain,
    pub swapchain_target: Option<d3d11::RenderTargetView>,
    pub(crate) swapchain_resolution: (u32, u32),
    present_parameters: PresentFlags,
}

impl Swapchain {
    pub fn new(
        swapchain: d3d11::dxgi::SwapChain,
        device: &d3d11::Device,
        size: (u32, u32),
    ) -> anyhow::Result<Self> {
        let mut result = Self {
            swapchain,
            swapchain_target: None,
            swapchain_resolution: size,
            present_parameters: PresentFlags::empty(),
        };
        result.recreate_target(device)?;
        Ok(result)
    }

    pub fn get_buffer(&self) -> d3d11::Result<d3d11::Texture2D> {
        self.swapchain.get_buffer(0)
    }

    fn recreate_target(&mut self, device: &d3d11::Device) -> anyhow::Result<()> {
        let backbuffer: d3d11::Texture2D = self
            .swapchain
            .get_buffer(0)
            .context("Acquiring swap-chain back buffer")?;
        self.swapchain_target = Some(
            device
                .create_render_target_view(&backbuffer, None)
                .context("Creating swap-chain render-target view")?,
        );
        Ok(())
    }

    /// The caller must release every other back-buffer reference first.
    pub fn resize(&mut self, device: &d3d11::Device, new_size: (u32, u32)) -> anyhow::Result<bool> {
        if new_size.0 == 0 || new_size.1 == 0 {
            return Ok(false);
        }
        if new_size == self.swapchain_resolution {
            return Ok(false);
        }

        drop(self.swapchain_target.take());
        self.swapchain
            .resize_buffers(
                BUFFER_COUNT,
                new_size.0,
                new_size.1,
                SWAPCHAIN_FORMAT,
                SwapChainFlags::empty(),
            )
            .context("Resizing swap-chain buffers")?;
        self.recreate_target(device)?;
        self.swapchain_resolution = new_size;
        Ok(true)
    }

    pub(crate) fn present(&mut self, vsync: bool) -> PresentOutcome {
        if self
            .swapchain
            .present(vsync as u32, self.present_parameters)
            == Some(SwapChainStatus::Occluded)
        {
            self.present_parameters = PresentFlags::TEST;
            std::thread::sleep(Duration::from_millis(50));
            PresentOutcome::Occluded
        } else {
            self.present_parameters = PresentFlags::empty();
            PresentOutcome::Presented
        }
    }
}

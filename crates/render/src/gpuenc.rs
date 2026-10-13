//! GPU upload path for hardware encoders: owns a D3D11 device on the encoder's adapter, uploads
//! each composed BGRA frame to a reusable texture (`UpdateSubresource`, as the live recorder in
//! `crates/encode/src/native.rs` does) and converts it to NV12 on the GPU with the D3D11 video
//! processor (`clipcheck_gpuconv`). The NV12 texture goes to `InputFrame::Texture`.

use clipcheck_gpuconv::d3d::{create_device, GpuConverter};
use clipcheck_hwenc::Vendor;
use windows::core::Interface;
use windows::Win32::Graphics::Direct3D11::{
    ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D, D3D11_BIND_RENDER_TARGET,
    D3D11_BIND_SHADER_RESOURCE, D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC};
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory1, IDXGIAdapter, IDXGIFactory1, DXGI_ADAPTER_DESC1, DXGI_ADAPTER_FLAG_SOFTWARE,
};

use crate::error::RenderError;

/// NV12 textures cycled through, so the encoder can still hold earlier frames.
const POOL_SIZE: usize = 8;

/// What DXGI reports about the GPU adapter of a vendor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdapterInfo {
    /// Adapter description (DXGI `Description`), trimmed.
    pub name: String,
    /// Dedicated video memory in bytes.
    pub dedicated_vram: u64,
    /// True for a software adapter. [`adapter_info`] never returns one, so this is false there.
    pub is_software: bool,
}

fn gerr(context: &str, e: impl std::fmt::Display) -> RenderError {
    RenderError::Encode(format!("{context}: {e}"))
}

/// PCI vendor id of `vendor`; None for the software and unknown vendors.
fn pci_vendor_id(vendor: Vendor) -> Option<u32> {
    match vendor {
        Vendor::Nvidia => Some(0x10DE),
        Vendor::Amd => Some(0x1002),
        Vendor::Intel => Some(0x8086),
        Vendor::Microsoft | Vendor::Other => None,
    }
}

/// The hardware adapter with `vendor`'s PCI id that has the most dedicated video memory (the
/// first one wins a tie). Software adapters are skipped.
fn best_adapter(vendor: Vendor) -> Option<(IDXGIAdapter, DXGI_ADAPTER_DESC1)> {
    let id = pci_vendor_id(vendor)?;
    // SAFETY: plain factory creation; no pointers are passed.
    let factory: IDXGIFactory1 = unsafe { CreateDXGIFactory1() }.ok()?;
    let software_flag = DXGI_ADAPTER_FLAG_SOFTWARE.0 as u32;
    let mut best: Option<(IDXGIAdapter, DXGI_ADAPTER_DESC1)> = None;
    let mut i = 0;
    // SAFETY: EnumAdapters1 returns an owned COM reference, or an error at the end of the list.
    while let Ok(adapter) = unsafe { factory.EnumAdapters1(i) } {
        i += 1;
        // SAFETY: adapter is a live adapter.
        let Ok(desc) = (unsafe { adapter.GetDesc1() }) else {
            continue;
        };
        if desc.VendorId != id || (desc.Flags & software_flag) != 0 {
            continue;
        }
        let better = match &best {
            Some((_, current)) => desc.DedicatedVideoMemory > current.DedicatedVideoMemory,
            None => true,
        };
        if better {
            if let Ok(a) = adapter.cast::<IDXGIAdapter>() {
                best = Some((a, desc));
            }
        }
    }
    best
}

/// The DXGI adapter for `vendor` (the one with the most dedicated VRAM), if there is one.
fn adapter_for(vendor: Vendor) -> Option<IDXGIAdapter> {
    best_adapter(vendor).map(|(adapter, _)| adapter)
}

/// Name and memory of the GPU adapter for `vendor`, or None when there is no such adapter (or
/// the vendor is software or unknown). With several adapters of one vendor, the one with the most
/// dedicated VRAM is reported, the same one [`GpuUploader`] uses.
pub fn adapter_info(vendor: Vendor) -> Option<AdapterInfo> {
    let (_, desc) = best_adapter(vendor)?;
    let end = desc
        .Description
        .iter()
        .position(|&c| c == 0)
        .unwrap_or(desc.Description.len());
    let name = String::from_utf16_lossy(desc.Description.get(..end).unwrap_or(&[]));
    Some(AdapterInfo {
        name: name.trim().to_string(),
        dedicated_vram: desc.DedicatedVideoMemory as u64,
        is_software: false,
    })
}

/// Uploads BGRA frames and converts them to NV12 textures on the encoder's D3D11 device.
pub struct GpuUploader {
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    converter: GpuConverter,
    bgra: ID3D11Texture2D,
    pool: Vec<ID3D11Texture2D>,
    next: usize,
    width: u32,
    height: u32,
}

impl GpuUploader {
    /// Creates a device (on the adapter matching `vendor`, else the default one) and the textures
    /// for `width` x `height` frames (even, non-zero).
    pub fn new(vendor: Vendor, width: u32, height: u32) -> Result<Self, RenderError> {
        let adapter = adapter_for(vendor);
        let (device, context) = match create_device(adapter.as_ref()) {
            Ok(v) => v,
            Err(e) if adapter.is_some() => {
                let _ = e;
                create_device(None).map_err(|e| gerr("create D3D11 device", e))?
            }
            Err(e) => return Err(gerr("create D3D11 device", e)),
        };
        let converter = GpuConverter::new(&device, width, height)
            .map_err(|e| gerr("create GPU converter", e))?;
        let mut pool = Vec::with_capacity(POOL_SIZE);
        for _ in 0..POOL_SIZE {
            pool.push(
                converter
                    .create_nv12_texture()
                    .map_err(|e| gerr("create NV12 texture", e))?,
            );
        }
        let bgra = create_bgra(&device, width, height)?;
        Ok(Self {
            device,
            context,
            converter,
            bgra,
            pool,
            next: 0,
            width,
            height,
        })
    }

    /// The device that owns the textures; pass it to `clipcheck_hwenc::open`.
    pub fn device(&self) -> &ID3D11Device {
        &self.device
    }

    /// Uploads one tightly packed BGRA frame and returns the NV12 texture holding it. The texture
    /// stays valid until `POOL_SIZE` further calls.
    pub fn upload(&mut self, bgra: &[u8]) -> Result<&ID3D11Texture2D, RenderError> {
        let expected = (self.width as usize)
            .saturating_mul(self.height as usize)
            .saturating_mul(4);
        if bgra.len() != expected {
            return Err(RenderError::Encode(format!(
                "frame has {} bytes, expected {expected}",
                bgra.len()
            )));
        }
        // SAFETY: self.bgra is a live DEFAULT BGRA texture of width x height; bgra holds exactly
        // width * height * 4 bytes (checked above), so the row pitch width * 4 stays in range.
        unsafe {
            self.context.UpdateSubresource(
                &self.bgra,
                0,
                None,
                bgra.as_ptr().cast(),
                self.width * 4,
                0,
            );
        }
        let slot = self.next % self.pool.len().max(1);
        self.next = self.next.wrapping_add(1);
        let dst = self
            .pool
            .get(slot)
            .ok_or_else(|| RenderError::Encode("NV12 pool is empty".to_string()))?;
        self.converter
            .convert(&self.bgra, dst)
            .map_err(|e| gerr("convert BGRA to NV12", e))?;
        Ok(dst)
    }
}

/// A DEFAULT BGRA texture usable as a converter source.
fn create_bgra(
    device: &ID3D11Device,
    width: u32,
    height: u32,
) -> Result<ID3D11Texture2D, RenderError> {
    let desc = D3D11_TEXTURE2D_DESC {
        Width: width,
        Height: height,
        MipLevels: 1,
        ArraySize: 1,
        Format: DXGI_FORMAT_B8G8R8A8_UNORM,
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        Usage: D3D11_USAGE_DEFAULT,
        BindFlags: (D3D11_BIND_SHADER_RESOURCE.0 | D3D11_BIND_RENDER_TARGET.0) as u32,
        CPUAccessFlags: 0,
        MiscFlags: 0,
    };
    let mut texture: Option<ID3D11Texture2D> = None;
    // SAFETY: desc is a valid local descriptor; the out-pointer references a live Option slot
    // that the API fills with a COM reference on success.
    unsafe { device.CreateTexture2D(&desc as *const _, None, Some(&mut texture as *mut _)) }
        .map_err(|e| gerr("CreateTexture2D BGRA", e))?;
    texture.ok_or_else(|| RenderError::Encode("CreateTexture2D returned no texture".to_string()))
}

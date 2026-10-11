//! GPU capture mode (D57): frames stay D3D11 textures; no CPU readback (worker C1).

use windows::Win32::Foundation::E_POINTER;
use windows::Win32::Graphics::Direct3D11::{
    ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D, D3D11_BIND_RENDER_TARGET,
    D3D11_BIND_SHADER_RESOURCE, D3D11_BOX, D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC};

/// A D3D11 texture handed between threads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SharedTexture(pub ID3D11Texture2D);

// SAFETY: D3D11 resources are free-threaded; the device is multithread protected, so a texture
// reference may be used from any thread.
unsafe impl Send for SharedTexture {}
// SAFETY: as above; the wrapper exposes no interior mutability of its own.
unsafe impl Sync for SharedTexture {}

/// One GPU frame from [`crate::WindowCapturer::start_gpu`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GpuFrame {
    /// BGRA copy of the window content (`width x height`), owned by the capture's texture ring.
    /// `None` for black frames.
    pub texture: Option<SharedTexture>,
    /// Width of the content in pixels.
    pub width: u32,
    /// Height of the content in pixels.
    pub height: u32,
    /// Presentation time in 100 ns units since capture start (QueryPerformanceCounter).
    pub pts_100ns: i64,
    /// True for black frames emitted while the window is hidden.
    pub is_black: bool,
}

/// Textures in the ring. Each accepted frame is copied into the next slot, so a slot is reused
/// after this many newer frames.
const RING_SLOTS: usize = 8;

/// Ring of BGRA textures that receive window frames. Rebuilt when the content size changes.
pub(crate) struct TextureRing {
    device: ID3D11Device,
    slots: Vec<ID3D11Texture2D>,
    width: u32,
    height: u32,
    next: usize,
}

impl TextureRing {
    /// An empty ring on `device`; slots are created on the first [`TextureRing::acquire`].
    pub(crate) fn new(device: ID3D11Device) -> Self {
        TextureRing {
            device,
            slots: Vec::new(),
            width: 0,
            height: 0,
            next: 0,
        }
    }

    /// Returns the next slot as a `width x height` texture and advances the ring. The ring is
    /// rebuilt first when the size differs from the current slots.
    pub(crate) fn acquire(
        &mut self,
        width: u32,
        height: u32,
    ) -> Result<ID3D11Texture2D, windows::core::Error> {
        if self.slots.is_empty() || self.width != width || self.height != height {
            let mut slots = Vec::with_capacity(RING_SLOTS);
            for _ in 0..RING_SLOTS {
                slots.push(create_slot(&self.device, width, height)?);
            }
            self.slots = slots;
            self.width = width;
            self.height = height;
            self.next = 0;
        }
        let tex = self.slots[self.next].clone();
        self.next = (self.next + 1) % self.slots.len();
        Ok(tex)
    }
}

/// Creates one default-usage BGRA texture that can be read by shaders and used as a target.
fn create_slot(
    device: &ID3D11Device,
    width: u32,
    height: u32,
) -> Result<ID3D11Texture2D, windows::core::Error> {
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
    let mut out: Option<ID3D11Texture2D> = None;
    // SAFETY: desc is a valid descriptor; out receives a new texture on success.
    unsafe { device.CreateTexture2D(&desc, None, Some(&mut out as *mut _))? };
    out.ok_or_else(|| windows::core::Error::from(E_POINTER))
}

/// Copies the top-left `w x h` region of `src` into `dst` on the GPU (no CPU access).
/// The caller guarantees `w x h` fits inside `src` and matches `dst`.
pub(crate) fn copy_top_left(
    context: &ID3D11DeviceContext,
    dst: &ID3D11Texture2D,
    src: &ID3D11Texture2D,
    w: u32,
    h: u32,
) {
    let region = D3D11_BOX {
        left: 0,
        top: 0,
        front: 0,
        right: w,
        bottom: h,
        back: 1,
    };
    // SAFETY: dst and src are textures on the same device with the same format; region lies
    // inside the top-left w x h of src and inside dst, as the caller guarantees.
    unsafe {
        context.CopySubresourceRegion(dst, 0, 0, 0, 0, src, 0, Some(&region as *const D3D11_BOX))
    };
}

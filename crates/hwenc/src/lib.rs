//! clipcheck-hwenc: hardware (and Microsoft software) video encoders through Media Foundation,
//! in process, fed with D3D11 NV12 textures (D57). Contracts: docs/impl/native-encoder.md.

mod types;
pub use types::*;

#[cfg(windows)]
mod mf_async;
#[cfg(windows)]
mod mf_common;
#[cfg(windows)]
mod mf_enum;
#[cfg(windows)]
mod mf_sync;

#[cfg(windows)]
pub use win::*;

#[cfg(windows)]
mod win {
    use windows::Win32::Graphics::Direct3D11::{ID3D11Device, ID3D11Texture2D};

    use crate::{EncError, EncodedPacket, EncoderDescriptor, VideoEncoderConfig};

    /// One frame handed to an encoder.
    pub enum InputFrame<'a> {
        /// NV12 texture on the encoder's D3D11 device (hardware encoders).
        Texture(&'a ID3D11Texture2D),
        /// NV12 in system memory, `width * height * 3 / 2` bytes (software encoder).
        Nv12 {
            /// Pixel data.
            data: &'a [u8],
            /// Width in pixels.
            width: u32,
            /// Height in pixels.
            height: u32,
        },
    }

    /// A running encoder session.
    pub trait VideoEncoder: Send {
        /// Submits one frame at `pts` (100 ns) and returns every packet that became ready.
        fn encode(
            &mut self,
            frame: InputFrame<'_>,
            pts: i64,
            force_keyframe: bool,
        ) -> Result<Vec<EncodedPacket>, EncError>;
        /// Drains the encoder and returns the remaining packets.
        fn flush(&mut self) -> Result<Vec<EncodedPacket>, EncError>;
        /// What this encoder is.
        fn descriptor(&self) -> &EncoderDescriptor;
    }

    /// Calls `MFStartup` once per process (never shut down).
    pub fn mf_startup() -> Result<(), EncError> {
        crate::mf_enum::startup()
    }

    /// Encoders for `codec`: hardware first, then Microsoft software.
    pub fn enumerate(codec: crate::Codec) -> Result<Vec<EncoderDescriptor>, EncError> {
        crate::mf_enum::enumerate(codec)
    }

    /// Opens `desc`. Hardware encoders need `device` (the device that owns the input textures).
    pub fn open(
        desc: &EncoderDescriptor,
        device: Option<&ID3D11Device>,
        cfg: &VideoEncoderConfig,
    ) -> Result<Box<dyn VideoEncoder>, EncError> {
        if desc.hardware {
            let device = device.ok_or_else(|| {
                EncError::BadInput("a hardware encoder needs a D3D11 device".into())
            })?;
            Ok(Box::new(crate::mf_async::MfAsyncEncoder::open(
                desc, device, cfg,
            )?))
        } else {
            Ok(Box::new(crate::mf_sync::MfSyncEncoder::open(desc, cfg)?))
        }
    }
}

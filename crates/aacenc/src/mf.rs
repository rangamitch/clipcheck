//! Microsoft AAC encoder MFT wrapper (worker A1).
//!
//! Synchronous MFT, in process. The encoder is opened with the output type first
//! (AAC-LC, raw payload), then the PCM input type, then the streaming notifications.

use std::mem::ManuallyDrop;
use std::ptr::null_mut;
use std::sync::OnceLock;

use windows::core::{Error as WinError, IUnknown, GUID};
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::{CoCreateInstance, CoIncrementMTAUsage, CLSCTX_INPROC_SERVER};

use crate::{AacError, AacPacket};

/// CLSID of the Microsoft AAC encoder MFT.
const ENCODER_CLSID: GUID = GUID::from_u128(0x93af0c51_2275_45d2_a35b_f2ba21caed00);
/// PCM samples per channel in one AAC-LC frame.
const FRAME_SAMPLES: u32 = 1024;
/// 100 ns units per second.
const HNS_PER_SEC: i64 = 10_000_000;
/// Size of the HEAACWAVEINFO header that precedes the AudioSpecificConfig in the user-data blob.
const HEAAC_HEADER_LEN: usize = 12;
/// Allowed AAC byte rates (bytes per second), i.e. 96, 128, 160 and 192 kbit/s.
const BYTE_RATES: [u32; 4] = [12_000, 16_000, 20_000, 24_000];

/// AAC-LC encoder for interleaved f32 PCM.
pub struct AacEncoder {
    transform: IMFTransform,
    asc: Vec<u8>,
    sample_rate: u32,
    channels: usize,
    /// Output buffer size the MFT asks for (cbSize of GetOutputStreamInfo).
    out_size: u32,
    /// Presentation time of the first pushed sample; packets are counted from it.
    first_pts: Option<i64>,
    /// Number of packets produced so far (for pts computation).
    packets_out: u64,
}

impl AacEncoder {
    /// Opens the encoder for `sample_rate` (48 000 or 44 100) and `channels` (1 or 2).
    pub fn new(sample_rate: u32, channels: u16, bitrate_kbps: u32) -> Result<Self, AacError> {
        if sample_rate != 44_100 && sample_rate != 48_000 {
            return Err(AacError::BadInput(format!(
                "sample rate {sample_rate} is not 44100 or 48000"
            )));
        }
        if channels != 1 && channels != 2 {
            return Err(AacError::BadInput(format!(
                "{channels} channels is not 1 or 2"
            )));
        }
        startup()?;

        let target = i64::from(bitrate_kbps) * 1000 / 8;
        let mut byte_rate = BYTE_RATES[0];
        for rate in BYTE_RATES {
            if (i64::from(rate) - target).abs() < (i64::from(byte_rate) - target).abs() {
                byte_rate = rate;
            }
        }

        let transform: IMFTransform = unsafe {
            // SAFETY: ENCODER_CLSID names the in-box AAC encoder MFT, which implements IMFTransform.
            CoCreateInstance::<_, IMFTransform>(
                &ENCODER_CLSID,
                None::<&IUnknown>,
                CLSCTX_INPROC_SERVER,
            )
        }
        .map_err(|e| win_err("CoCreateInstance", e))?;

        // Output type first: the AAC encoder requires it before the input type.
        let out_type = new_media_type()?;
        set_guid(&out_type, &MF_MT_MAJOR_TYPE, &MFMediaType_Audio)?;
        set_guid(&out_type, &MF_MT_SUBTYPE, &MFAudioFormat_AAC)?;
        set_u32(&out_type, &MF_MT_AUDIO_BITS_PER_SAMPLE, 16)?;
        set_u32(&out_type, &MF_MT_AUDIO_SAMPLES_PER_SECOND, sample_rate)?;
        set_u32(&out_type, &MF_MT_AUDIO_NUM_CHANNELS, u32::from(channels))?;
        set_u32(&out_type, &MF_MT_AUDIO_AVG_BYTES_PER_SECOND, byte_rate)?;
        set_u32(&out_type, &MF_MT_AAC_PAYLOAD_TYPE, 0)?;
        set_u32(&out_type, &MF_MT_AAC_AUDIO_PROFILE_LEVEL_INDICATION, 0x29)?;
        unsafe {
            // SAFETY: transform is a live IMFTransform and out_type a fully built IMFMediaType.
            transform.SetOutputType(0, &out_type, 0)
        }
        .map_err(|e| win_err("IMFTransform::SetOutputType", e))?;

        let in_type = new_media_type()?;
        set_guid(&in_type, &MF_MT_MAJOR_TYPE, &MFMediaType_Audio)?;
        set_guid(&in_type, &MF_MT_SUBTYPE, &MFAudioFormat_PCM)?;
        set_u32(&in_type, &MF_MT_AUDIO_BITS_PER_SAMPLE, 16)?;
        set_u32(&in_type, &MF_MT_AUDIO_SAMPLES_PER_SECOND, sample_rate)?;
        set_u32(&in_type, &MF_MT_AUDIO_NUM_CHANNELS, u32::from(channels))?;
        set_u32(
            &in_type,
            &MF_MT_AUDIO_BLOCK_ALIGNMENT,
            2 * u32::from(channels),
        )?;
        set_u32(
            &in_type,
            &MF_MT_AUDIO_AVG_BYTES_PER_SECOND,
            sample_rate * 2 * u32::from(channels),
        )?;
        unsafe {
            // SAFETY: transform is a live IMFTransform and in_type a fully built IMFMediaType.
            transform.SetInputType(0, &in_type, 0)
        }
        .map_err(|e| win_err("IMFTransform::SetInputType", e))?;

        let asc = read_asc(&transform, sample_rate, channels)?;

        let out_info = unsafe {
            // SAFETY: stream 0 exists on the encoder once both types are set.
            transform.GetOutputStreamInfo(0)
        }
        .map_err(|e| win_err("IMFTransform::GetOutputStreamInfo", e))?;

        unsafe {
            // SAFETY: streaming notifications take no parameter; the encoder is configured.
            transform.ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)
        }
        .map_err(|e| win_err("ProcessMessage(BEGIN_STREAMING)", e))?;
        unsafe {
            // SAFETY: as above.
            transform.ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)
        }
        .map_err(|e| win_err("ProcessMessage(START_OF_STREAM)", e))?;

        Ok(Self {
            transform,
            asc,
            sample_rate,
            channels: usize::from(channels),
            out_size: out_info.cbSize,
            first_pts: None,
            packets_out: 0,
        })
    }

    /// AudioSpecificConfig of the output stream.
    pub fn asc(&self) -> &[u8] {
        &self.asc
    }

    /// Encodes interleaved samples; `pts` is the time of `samples[0]`.
    pub fn push(&mut self, samples: &[f32], pts: i64) -> Result<Vec<AacPacket>, AacError> {
        if samples.is_empty() {
            return Ok(Vec::new());
        }
        if !samples.len().is_multiple_of(self.channels) {
            return Err(AacError::BadInput(format!(
                "{} samples is not a multiple of {} channels",
                samples.len(),
                self.channels
            )));
        }
        if self.first_pts.is_none() {
            self.first_pts = Some(pts);
        }

        let frames = (samples.len() / self.channels) as i64;
        let mut bytes = Vec::with_capacity(samples.len() * 2);
        for &s in samples {
            let v = (s.clamp(-1.0, 1.0) * 32767.0).round() as i16;
            bytes.extend_from_slice(&v.to_le_bytes());
        }
        let sample = make_input_sample(
            &bytes,
            pts,
            frames * HNS_PER_SEC / i64::from(self.sample_rate),
        )?;

        let mut out = Vec::new();
        match unsafe {
            // SAFETY: transform is live and sample holds a valid PCM buffer.
            self.transform.ProcessInput(0, &sample, 0)
        } {
            Ok(()) => {}
            Err(e) if e.code() == MF_E_NOTACCEPTING => {
                self.drain(&mut out)?;
                unsafe {
                    // SAFETY: as above; the encoder accepts input again after draining outputs.
                    self.transform.ProcessInput(0, &sample, 0)
                }
                .map_err(|e| win_err("IMFTransform::ProcessInput", e))?;
            }
            Err(e) => return Err(win_err("IMFTransform::ProcessInput", e)),
        }
        self.drain(&mut out)?;
        Ok(out)
    }

    /// Drains the encoder.
    pub fn flush(&mut self) -> Result<Vec<AacPacket>, AacError> {
        unsafe {
            // SAFETY: COMMAND_DRAIN takes no parameter.
            self.transform.ProcessMessage(MFT_MESSAGE_COMMAND_DRAIN, 0)
        }
        .map_err(|e| win_err("ProcessMessage(COMMAND_DRAIN)", e))?;
        let mut out = Vec::new();
        self.drain(&mut out)?;
        Ok(out)
    }

    /// Pulls every available output frame into `out`, stopping at NEED_MORE_INPUT.
    fn drain(&mut self, out: &mut Vec<AacPacket>) -> Result<(), AacError> {
        loop {
            let sample = empty_output_sample(self.out_size)?;
            let mut buffers = [MFT_OUTPUT_DATA_BUFFER {
                dwStreamID: 0,
                pSample: ManuallyDrop::new(Some(sample)),
                dwStatus: 0,
                pEvents: ManuallyDrop::new(None),
            }];
            let mut status = 0u32;
            let result = unsafe {
                // SAFETY: buffers holds one output buffer with a sample we allocated; status is valid.
                self.transform.ProcessOutput(0, &mut buffers, &mut status)
            };
            // Take ownership of the COM pointers back so they are released on every path.
            let [buffer] = buffers;
            let MFT_OUTPUT_DATA_BUFFER {
                pSample, pEvents, ..
            } = buffer;
            drop(ManuallyDrop::into_inner(pEvents));
            let produced = ManuallyDrop::into_inner(pSample);

            match result {
                Ok(()) => {
                    let sample = produced.ok_or_else(|| AacError::Windows {
                        context: "IMFTransform::ProcessOutput",
                        message: "no output sample returned".to_string(),
                    })?;
                    let data = sample_bytes(&sample)?;
                    let pts = self.packet_pts();
                    self.packets_out += 1;
                    out.push(AacPacket {
                        data,
                        pts,
                        samples: FRAME_SAMPLES,
                    });
                }
                Err(e) if e.code() == MF_E_TRANSFORM_NEED_MORE_INPUT => return Ok(()),
                Err(e) => return Err(win_err("IMFTransform::ProcessOutput", e)),
            }
        }
    }

    /// Presentation time of the next packet: first push time plus k frames.
    fn packet_pts(&self) -> i64 {
        let first = self.first_pts.unwrap_or(0);
        let offset =
            i128::from(self.packets_out) * i128::from(FRAME_SAMPLES) * i128::from(HNS_PER_SEC)
                / i128::from(self.sample_rate);
        first + offset as i64
    }
}

/// Converts a Windows error into an `AacError` tagged with the failing call.
fn win_err(context: &'static str, e: WinError) -> AacError {
    AacError::Windows {
        context,
        message: e.message(),
    }
}

/// Once-per-process COM MTA usage and Media Foundation startup.
fn startup() -> Result<(), AacError> {
    static INIT: OnceLock<Result<(), AacError>> = OnceLock::new();
    INIT.get_or_init(|| unsafe {
        // SAFETY: CoIncrementMTAUsage has no preconditions; the cookie is intentionally leaked
        // so the MTA stays alive for the process lifetime.
        let _ = CoIncrementMTAUsage();
        // SAFETY: MFStartup takes a version constant and flags; it is called once per process.
        MFStartup(MF_VERSION, MFSTARTUP_FULL).map_err(|e| win_err("MFStartup", e))
    })
    .clone()
}

fn new_media_type() -> Result<IMFMediaType, AacError> {
    unsafe {
        // SAFETY: MFCreateMediaType has no preconditions.
        MFCreateMediaType()
    }
    .map_err(|e| win_err("MFCreateMediaType", e))
}

fn set_u32(t: &IMFMediaType, key: &GUID, value: u32) -> Result<(), AacError> {
    unsafe {
        // SAFETY: t is a live media type and key outlives the call.
        t.SetUINT32(key, value)
    }
    .map_err(|e| win_err("IMFAttributes::SetUINT32", e))
}

fn set_guid(t: &IMFMediaType, key: &GUID, value: &GUID) -> Result<(), AacError> {
    unsafe {
        // SAFETY: t is a live media type; key and value outlive the call.
        t.SetGUID(key, value)
    }
    .map_err(|e| win_err("IMFAttributes::SetGUID", e))
}

/// Reads the AudioSpecificConfig from the encoder's output type user-data blob,
/// falling back to a hand-built two-byte ASC when the blob is absent or too short.
fn read_asc(
    transform: &IMFTransform,
    sample_rate: u32,
    channels: u16,
) -> Result<Vec<u8>, AacError> {
    let current = unsafe {
        // SAFETY: stream 0 has an output type set.
        transform.GetOutputCurrentType(0)
    }
    .map_err(|e| win_err("IMFTransform::GetOutputCurrentType", e))?;

    let size = unsafe {
        // SAFETY: the key is a static GUID; a missing blob yields an error, treated as size 0.
        current.GetBlobSize(&MF_MT_USER_DATA)
    }
    .unwrap_or(0) as usize;

    if size > HEAAC_HEADER_LEN {
        let mut blob = vec![0u8; size];
        unsafe {
            // SAFETY: blob is exactly the size reported by GetBlobSize.
            current.GetBlob(&MF_MT_USER_DATA, &mut blob, None)
        }
        .map_err(|e| win_err("IMFAttributes::GetBlob", e))?;
        return Ok(blob[HEAAC_HEADER_LEN..].to_vec());
    }

    let freq_index: u16 = if sample_rate == 48_000 { 3 } else { 4 };
    let asc: u16 = (2 << 11) | (freq_index << 7) | (channels << 3);
    Ok(asc.to_be_bytes().to_vec())
}

/// Builds an input sample holding `data` (PCM bytes) with the given time and duration.
fn make_input_sample(data: &[u8], pts: i64, duration: i64) -> Result<IMFSample, AacError> {
    let len = u32::try_from(data.len())
        .map_err(|_| AacError::BadInput("push chunk too large".to_string()))?;
    unsafe {
        // SAFETY: MF creation calls have no preconditions beyond valid arguments.
        let sample = MFCreateSample().map_err(|e| win_err("MFCreateSample", e))?;
        let buffer = MFCreateMemoryBuffer(len).map_err(|e| win_err("MFCreateMemoryBuffer", e))?;
        let mut ptr: *mut u8 = null_mut();
        // SAFETY: a successful Lock gives a pointer to at least `len` writable bytes until Unlock.
        buffer
            .Lock(&mut ptr, None, None)
            .map_err(|e| win_err("IMFMediaBuffer::Lock", e))?;
        // SAFETY: data holds `len` bytes, ptr is the locked buffer of capacity `len`, and they do not overlap.
        std::ptr::copy_nonoverlapping(data.as_ptr(), ptr, data.len());
        // SAFETY: the buffer was locked above.
        buffer
            .Unlock()
            .map_err(|e| win_err("IMFMediaBuffer::Unlock", e))?;
        buffer
            .SetCurrentLength(len)
            .map_err(|e| win_err("IMFMediaBuffer::SetCurrentLength", e))?;
        sample
            .AddBuffer(&buffer)
            .map_err(|e| win_err("IMFSample::AddBuffer", e))?;
        sample
            .SetSampleTime(pts)
            .map_err(|e| win_err("IMFSample::SetSampleTime", e))?;
        sample
            .SetSampleDuration(duration)
            .map_err(|e| win_err("IMFSample::SetSampleDuration", e))?;
        Ok(sample)
    }
}

/// Allocates an empty output sample with a buffer of `size` bytes for the MFT to fill.
fn empty_output_sample(size: u32) -> Result<IMFSample, AacError> {
    unsafe {
        // SAFETY: MF creation calls have no preconditions beyond valid arguments.
        let sample = MFCreateSample().map_err(|e| win_err("MFCreateSample", e))?;
        let buffer = MFCreateMemoryBuffer(size).map_err(|e| win_err("MFCreateMemoryBuffer", e))?;
        sample
            .AddBuffer(&buffer)
            .map_err(|e| win_err("IMFSample::AddBuffer", e))?;
        Ok(sample)
    }
}

/// Copies the bytes of an output sample into a Vec.
fn sample_bytes(sample: &IMFSample) -> Result<Vec<u8>, AacError> {
    unsafe {
        // SAFETY: the sample is live; ConvertToContiguousBuffer returns a live buffer.
        let buffer = sample
            .ConvertToContiguousBuffer()
            .map_err(|e| win_err("IMFSample::ConvertToContiguousBuffer", e))?;
        let mut ptr: *mut u8 = null_mut();
        let mut len = 0u32;
        // SAFETY: a successful Lock gives `len` readable bytes at ptr until Unlock.
        buffer
            .Lock(&mut ptr, None, Some(&mut len as *mut u32))
            .map_err(|e| win_err("IMFMediaBuffer::Lock", e))?;
        let data = if len == 0 || ptr.is_null() {
            Vec::new()
        } else {
            std::slice::from_raw_parts(ptr, len as usize).to_vec()
        };
        // SAFETY: the buffer was locked above.
        buffer
            .Unlock()
            .map_err(|e| win_err("IMFMediaBuffer::Unlock", e))?;
        Ok(data)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn smoke_encodes_one_second() {
        let mut enc = AacEncoder::new(48_000, 2, 160).expect("open encoder");
        assert_eq!(enc.asc(), &[0x11u8, 0x90][..]);

        let mut samples = Vec::with_capacity(96_000);
        for i in 0..48_000 {
            let v = (2.0 * std::f32::consts::PI * 440.0 * i as f32 / 48_000.0).sin() * 0.5;
            samples.push(v);
            samples.push(v);
        }
        let mut packets = enc.push(&samples, 0).expect("push");
        packets.extend(enc.flush().expect("flush"));

        println!("aacenc smoke: {} packets", packets.len());
        assert!(!packets.is_empty());
        assert!(packets.iter().all(|p| !p.data.is_empty()));
        assert_eq!(enc.asc(), &[0x11u8, 0x90][..]);
    }
}

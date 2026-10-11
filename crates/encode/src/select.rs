//! Encoder selection from a probe report.
//!
//! Auto order per codec: NVENC > AMF > QSV > Software. Only encoders the
//! probe reported as working are candidates. AV1 never selects software.

use crate::probe::{Backend, CodecKind, EncoderInfo, ProbeReport};

/// Auto order, highest priority first.
pub const AUTO_ORDER: [Backend; 4] = [
    Backend::Nvenc,
    Backend::Amf,
    Backend::Qsv,
    Backend::Software,
];

/// Caller's backend preference.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Preference {
    /// Use the first working backend in [`AUTO_ORDER`].
    Auto,
    /// Use this backend if it works, otherwise fall back to Auto.
    Force(Backend),
}

/// Chosen encoder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Selection {
    /// The encoder to use.
    pub encoder: EncoderInfo,
    /// Set when a forced backend was unavailable and Auto picked `encoder`.
    pub fell_back_from: Option<Backend>,
}

/// Picks an encoder for `codec` according to `pref`.
///
/// Returns `None` when no working encoder exists for the codec. With
/// `Force(b)` that is not working, the result is the Auto pick with
/// `fell_back_from = Some(b)`.
pub fn pick(report: &ProbeReport, codec: &CodecKind, pref: &Preference) -> Option<Selection> {
    match *pref {
        Preference::Auto => auto_pick(report, codec).map(|encoder| Selection {
            encoder,
            fell_back_from: None,
        }),
        Preference::Force(backend) => {
            if let Some(encoder) = working(report, backend, codec) {
                return Some(Selection {
                    encoder,
                    fell_back_from: None,
                });
            }
            auto_pick(report, codec).map(|encoder| Selection {
                encoder,
                fell_back_from: Some(backend),
            })
        }
    }
}

fn auto_pick(report: &ProbeReport, codec: &CodecKind) -> Option<EncoderInfo> {
    AUTO_ORDER
        .iter()
        .find_map(|&backend| working(report, backend, codec))
}

/// The working encoder for `(backend, codec)`, if any. Software AV1 is never returned.
fn working(report: &ProbeReport, backend: Backend, codec: &CodecKind) -> Option<EncoderInfo> {
    if backend == Backend::Software && *codec == CodecKind::Av1 {
        return None;
    }
    report
        .encoders
        .iter()
        .find(|e| e.backend == backend && e.codec == *codec && e.works)
        .cloned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(backend: Backend, codec: CodecKind, works: bool) -> EncoderInfo {
        EncoderInfo {
            backend,
            codec,
            encoder_name: format!("{backend:?}-{codec:?}"),
            works,
            error: if works { None } else { Some("nope".into()) },
            probe_ms: 1,
        }
    }

    fn report(encoders: Vec<EncoderInfo>) -> ProbeReport {
        ProbeReport {
            build: "test".into(),
            encoders,
        }
    }

    #[test]
    fn auto_prefers_nvenc_then_amf_qsv_software() {
        let all = report(vec![
            info(Backend::Software, CodecKind::H264, true),
            info(Backend::Qsv, CodecKind::H264, true),
            info(Backend::Amf, CodecKind::H264, true),
            info(Backend::Nvenc, CodecKind::H264, true),
        ]);
        let s = pick(&all, &CodecKind::H264, &Preference::Auto).unwrap();
        assert_eq!(s.encoder.backend, Backend::Nvenc);
        assert_eq!(s.fell_back_from, None);

        let no_nv = report(vec![
            info(Backend::Software, CodecKind::H264, true),
            info(Backend::Qsv, CodecKind::H264, true),
            info(Backend::Amf, CodecKind::H264, true),
        ]);
        assert_eq!(
            pick(&no_nv, &CodecKind::H264, &Preference::Auto)
                .unwrap()
                .encoder
                .backend,
            Backend::Amf
        );

        let sw_only = report(vec![
            info(Backend::Nvenc, CodecKind::H264, false),
            info(Backend::Software, CodecKind::H264, true),
        ]);
        assert_eq!(
            pick(&sw_only, &CodecKind::H264, &Preference::Auto)
                .unwrap()
                .encoder
                .backend,
            Backend::Software
        );
    }

    #[test]
    fn auto_none_when_nothing_works_for_codec() {
        let r = report(vec![info(Backend::Nvenc, CodecKind::H264, false)]);
        assert_eq!(pick(&r, &CodecKind::H264, &Preference::Auto), None);
        assert_eq!(pick(&r, &CodecKind::H265, &Preference::Auto), None);
    }

    #[test]
    fn codec_filter_is_respected() {
        let r = report(vec![
            info(Backend::Nvenc, CodecKind::H264, true),
            info(Backend::Software, CodecKind::H265, true),
        ]);
        let s = pick(&r, &CodecKind::H265, &Preference::Auto).unwrap();
        assert_eq!(s.encoder.backend, Backend::Software);
        assert_eq!(s.encoder.codec, CodecKind::H265);
    }

    #[test]
    fn forced_available_is_used_without_fallback() {
        let r = report(vec![
            info(Backend::Nvenc, CodecKind::H264, true),
            info(Backend::Software, CodecKind::H264, true),
        ]);
        let s = pick(&r, &CodecKind::H264, &Preference::Force(Backend::Software)).unwrap();
        assert_eq!(s.encoder.backend, Backend::Software);
        assert_eq!(s.fell_back_from, None);
    }

    #[test]
    fn forced_unavailable_falls_back_to_auto_and_reports_it() {
        let r = report(vec![
            info(Backend::Nvenc, CodecKind::H264, true),
            info(Backend::Software, CodecKind::H264, true),
        ]);
        let s = pick(&r, &CodecKind::H264, &Preference::Force(Backend::Amf)).unwrap();
        assert_eq!(s.encoder.backend, Backend::Nvenc);
        assert_eq!(s.fell_back_from, Some(Backend::Amf));
    }

    #[test]
    fn forced_unavailable_and_auto_empty_is_none() {
        let r = report(vec![info(Backend::Amf, CodecKind::H264, false)]);
        assert_eq!(
            pick(&r, &CodecKind::H264, &Preference::Force(Backend::Qsv)),
            None
        );
    }

    #[test]
    fn av1_never_selects_software() {
        let r = report(vec![info(Backend::Software, CodecKind::Av1, true)]);
        assert_eq!(pick(&r, &CodecKind::Av1, &Preference::Auto), None);
        assert_eq!(
            pick(&r, &CodecKind::Av1, &Preference::Force(Backend::Software)),
            None
        );
    }

    #[test]
    fn av1_forced_hw_unavailable_falls_back_to_hw_auto() {
        let r = report(vec![
            info(Backend::Qsv, CodecKind::Av1, true),
            info(Backend::Software, CodecKind::Av1, true),
        ]);
        let s = pick(&r, &CodecKind::Av1, &Preference::Force(Backend::Nvenc)).unwrap();
        assert_eq!(s.encoder.backend, Backend::Qsv);
        assert_eq!(s.fell_back_from, Some(Backend::Nvenc));
    }

    #[test]
    fn av1_forced_software_with_no_hw_returns_none_and_no_selection() {
        let r = report(vec![info(Backend::Nvenc, CodecKind::Av1, false)]);
        assert_eq!(
            pick(&r, &CodecKind::Av1, &Preference::Force(Backend::Software)),
            None
        );
    }
}

//! Integration tests for clipcheck-encode: encoder selection and probe report types.

use clipcheck_encode::probe::{Backend, CodecKind, EncoderInfo, ProbeReport};
use clipcheck_encode::select::{pick, Preference};

fn info(backend: Backend, codec: CodecKind, works: bool) -> EncoderInfo {
    EncoderInfo {
        backend,
        codec,
        encoder_name: format!("{backend:?}-{codec:?}"),
        works,
        error: if works { None } else { Some("failed".into()) },
        probe_ms: 1,
    }
}

fn report(encoders: Vec<EncoderInfo>) -> ProbeReport {
    ProbeReport {
        build: "test build".into(),
        encoders,
    }
}

// 9. Selection tests with hand-built reports.
#[test]
fn auto_order_h264_across_backends() {
    use Backend::*;
    use CodecKind::H264;

    let all = report(vec![
        info(Nvenc, H264, true),
        info(Amf, H264, true),
        info(Qsv, H264, true),
        info(Software, H264, true),
    ]);
    let s = pick(&all, &H264, &Preference::Auto).unwrap();
    assert_eq!(s.encoder.backend, Nvenc);
    assert_eq!(s.fell_back_from, None);

    let no_nvenc = report(vec![
        info(Nvenc, H264, false),
        info(Amf, H264, true),
        info(Qsv, H264, true),
        info(Software, H264, true),
    ]);
    assert_eq!(
        pick(&no_nvenc, &H264, &Preference::Auto)
            .unwrap()
            .encoder
            .backend,
        Amf
    );

    let no_amf = report(vec![
        info(Nvenc, H264, false),
        info(Amf, H264, false),
        info(Qsv, H264, true),
        info(Software, H264, true),
    ]);
    assert_eq!(
        pick(&no_amf, &H264, &Preference::Auto)
            .unwrap()
            .encoder
            .backend,
        Qsv
    );

    let sw_only = report(vec![
        info(Nvenc, H264, false),
        info(Amf, H264, false),
        info(Qsv, H264, false),
        info(Software, H264, true),
    ]);
    assert_eq!(
        pick(&sw_only, &H264, &Preference::Auto)
            .unwrap()
            .encoder
            .backend,
        Software
    );
}

#[test]
fn auto_order_hevc() {
    use Backend::*;
    use CodecKind::H265;

    let r = report(vec![info(Nvenc, H265, true), info(Software, H265, true)]);
    assert_eq!(
        pick(&r, &H265, &Preference::Auto).unwrap().encoder.backend,
        Nvenc
    );

    let r = report(vec![info(Nvenc, H265, false), info(Software, H265, true)]);
    let s = pick(&r, &H265, &Preference::Auto).unwrap();
    assert_eq!(s.encoder.backend, Software);
    assert_eq!(s.encoder.codec, H265);
}

#[test]
fn force_available_has_no_fallback() {
    use Backend::*;
    use CodecKind::H264;

    let r = report(vec![info(Nvenc, H264, true), info(Qsv, H264, true)]);
    let s = pick(&r, &H264, &Preference::Force(Qsv)).unwrap();
    assert_eq!(s.encoder.backend, Qsv);
    assert_eq!(s.fell_back_from, None);
}

#[test]
fn force_unavailable_falls_back_to_auto() {
    use Backend::*;
    use CodecKind::H264;

    let r = report(vec![info(Nvenc, H264, true), info(Qsv, H264, false)]);
    let s = pick(&r, &H264, &Preference::Force(Qsv)).unwrap();
    assert_eq!(s.encoder.backend, Nvenc);
    assert_eq!(s.fell_back_from, Some(Qsv));
}

#[test]
fn force_software_av1_is_none() {
    use Backend::*;
    use CodecKind::Av1;

    let r = report(vec![info(Software, Av1, true)]);
    assert_eq!(pick(&r, &Av1, &Preference::Force(Software)), None);
    assert_eq!(pick(&r, &Av1, &Preference::Auto), None);
}

#[test]
fn av1_auto_picks_amf_when_only_amf_works() {
    use Backend::*;
    use CodecKind::Av1;

    let r = report(vec![
        info(Nvenc, Av1, false),
        info(Amf, Av1, true),
        info(Software, Av1, true),
    ]);
    let s = pick(&r, &Av1, &Preference::Auto).unwrap();
    assert_eq!(s.encoder.backend, Amf);
    assert_eq!(s.encoder.codec, Av1);
}

#[test]
fn av1_auto_none_when_nothing_works() {
    use Backend::*;
    use CodecKind::Av1;

    let r = report(vec![
        info(Nvenc, Av1, false),
        info(Amf, Av1, false),
        info(Qsv, Av1, false),
    ]);
    assert_eq!(pick(&r, &Av1, &Preference::Auto), None);
}

#[test]
fn auto_skips_failed_nvenc_h264() {
    use Backend::*;
    use CodecKind::H264;

    let r = report(vec![info(Nvenc, H264, false), info(Software, H264, true)]);
    assert!(!r.encoders[0].works);
    assert!(r.encoders[0].error.is_some());
    let s = pick(&r, &H264, &Preference::Auto).unwrap();
    assert_eq!(s.encoder.backend, Software);
    assert_eq!(s.fell_back_from, None);
}

// 10. Serde: lowercase names and round-trip of a hand-built report.
#[test]
fn serde_lowercase_names_and_round_trip() {
    assert_eq!(serde_json::to_string(&Backend::Nvenc).unwrap(), "\"nvenc\"");
    assert_eq!(
        serde_json::to_string(&Backend::Software).unwrap(),
        "\"software\""
    );
    assert_eq!(serde_json::to_string(&CodecKind::H265).unwrap(), "\"h265\"");
    assert_eq!(serde_json::to_string(&CodecKind::Av1).unwrap(), "\"av1\"");

    let original = report(vec![
        EncoderInfo {
            backend: Backend::Nvenc,
            codec: CodecKind::H264,
            encoder_name: "h264_nvenc".into(),
            works: true,
            error: None,
            probe_ms: 812,
        },
        EncoderInfo {
            backend: Backend::Amf,
            codec: CodecKind::Av1,
            encoder_name: "av1_amf".into(),
            works: false,
            error: Some("Driver does not support".into()),
            probe_ms: 0,
        },
    ]);
    let text = serde_json::to_string(&original).unwrap();
    let back: ProbeReport = serde_json::from_str(&text).unwrap();
    assert_eq!(original, back);
}

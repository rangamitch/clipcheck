use clipcheck_core::media::{
    ms_to_100ns, secs_to_100ns, ticks_to_ms, ticks_to_secs_f64, Codec, EncodedPacket, StreamId,
    VideoParams, TICKS_PER_MS, TICKS_PER_SEC,
};

fn sample_params() -> VideoParams {
    VideoParams {
        codec: Codec::Hevc,
        width: 1920,
        height: 1080,
        fps_num: 60,
        fps_den: 1,
        extradata: vec![0x01, 0x02, 0x03, 0xFF],
    }
}

fn sample_packet() -> EncodedPacket {
    EncodedPacket {
        stream: StreamId::Video,
        pts_100ns: 1_000_000,
        dts_100ns: 900_000,
        duration_100ns: 166_666,
        keyframe: true,
        generation: 3,
        data: vec![0, 0, 0, 1, 0x65, 0xAA, 0xBB],
    }
}

#[test]
fn constants() {
    assert_eq!(TICKS_PER_MS, 10_000);
    assert_eq!(TICKS_PER_SEC, 10_000_000);
}

#[test]
fn ms_to_100ns_conversions() {
    assert_eq!(ms_to_100ns(1), 10_000);
    assert_eq!(ms_to_100ns(0), 0);
    assert_eq!(ms_to_100ns(-5), -50_000);
    assert_eq!(ms_to_100ns(i64::MAX), i64::MAX);
    assert_eq!(ms_to_100ns(i64::MIN), i64::MIN);
}

#[test]
fn secs_to_100ns_conversions() {
    assert_eq!(secs_to_100ns(1.0), 10_000_000);
    assert_eq!(secs_to_100ns(0.5), 5_000_000);
    assert_eq!(secs_to_100ns(0.0), 0);
    assert_eq!(secs_to_100ns(-1.0), -10_000_000);
    let tiny = secs_to_100ns(0.00000005);
    assert!(tiny == 0 || tiny == 1, "got {tiny}");
}

#[test]
fn ticks_to_ms_truncates_toward_zero() {
    assert_eq!(ticks_to_ms(25_000), 2);
    assert_eq!(ticks_to_ms(9_999), 0);
    assert_eq!(ticks_to_ms(10_000), 1);
    assert_eq!(ticks_to_ms(-25_000), -2);
    assert_eq!(ticks_to_ms(-9_999), 0);
    assert_eq!(ticks_to_ms(0), 0);
}

#[test]
fn ticks_to_secs_f64_conversions() {
    assert_eq!(ticks_to_secs_f64(10_000_000), 1.0);
    assert_eq!(ticks_to_secs_f64(0), 0.0);
    assert_eq!(ticks_to_secs_f64(-5_000_000), -0.5);
    let rt = ticks_to_secs_f64(secs_to_100ns(2.5));
    assert!((rt - 2.5).abs() < 1e-9, "got {rt}");
}

#[test]
fn stream_id_equality() {
    assert_ne!(StreamId::Audio(1), StreamId::Audio(2));
    assert_ne!(StreamId::Video, StreamId::Audio(0));
    assert_eq!(StreamId::Audio(1), StreamId::Audio(1));
}

#[test]
fn codec_serde_round_trip() {
    for codec in [Codec::H264, Codec::Hevc, Codec::Av1] {
        let json = serde_json::to_string(&codec).unwrap();
        let back: Codec = serde_json::from_str(&json).unwrap();
        assert_eq!(back, codec);
    }
}

#[test]
fn codec_serde_uses_snake_case_strings() {
    assert_eq!(serde_json::to_string(&Codec::H264).unwrap(), "\"h264\"");
    assert_eq!(serde_json::to_string(&Codec::Hevc).unwrap(), "\"hevc\"");
    assert_eq!(serde_json::to_string(&Codec::Av1).unwrap(), "\"av1\"");
    assert!(serde_json::from_str::<Codec>("\"h265\"").is_err());
    assert!(serde_json::from_str::<Codec>("\"H265\"").is_err());
}

#[test]
fn stream_id_serde_round_trip() {
    for id in [
        StreamId::Video,
        StreamId::Audio(0),
        StreamId::Audio(1),
        StreamId::Audio(255),
    ] {
        let json = serde_json::to_string(&id).unwrap();
        let back: StreamId = serde_json::from_str(&json).unwrap();
        assert_eq!(back, id);
    }
}

#[test]
fn video_params_clone_eq_and_serde() {
    let p = sample_params();
    let cloned = p.clone();
    assert_eq!(p, cloned);

    let mut other = sample_params();
    other.width = 1280;
    assert_ne!(p, other);

    let json = serde_json::to_string(&p).unwrap();
    let back: VideoParams = serde_json::from_str(&json).unwrap();
    assert_eq!(back, p);
    assert_eq!(back.extradata, vec![0x01, 0x02, 0x03, 0xFF]);
}

#[test]
fn encoded_packet_clone_eq_and_serde() {
    let pkt = sample_packet();
    let cloned = pkt.clone();
    assert_eq!(pkt, cloned);

    let mut other = sample_packet();
    other.data.push(0x00);
    assert_ne!(pkt, other);

    let json = serde_json::to_string(&pkt).unwrap();
    let back: EncodedPacket = serde_json::from_str(&json).unwrap();
    assert_eq!(back, pkt);
    assert_eq!(back.data, vec![0, 0, 0, 1, 0x65, 0xAA, 0xBB]);
}

#[test]
fn encoded_packet_empty_data_round_trips() {
    let mut pkt = sample_packet();
    pkt.stream = StreamId::Audio(2);
    pkt.keyframe = false;
    pkt.data = Vec::new();
    let json = serde_json::to_string(&pkt).unwrap();
    let back: EncodedPacket = serde_json::from_str(&json).unwrap();
    assert_eq!(back, pkt);
    assert!(back.data.is_empty());
}

#[test]
fn video_params_empty_extradata_round_trips() {
    let mut p = sample_params();
    p.codec = Codec::Av1;
    p.extradata = Vec::new();
    let json = serde_json::to_string(&p).unwrap();
    let back: VideoParams = serde_json::from_str(&json).unwrap();
    assert_eq!(back, p);
}

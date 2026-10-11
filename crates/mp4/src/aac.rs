//! AAC AudioSpecificConfig (worker M1).

/// MPEG-4 sampling frequency table, indexed by `samplingFrequencyIndex`.
const SAMPLE_RATES: [u32; 13] = [
    96000, 88200, 64000, 48000, 44100, 32000, 24000, 22050, 16000, 12000, 11025, 8000, 7350,
];

/// AudioSpecificConfig for AAC-LC. None for an unsupported sample rate or zero channels.
///
/// Two bytes: 5 bits audioObjectType (2), 4 bits samplingFrequencyIndex, 4 bits channelConfiguration,
/// 3 zero bits. Channel configurations 1..=7 are accepted.
pub fn aac_lc_asc(sample_rate: u32, channels: u8) -> Option<Vec<u8>> {
    if !(1..=7).contains(&channels) {
        return None;
    }
    let index = SAMPLE_RATES.iter().position(|&rate| rate == sample_rate)? as u16;
    let bits: u16 = (2 << 11) | (index << 7) | (u16::from(channels) << 3);
    Some(bits.to_be_bytes().to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_known_configs() {
        assert_eq!(aac_lc_asc(48000, 2), Some(vec![0x11, 0x90]));
        assert_eq!(aac_lc_asc(44100, 2), Some(vec![0x12, 0x10]));
        assert_eq!(aac_lc_asc(44100, 0), None);
        assert_eq!(aac_lc_asc(44101, 2), None);
    }
}

/// Encode pipeline PCM as a 16-bit mono WAV.
pub fn encode_wav_to_vec(pcm_data: &[f32], sample_rate: u32, channels: u16) -> Vec<u8> {
    let data_size = pcm_data.len() * 2;
    let mut wav = Vec::with_capacity(44 + data_size);

    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(36 + data_size as u32).to_le_bytes());
    wav.extend_from_slice(b"WAVE");

    wav.extend_from_slice(b"fmt ");
    wav.extend_from_slice(&16u32.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes());
    wav.extend_from_slice(&channels.to_le_bytes());
    wav.extend_from_slice(&sample_rate.to_le_bytes());
    wav.extend_from_slice(&(sample_rate * channels as u32 * 2).to_le_bytes());
    wav.extend_from_slice(&(channels * 2).to_le_bytes());
    wav.extend_from_slice(&16u16.to_le_bytes());

    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&(data_size as u32).to_le_bytes());
    for &sample in pcm_data {
        let clamped = sample.clamp(-1.0, 1.0);
        let int_sample = (clamped * 32767.0) as i16;
        wav.extend_from_slice(&int_sample.to_le_bytes());
    }

    wav
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn produces_valid_header() {
        let pcm = vec![0.0f32, 0.5, -0.5, 1.0, -1.0];
        let wav = encode_wav_to_vec(&pcm, 16000, 1);
        assert_eq!(&wav[0..4], b"RIFF");
        assert_eq!(&wav[8..12], b"WAVE");
        let data_size = u32::from_le_bytes(wav[40..44].try_into().unwrap());
        assert_eq!(data_size as usize, pcm.len() * 2);
        let sr = u32::from_le_bytes(wav[24..28].try_into().unwrap());
        assert_eq!(sr, 16000);
        let ch = u16::from_le_bytes([wav[22], wav[23]]);
        assert_eq!(ch, 1);
    }
}

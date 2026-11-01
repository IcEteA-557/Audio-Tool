use audiolink_protocol::{AudioPacket, CHANNELS, FRAMES_PER_PACKET, SAMPLE_RATE};

use crate::resample::RateConverter;

/// 把采集到的交错 f32 切成 5ms 的 16 bit 立体声包。
pub struct Packetizer {
    pending: Vec<f32>,
    input_rate: u32,
    channels: usize,
    converter: Option<RateConverter>,
    convert_pending: Vec<Vec<f32>>,
    seq: u32,
    volume: f32,
}

impl Packetizer {
    pub fn new() -> Self {
        Self {
            pending: Vec::new(),
            input_rate: SAMPLE_RATE,
            channels: CHANNELS as usize,
            converter: None,
            convert_pending: vec![Vec::new(), Vec::new()],
            seq: 0,
            volume: 1.0,
        }
    }

    pub fn set_volume(&mut self, volume: f32) {
        self.volume = volume.clamp(0.0, 1.0);
    }

    pub fn push_interleaved(
        &mut self,
        samples: &[f32],
        channels: u16,
        sample_rate: u32,
    ) -> Result<Vec<AudioPacket>, String> {
        if channels == 0 || sample_rate == 0 || samples.is_empty() {
            return Ok(Vec::new());
        }
        self.ensure_format(sample_rate, channels as usize)?;
        let stereo = to_stereo(samples, channels as usize);
        let frames = if sample_rate == SAMPLE_RATE {
            stereo
        } else {
            self.convert(&stereo)?
        };
        self.pending.extend(frames);
        Ok(self.take_packets())
    }

    fn ensure_format(&mut self, sample_rate: u32, channels: usize) -> Result<(), String> {
        if self.input_rate == sample_rate && self.channels == channels && (sample_rate == SAMPLE_RATE || self.converter.is_some())
        {
            return Ok(());
        }
        self.input_rate = sample_rate;
        self.channels = channels;
        self.pending.clear();
        self.convert_pending = vec![Vec::new(), Vec::new()];
        self.converter = if sample_rate == SAMPLE_RATE {
            None
        } else {
            Some(RateConverter::new(sample_rate, SAMPLE_RATE, 2, 240)?)
        };
        Ok(())
    }

    fn convert(&mut self, stereo: &[f32]) -> Result<Vec<f32>, String> {
        let converter = self.converter.as_mut().expect("converter");
        for (index, sample) in stereo.iter().enumerate() {
            self.convert_pending[index % 2].push(*sample);
        }
        let mut out = Vec::new();
        while self.convert_pending[0].len() >= converter.input_frames_next()
            && self.convert_pending[1].len() >= converter.input_frames_next()
        {
            let need = converter.input_frames_next();
            let input = vec![
                self.convert_pending[0].drain(..need).collect(),
                self.convert_pending[1].drain(..need).collect(),
            ];
            let produced = converter.process(&input)?;
            for frame in 0..produced[0].len() {
                out.push(produced[0][frame]);
                out.push(produced[1][frame]);
            }
        }
        Ok(out)
    }

    fn take_packets(&mut self) -> Vec<AudioPacket> {
        let packet_samples = FRAMES_PER_PACKET as usize * 2;
        let mut packets = Vec::new();
        while self.pending.len() >= packet_samples {
            let chunk: Vec<f32> = self.pending.drain(..packet_samples).collect();
            let pcm = chunk
                .into_iter()
                .map(|sample| float_to_i16(sample * self.volume))
                .collect();
            let seq = self.seq;
            self.seq = self.seq.wrapping_add(1);
            packets.push(AudioPacket {
                seq,
                timestamp_ns: 0,
                sample_rate: SAMPLE_RATE,
                channels: CHANNELS,
                pcm,
            });
        }
        packets
    }
}

impl Default for Packetizer {
    fn default() -> Self {
        Self::new()
    }
}

fn to_stereo(samples: &[f32], channels: usize) -> Vec<f32> {
    match channels {
        2 => samples.to_vec(),
        1 => samples.iter().flat_map(|sample| [*sample, *sample]).collect(),
        _ => samples
            .chunks(channels)
            .flat_map(|frame| [frame.first().copied().unwrap_or(0.0), frame.get(1).copied().unwrap_or(0.0)])
            .collect(),
    }
}

pub fn float_to_i16(sample: f32) -> i16 {
    (sample.clamp(-1.0, 1.0) * 32767.0) as i16
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_48k_audio_into_five_millisecond_packets() {
        let mut packetizer = Packetizer::new();
        let samples = vec![0.25; 2400 * 2];
        let packets = packetizer
            .push_interleaved(&samples, 2, SAMPLE_RATE)
            .unwrap();
        assert_eq!(packets.len(), 10);
        assert_eq!(packets[0].seq, 0);
        assert_eq!(packets[9].seq, 9);
        assert_eq!(packets[0].pcm.len(), 240 * 2);
        assert!(packets[0].pcm.iter().all(|sample| *sample > 0));
    }

    #[test]
    fn volume_zero_silences_packets() {
        let mut packetizer = Packetizer::new();
        packetizer.set_volume(0.0);
        let packets = packetizer
            .push_interleaved(&[0.8; 240 * 2], 2, SAMPLE_RATE)
            .unwrap();
        assert!(packets[0].pcm.iter().all(|sample| *sample == 0));
    }
}

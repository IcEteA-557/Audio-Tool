use std::collections::BTreeMap;

use audiolink_protocol::{AudioPacket, CHANNELS, SAMPLE_RATE};

const MAX_REORDER: u32 = 3;

#[derive(Debug, Clone)]
struct StoredPacket {
    pcm: Vec<i16>,
    timestamp_ns: u64,
    channels: usize,
}

/// 按序号播放的抖动缓冲。缺包时用上一帧做衰减填补，不回头等待。
pub struct JitterBuffer {
    sample_rate: u32,
    channels: usize,
    target_frames: usize,
    packet_frames: usize,
    reorder: BTreeMap<u32, StoredPacket>,
    pcm: Vec<i16>,
    stamps: Vec<(usize, u64)>,
    next_seq: Option<u32>,
    started: bool,
    last_frame: Vec<i16>,
    frames_buffered: usize,
    read_frame: usize,
    pub received_packets: u64,
    pub lost_packets: u64,
    pub underruns: u64,
    jitter_ns: f64,
    last_transit: Option<i64>,
}

impl JitterBuffer {
    pub fn new(target_ms: u16) -> Self {
        let target_frames = frames_for_ms(target_ms, SAMPLE_RATE).max(1);
        Self {
            sample_rate: SAMPLE_RATE,
            channels: CHANNELS as usize,
            target_frames,
            packet_frames: 240,
            reorder: BTreeMap::new(),
            pcm: Vec::new(),
            stamps: Vec::new(),
            next_seq: None,
            started: false,
            last_frame: vec![0; CHANNELS as usize],
            frames_buffered: 0,
            read_frame: 0,
            received_packets: 0,
            lost_packets: 0,
            underruns: 0,
            jitter_ns: 0.0,
            last_transit: None,
        }
    }

    pub fn set_target_ms(&mut self, target_ms: u16) {
        self.target_frames = frames_for_ms(target_ms, self.sample_rate).max(1);
    }

    pub fn target_frames(&self) -> usize {
        self.target_frames
    }

    pub fn buffered_frames(&self) -> usize {
        self.frames_buffered
    }

    pub fn buffered_ms(&self) -> f32 {
        self.frames_buffered as f32 * 1000.0 / self.sample_rate as f32
    }

    pub fn loss_pct(&self) -> f32 {
        let total = self.received_packets + self.lost_packets;
        if total == 0 {
            0.0
        } else {
            self.lost_packets as f32 * 100.0 / total as f32
        }
    }

    pub fn jitter_ms(&self) -> f32 {
        (self.jitter_ns / 1_000_000.0) as f32
    }

    pub fn push(&mut self, packet: AudioPacket, arrival_ns: u64) {
        if packet.channels as usize != self.channels || packet.sample_rate != self.sample_rate {
            return;
        }
        let frames = packet.frame_count();
        if frames == 0 {
            return;
        }
        self.observe_jitter(packet.timestamp_ns, arrival_ns);
        self.packet_frames = frames;
        if let Some(next) = self.next_seq {
            if seq_before(packet.seq, next) {
                return;
            }
        }
        self.reorder.insert(
            packet.seq,
            StoredPacket {
                pcm: packet.pcm,
                timestamp_ns: packet.timestamp_ns,
                channels: self.channels,
            },
        );
        self.drain();
    }

    /// 取出 `frames` 个交错立体声样本，并返回这批样本第一帧的采集时间戳。
    pub fn pop_interleaved(&mut self, frames: usize) -> (Vec<i16>, Option<u64>) {
        if frames == 0 {
            return (Vec::new(), None);
        }
        self.drain();
        if !self.started {
            if self.frames_buffered < self.target_frames {
                return (vec![0; frames * self.channels], None);
            }
            self.started = true;
        }
        while self.frames_buffered < frames {
            if !self.conceal_hole() {
                self.pad_underrun(frames - self.frames_buffered);
                break;
            }
        }
        let stamp = self.stamp_at_read();
        let samples = frames * self.channels;
        let mut out = Vec::with_capacity(samples);
        let available = self.frames_buffered.min(frames) * self.channels;
        out.extend(self.pcm.drain(..available.min(self.pcm.len())));
        self.frames_buffered -= available / self.channels;
        self.read_frame += available / self.channels;
        self.compact_stamps();
        if out.len() < samples {
            out.resize(samples, 0);
        }
        if let Some(frame) = out.chunks_exact(self.channels).last() {
            self.last_frame = frame.to_vec();
        }
        (out, stamp)
    }

    fn observe_jitter(&mut self, capture_ns: u64, arrival_ns: u64) {
        let transit = arrival_ns as i64 - capture_ns as i64;
        if let Some(previous) = self.last_transit {
            let delta = (transit - previous).abs() as f64;
            self.jitter_ns += (delta - self.jitter_ns) / 16.0;
        }
        self.last_transit = Some(transit);
    }

    fn drain(&mut self) {
        if self.next_seq.is_none() {
            self.next_seq = self.reorder.keys().next().copied();
        }
        let Some(mut next) = self.next_seq else {
            return;
        };
        loop {
            if let Some(packet) = self.reorder.remove(&next) {
                self.commit(packet);
                next = next.wrapping_add(1);
                self.next_seq = Some(next);
                continue;
            }
            let Some(later) = self.reorder.keys().next().copied() else {
                break;
            };
            let gap = later.wrapping_sub(next);
            if gap == 0 || gap <= MAX_REORDER {
                break;
            }
            self.conceal_packet();
            self.lost_packets += 1;
            next = next.wrapping_add(1);
            self.next_seq = Some(next);
        }
    }

    fn conceal_hole(&mut self) -> bool {
        let Some(next) = self.next_seq else {
            return false;
        };
        let Some(later) = self.reorder.keys().next().copied() else {
            return false;
        };
        if !seq_before(next, later) {
            return false;
        }
        self.conceal_packet();
        self.lost_packets += 1;
        self.next_seq = Some(next.wrapping_add(1));
        self.drain();
        true
    }

    fn commit(&mut self, packet: StoredPacket) {
        let frames = packet.pcm.len() / packet.channels;
        self.stamps
            .push((self.read_frame + self.frames_buffered, packet.timestamp_ns));
        self.pcm.extend(packet.pcm);
        self.frames_buffered += frames;
        self.received_packets += 1;
        if let Some(frame) = self
            .pcm
            .chunks_exact(self.channels)
            .nth(self.frames_buffered.saturating_sub(1))
        {
            self.last_frame = frame.to_vec();
        }
    }

    fn conceal_packet(&mut self) {
        self.decay_last_frame();
        let mut pcm = Vec::with_capacity(self.packet_frames * self.channels);
        for _ in 0..self.packet_frames {
            pcm.extend_from_slice(&self.last_frame);
        }
        let stamp = self
            .stamps
            .last()
            .map(|(_, ts)| *ts)
            .unwrap_or(0);
        self.stamps
            .push((self.read_frame + self.frames_buffered, stamp));
        self.pcm.extend(pcm);
        self.frames_buffered += self.packet_frames;
    }

    fn pad_underrun(&mut self, frames: usize) {
        if frames == 0 {
            return;
        }
        self.underruns += 1;
        self.decay_last_frame();
        for _ in 0..frames {
            self.pcm.extend_from_slice(&self.last_frame);
        }
        self.frames_buffered += frames;
    }

    fn decay_last_frame(&mut self) {
        if self.last_frame.len() != self.channels {
            self.last_frame = vec![0; self.channels];
        }
        for sample in &mut self.last_frame {
            *sample = (*sample as i32 * 7 / 10) as i16;
        }
    }

    fn stamp_at_read(&self) -> Option<u64> {
        self.stamps
            .iter()
            .rev()
            .find(|(frame, _)| *frame <= self.read_frame)
            .map(|(frame, ts)| {
                let delta = self.read_frame - frame;
                ts.saturating_add(delta as u64 * 1_000_000_000 / self.sample_rate as u64)
            })
    }

    fn compact_stamps(&mut self) {
        if self.stamps.len() < 2 {
            return;
        }
        let mut keep_from = 0;
        for (index, (frame, _)) in self.stamps.iter().enumerate() {
            if *frame <= self.read_frame {
                keep_from = index;
            } else {
                break;
            }
        }
        if keep_from > 0 {
            self.stamps.drain(..keep_from);
        }
    }
}

fn frames_for_ms(ms: u16, sample_rate: u32) -> usize {
    (u32::from(ms) * sample_rate / 1000) as usize
}

fn seq_before(a: u32, b: u32) -> bool {
    b.wrapping_sub(a) < 0x8000_0000 && a != b
}

#[cfg(test)]
mod tests {
    use super::*;

    fn packet(seq: u32, frames: &[i16]) -> AudioPacket {
        let mut pcm = Vec::new();
        for frame in frames {
            pcm.push(*frame);
            pcm.push(*frame);
        }
        AudioPacket {
            seq,
            timestamp_ns: u64::from(seq) * 1_000_000,
            sample_rate: SAMPLE_RATE,
            channels: 2,
            pcm,
        }
    }

    #[test]
    fn reorder_plays_in_sequence() {
        let mut jitter = JitterBuffer::new(1);
        jitter.push(packet(0, &[1, 2]), 10);
        jitter.push(packet(2, &[5, 6]), 12);
        jitter.push(packet(1, &[3, 4]), 11);
        jitter.started = true;
        let (pcm, _) = jitter.pop_interleaved(6);
        let left: Vec<i16> = pcm.iter().step_by(2).copied().collect();
        assert_eq!(left, vec![1, 2, 3, 4, 5, 6]);
        assert_eq!(jitter.lost_packets, 0);
    }

    #[test]
    fn missing_packet_is_concealed_and_counted() {
        let mut jitter = JitterBuffer::new(1);
        jitter.push(packet(0, &[10, 10]), 10);
        jitter.push(packet(2, &[40, 40]), 30);
        jitter.started = true;
        let _ = jitter.pop_interleaved(2);
        let (pcm, _) = jitter.pop_interleaved(2);
        assert_eq!(jitter.lost_packets, 1);
        assert!(pcm.iter().all(|sample| sample.abs() < 10));
        let (pcm, _) = jitter.pop_interleaved(2);
        let left: Vec<i16> = pcm.iter().step_by(2).copied().collect();
        assert_eq!(left, vec![40, 40]);
    }

    #[test]
    fn late_duplicate_is_ignored() {
        let mut jitter = JitterBuffer::new(1);
        jitter.push(packet(0, &[1, 1]), 10);
        jitter.started = true;
        let _ = jitter.pop_interleaved(2);
        jitter.push(packet(0, &[9, 9]), 20);
        assert_eq!(jitter.received_packets, 1);
    }
}

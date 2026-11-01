use std::collections::VecDeque;

use audiolink_protocol::AudioPacket;

use crate::clock::{mono_ns, ntp_offset_smoothed};
use crate::jitter::JitterBuffer;
use crate::latency::AutoLatency;
use crate::resample::{speed_ratio, DriftResampler};

#[derive(Debug, Clone)]
pub struct PlaybackStats {
    pub latency_ms: f32,
    pub jitter_ms: f32,
    pub buffer_ms: f32,
    pub loss_pct: f32,
    pub underruns: u64,
    pub target_ms: u16,
    pub auto_note: String,
    pub rtt_ms: f32,
}

/// 接收端播放：抖动缓冲、rubato 变速，以及按缓冲欠载自动放宽延迟。
pub struct PlaybackEngine {
    jitter: JitterBuffer,
    resampler: DriftResampler,
    pending: VecDeque<f32>,
    device_latency_ms: f32,
    /// 采集端时钟 − 本机时钟。本机试听时为 0。
    clock_offset_ns: i64,
    have_offset: bool,
    latency_ms: f32,
    rtt_ms: f32,
    auto: AutoLatency,
    underruns_seen: u64,
    pending_auto: Option<u16>,
}

impl PlaybackEngine {
    pub fn new(target_ms: u16, device_latency_ms: f32) -> Result<Self, String> {
        Ok(Self {
            jitter: JitterBuffer::new(target_ms),
            resampler: DriftResampler::new(2, 240)?,
            pending: VecDeque::new(),
            device_latency_ms,
            clock_offset_ns: 0,
            have_offset: false,
            latency_ms: 0.0,
            rtt_ms: 0.0,
            auto: AutoLatency::new(target_ms),
            underruns_seen: 0,
            pending_auto: None,
        })
    }

    pub fn push_packet(&mut self, packet: AudioPacket, arrival_ns: u64) {
        self.jitter.push(packet, arrival_ns);
    }

    pub fn set_user_target(&mut self, target_ms: u16) {
        self.auto.set_user_target(target_ms);
        self.jitter.set_target_ms(target_ms);
        self.pending_auto = None;
    }

    /// 收到对端 Pong。`sample_offset` 是对端时钟 − 本机时钟。
    pub fn observe_clock(&mut self, sample_offset_ns: i64, rtt_ns: u64) {
        self.clock_offset_ns = ntp_offset_smoothed(
            self.have_offset.then_some(self.clock_offset_ns),
            sample_offset_ns,
        );
        self.have_offset = true;
        self.rtt_ms = rtt_ns as f32 / 1_000_000.0;
    }

    pub fn fill(&mut self, output: &mut [f32]) -> Result<(), String> {
        while self.pending.len() < output.len() {
            let ratio = speed_ratio(
                self.jitter.buffered_frames(),
                self.jitter.target_frames(),
                48_000,
            );
            let need = self.resampler.input_frames_next().max(1);
            let (pcm, stamp) = self.jitter.pop_interleaved(need);
            if let Some(capture_ns) = stamp {
                self.note_latency(capture_ns);
            }
            let mut left = Vec::with_capacity(need);
            let mut right = Vec::with_capacity(need);
            for frame in pcm.chunks_exact(2) {
                left.push(frame[0] as f32 / 32768.0);
                right.push(frame[1] as f32 / 32768.0);
            }
            let produced = self.resampler.process(&[left, right], ratio)?;
            for index in 0..produced[0].len() {
                self.pending.push_back(produced[0][index]);
                self.pending.push_back(produced[1].get(index).copied().unwrap_or(0.0));
            }
        }
        for sample in output.iter_mut() {
            *sample = self.pending.pop_front().unwrap_or(0.0);
        }
        self.observe_underruns();
        Ok(())
    }

    pub fn take_auto_target(&mut self) -> Option<u16> {
        self.pending_auto.take()
    }

    pub fn stats(&self) -> PlaybackStats {
        PlaybackStats {
            latency_ms: self.latency_ms,
            jitter_ms: self.jitter.jitter_ms(),
            buffer_ms: self.jitter.buffered_ms(),
            loss_pct: self.jitter.loss_pct(),
            underruns: self.jitter.underruns,
            target_ms: self.auto.target_ms,
            auto_note: self.auto.note.clone(),
            rtt_ms: self.rtt_ms,
        }
    }

    fn note_latency(&mut self, capture_ns: u64) {
        let now = mono_ns() as i64;
        let capture_local = capture_ns as i64 - self.clock_offset_ns;
        let latency = (now - capture_local) as f32 / 1_000_000.0 + self.device_latency_ms;
        if latency.is_finite() && (0.0..2_000.0).contains(&latency) {
            self.latency_ms = if self.latency_ms == 0.0 {
                latency
            } else {
                self.latency_ms * 0.85 + latency * 0.15
            };
        }
    }

    fn observe_underruns(&mut self) {
        let underruns = self.jitter.underruns;
        let delta = underruns.saturating_sub(self.underruns_seen);
        self.underruns_seen = underruns;
        if delta > 0 && self.auto.record_underruns(delta, std::time::Instant::now()) {
            self.jitter.set_target_ms(self.auto.target_ms);
            self.pending_auto = Some(self.auto.target_ms);
        }
    }
}

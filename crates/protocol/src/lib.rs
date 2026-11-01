//! 音频包与控制通道。音频是小端 PCM，控制消息是长度前缀的 postcard。

use serde::{Deserialize, Serialize};

pub const PROTOCOL_VERSION: u32 = 1;
pub const SAMPLE_RATE: u32 = 48_000;
pub const CHANNELS: u8 = 2;
pub const FRAMES_PER_PACKET: u16 = 240;
pub const BEACON_PORT: u16 = 47611;
pub const CONTROL_PORT: u16 = 47612;

const AUDIO_MAGIC: &[u8; 4] = b"ALKA";
const BEACON_MAGIC: &[u8; 4] = b"ALNB";
const AUDIO_VERSION: u8 = 1;
const BEACON_VERSION: u8 = 1;
pub const AUDIO_HEADER_LEN: usize = 25;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Role {
    Sender,
    Receiver,
}

impl Role {
    pub fn label(self) -> &'static str {
        match self {
            Self::Sender => "发送端",
            Self::Receiver => "接收端",
        }
    }

    fn code(self) -> u8 {
        match self {
            Self::Sender => 1,
            Self::Receiver => 2,
        }
    }

    fn from_code(code: u8) -> Option<Self> {
        match code {
            1 => Some(Self::Sender),
            2 => Some(Self::Receiver),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct AudioPacket {
    pub seq: u32,
    pub timestamp_ns: u64,
    pub sample_rate: u32,
    pub channels: u8,
    pub pcm: Vec<i16>,
}

impl AudioPacket {
    pub fn frame_count(&self) -> usize {
        let channels = self.channels.max(1) as usize;
        self.pcm.len() / channels
    }

    pub fn encode(&self) -> Option<Vec<u8>> {
        if self.channels == 0 || self.sample_rate == 0 {
            return None;
        }
        let frames = self.frame_count();
        if frames == 0 || frames > u16::MAX as usize {
            return None;
        }
        if self.pcm.len() != frames * self.channels as usize {
            return None;
        }
        let mut out = Vec::with_capacity(AUDIO_HEADER_LEN + self.pcm.len() * 2);
        out.extend_from_slice(AUDIO_MAGIC);
        out.push(AUDIO_VERSION);
        out.push(0);
        out.extend_from_slice(&self.seq.to_le_bytes());
        out.extend_from_slice(&self.timestamp_ns.to_le_bytes());
        out.extend_from_slice(&self.sample_rate.to_le_bytes());
        out.push(self.channels);
        out.extend_from_slice(&(frames as u16).to_le_bytes());
        for sample in &self.pcm {
            out.extend_from_slice(&sample.to_le_bytes());
        }
        Some(out)
    }

    pub fn decode(buf: &[u8]) -> Option<Self> {
        if buf.len() < AUDIO_HEADER_LEN || &buf[0..4] != AUDIO_MAGIC || buf[4] != AUDIO_VERSION {
            return None;
        }
        let seq = u32::from_le_bytes(buf[6..10].try_into().ok()?);
        let timestamp_ns = u64::from_le_bytes(buf[10..18].try_into().ok()?);
        let sample_rate = u32::from_le_bytes(buf[18..22].try_into().ok()?);
        let channels = buf[22];
        let frames = u16::from_le_bytes(buf[23..25].try_into().ok()?) as usize;
        if channels == 0 || sample_rate == 0 || frames == 0 {
            return None;
        }
        let sample_count = frames * channels as usize;
        if buf.len() != AUDIO_HEADER_LEN + sample_count * 2 {
            return None;
        }
        let mut pcm = Vec::with_capacity(sample_count);
        for chunk in buf[AUDIO_HEADER_LEN..].chunks_exact(2) {
            pcm.push(i16::from_le_bytes([chunk[0], chunk[1]]));
        }
        Some(Self {
            seq,
            timestamp_ns,
            sample_rate,
            channels,
            pcm,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ControlMessage {
    Hello {
        name: String,
        version: u32,
        role: Role,
        sample_rate: u32,
        channels: u8,
        audio_port: u16,
    },
    StartStream {
        target_latency_ms: u16,
    },
    StopStream,
    SetVolume {
        volume: f32,
    },
    SetMuteLocal {
        mute: bool,
    },
    PlaybackDevices {
        devices: Vec<String>,
        selected: u32,
    },
    Stats {
        loss_pct: f32,
        jitter_ms: f32,
        buffer_ms: f32,
        latency_ms: f32,
        rtt_ms: f32,
        underruns: u32,
        target_latency_ms: u16,
    },
    Ping {
        t_sender_ns: u64,
    },
    Pong {
        t_sender_ns: u64,
        t_recv_ns: u64,
        t_reply_ns: u64,
    },
    SetTargetLatency {
        target_latency_ms: u16,
        automatic: bool,
    },
}

pub fn encode_frame(msg: &ControlMessage) -> Result<Vec<u8>, String> {
    let body = postcard::to_allocvec(msg).map_err(|err| err.to_string())?;
    let mut out = Vec::with_capacity(4 + body.len());
    out.extend_from_slice(&(body.len() as u32).to_le_bytes());
    out.extend_from_slice(&body);
    Ok(out)
}

#[derive(Debug, Default)]
pub struct FrameDecoder {
    buf: Vec<u8>,
}

impl FrameDecoder {
    pub fn new() -> Self {
        Self { buf: Vec::new() }
    }

    pub fn push(&mut self, data: &[u8]) -> Result<Vec<ControlMessage>, String> {
        self.buf.extend_from_slice(data);
        let mut out = Vec::new();
        loop {
            if self.buf.len() < 4 {
                break;
            }
            let len = u32::from_le_bytes(self.buf[0..4].try_into().unwrap()) as usize;
            if len > 1_000_000 {
                self.buf.clear();
                return Err("控制消息过长".into());
            }
            if self.buf.len() < 4 + len {
                break;
            }
            let body = self.buf[4..4 + len].to_vec();
            self.buf.drain(..4 + len);
            let msg = postcard::from_bytes(&body).map_err(|err| err.to_string())?;
            out.push(msg);
        }
        Ok(out)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Beacon {
    pub role: Role,
    pub control_port: u16,
    pub audio_port: u16,
    pub name: String,
}

impl Beacon {
    pub fn encode(&self) -> Option<Vec<u8>> {
        let name = self.name.as_bytes();
        if name.len() > 64 || name.is_empty() {
            return None;
        }
        let mut out = Vec::with_capacity(11 + name.len());
        out.extend_from_slice(BEACON_MAGIC);
        out.push(BEACON_VERSION);
        out.push(self.role.code());
        out.extend_from_slice(&self.control_port.to_le_bytes());
        out.extend_from_slice(&self.audio_port.to_le_bytes());
        out.push(name.len() as u8);
        out.extend_from_slice(name);
        Some(out)
    }

    pub fn decode(buf: &[u8]) -> Option<Self> {
        if buf.len() < 11 || &buf[0..4] != BEACON_MAGIC || buf[4] != BEACON_VERSION {
            return None;
        }
        let role = Role::from_code(buf[5])?;
        let control_port = u16::from_le_bytes(buf[6..8].try_into().ok()?);
        let audio_port = u16::from_le_bytes(buf[8..10].try_into().ok()?);
        let name_len = buf[10] as usize;
        if buf.len() != 11 + name_len || name_len == 0 {
            return None;
        }
        let name = String::from_utf8(buf[11..].to_vec()).ok()?;
        Some(Self {
            role,
            control_port,
            audio_port,
            name,
        })
    }
}

/// 客户端时钟与服务端时钟的偏差，以及往返时间。
/// `t1`/`t4` 属于发 Ping 的一侧，`t2`/`t3` 属于回 Pong 的一侧。
/// 返回的 offset = 回包一侧时钟 − 发 Ping 一侧时钟。
pub fn ntp_offset_and_rtt(t1: u64, t2: u64, t3: u64, t4: u64) -> (i64, u64) {
    let t1 = t1 as i128;
    let t2 = t2 as i128;
    let t3 = t3 as i128;
    let t4 = t4 as i128;
    let offset = ((t2 - t1) + (t3 - t4)) / 2;
    let rtt = (t4 - t1) - (t3 - t2);
    (offset as i64, rtt.max(0) as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn audio_packet_roundtrip() {
        let packet = AudioPacket {
            seq: 7,
            timestamp_ns: 123456,
            sample_rate: SAMPLE_RATE,
            channels: 2,
            pcm: vec![1, -2, 3, -4, 5, -6],
        };
        let bytes = packet.encode().unwrap();
        let decoded = AudioPacket::decode(&bytes).unwrap();
        assert_eq!(decoded, packet);
        assert_eq!(decoded.frame_count(), 3);
    }

    #[test]
    fn audio_packet_rejects_truncated_payload() {
        let packet = AudioPacket {
            seq: 1,
            timestamp_ns: 1,
            sample_rate: SAMPLE_RATE,
            channels: 2,
            pcm: vec![1, 2],
        };
        let mut bytes = packet.encode().unwrap();
        bytes.pop();
        assert!(AudioPacket::decode(&bytes).is_none());
    }

    #[test]
    fn control_frames_reassemble_partial_reads() {
        let first = ControlMessage::Hello {
            name: "mac".into(),
            version: PROTOCOL_VERSION,
            role: Role::Sender,
            sample_rate: SAMPLE_RATE,
            channels: CHANNELS,
            audio_port: 0,
        };
        let second = ControlMessage::SetVolume { volume: 0.5 };
        let mut bytes = encode_frame(&first).unwrap();
        bytes.extend(encode_frame(&second).unwrap());
        let mut decoder = FrameDecoder::new();
        assert!(decoder.push(&bytes[..3]).unwrap().is_empty());
        let msgs = decoder.push(&bytes[3..]).unwrap();
        assert_eq!(msgs, vec![first, second]);
    }

    #[test]
    fn beacon_roundtrip() {
        let beacon = Beacon {
            role: Role::Receiver,
            control_port: CONTROL_PORT,
            audio_port: 9,
            name: "windows-pc".into(),
        };
        let decoded = Beacon::decode(&beacon.encode().unwrap()).unwrap();
        assert_eq!(decoded, beacon);
    }

    #[test]
    fn ntp_same_clock_and_offset_clock() {
        let (offset, rtt) = ntp_offset_and_rtt(0, 10, 10, 20);
        assert_eq!(offset, 0);
        assert_eq!(rtt, 20);
        let (offset, rtt) = ntp_offset_and_rtt(0, 1_010, 1_010, 20);
        assert_eq!(offset, 1_000);
        assert_eq!(rtt, 20);
    }
}

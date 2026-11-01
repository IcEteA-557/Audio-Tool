use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use ringbuf::traits::{Consumer, Producer, Split};
use ringbuf::HeapRb;

use cidre::{arc, cat, cf, core_audio as ca, ns, os};

struct Ctx {
    producer: ringbuf::HeapProd<f32>,
    interleaved: bool,
    is_float: bool,
    dropped: Arc<AtomicU64>,
}

pub struct SystemCapture {
    consumer: ringbuf::HeapCons<f32>,
    sample_rate: u32,
    channels: u16,
    device: Option<ca::hardware::StartedDevice<ca::AggregateDevice>>,
    ctx: Option<Box<Ctx>>,
    tap: Option<ca::TapGuard>,
    pub dropped: Arc<AtomicU64>,
}

impl SystemCapture {
    pub fn start(mute_local: bool, exclude_self: bool) -> Result<Self, String> {
        let output = ca::hardware::System::default_output_device()
            .map_err(|err| format!("读取默认输出设备失败: {err}"))?;
        let output_uid = output
            .uid()
            .map_err(|err| format!("读取输出设备 UID 失败: {err}"))?;

        let excluded = exclude_list(exclude_self)?;
        let mut tap_desc =
            ca::TapDesc::with_stereo_global_tap_excluding_processes(excluded.as_ref());
        tap_desc.set_private(true);
        tap_desc.set_mute_behavior(if mute_local {
            ca::TapMuteBehavior::Muted
        } else {
            ca::TapMuteBehavior::Unmuted
        });
        let name = ns::str!(c"AudioLink");
        tap_desc.set_name(Some(name));
        let tap = tap_desc
            .create_process_tap()
            .map_err(|err| format!("创建系统音频采集失败: {err}。请在系统设置中允许音频互联录制系统音频。"))?;
        let tap_uid = tap
            .uid()
            .map_err(|err| format!("读取采集 UID 失败: {err}"))?;
        let asbd = tap
            .asbd()
            .map_err(|err| format!("读取采集格式失败: {err}"))?;
        let sample_rate = asbd.sample_rate as u32;
        let channels = asbd.channels_per_frame as u16;
        let flags = asbd.format_flags.0;
        let is_float = flags & (1 << 0) != 0;
        let interleaved = flags & (1 << 5) == 0;
        log::info!(
            "系统音频采集 rate={sample_rate} channels={channels} flags={flags:#x} float={is_float} interleaved={interleaved}"
        );

        let sub_device = cf::DictionaryOf::with_keys_values(
            &[ca::sub_device_keys::uid()],
            &[output_uid.as_type_ref()],
        );
        let sub_tap = cf::DictionaryOf::with_keys_values(
            &[ca::sub_device_keys::uid()],
            &[tap_uid.as_type_ref()],
        );
        let agg_uid = cf::Uuid::new().to_cf_string();
        let agg_desc = cf::DictionaryOf::with_keys_values(
            &[
                ca::aggregate_device_keys::is_private(),
                ca::aggregate_device_keys::is_stacked(),
                ca::aggregate_device_keys::tap_auto_start(),
                ca::aggregate_device_keys::name(),
                ca::aggregate_device_keys::main_sub_device(),
                ca::aggregate_device_keys::uid(),
                ca::aggregate_device_keys::sub_device_list(),
                ca::aggregate_device_keys::tap_list(),
            ],
            &[
                cf::Boolean::value_true().as_type_ref(),
                cf::Boolean::value_false().as_type_ref(),
                cf::Boolean::value_true().as_type_ref(),
                cf::str!(c"AudioLink Aggregate").as_type_ref(),
                output_uid.as_type_ref(),
                agg_uid.as_type_ref(),
                cf::ArrayOf::from_slice(&[sub_device.as_ref()]).as_type_ref(),
                cf::ArrayOf::from_slice(&[sub_tap.as_ref()]).as_type_ref(),
            ],
        );
        let mut aggregate = ca::AggregateDevice::with_desc(&agg_desc)
            .map_err(|err| format!("创建聚合设备失败: {err}"))?;
        if let Err(err) = aggregate.set_buf_frame_size(256) {
            log::warn!("未能把采集缓冲设为 256 帧: {err}");
        }

        let dropped = Arc::new(AtomicU64::new(0));
        let ring = HeapRb::<f32>::new(sample_rate.max(48_000) as usize * 2);
        let (producer, consumer) = ring.split();
        let mut ctx = Box::new(Ctx {
            producer,
            interleaved,
            is_float,
            dropped: Arc::clone(&dropped),
        });

        extern "C" fn proc(
            _device: ca::Device,
            _now: &cat::AudioTimeStamp,
            input_data: &cat::AudioBufList<1>,
            _input_time: &cat::AudioTimeStamp,
            _output_data: &mut cat::AudioBufList<1>,
            _output_time: &cat::AudioTimeStamp,
            ctx: Option<&mut Ctx>,
        ) -> os::Status {
            if let Some(ctx) = ctx {
                push_input(ctx, input_data);
            }
            os::Status::NO_ERR
        }

        let proc_id = aggregate
            .create_io_proc_id(proc, Some(&mut *ctx))
            .map_err(|err| format!("注册采集回调失败: {err}"))?;
        let device = ca::device_start(aggregate, Some(proc_id))
            .map_err(|err| format!("启动采集失败: {err}"))?;

        Ok(Self {
            consumer,
            sample_rate: sample_rate.max(1),
            channels: channels.max(1),
            device: Some(device),
            ctx: Some(ctx),
            tap: Some(tap),
            dropped,
        })
    }

    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    pub fn channels(&self) -> u16 {
        self.channels
    }

    pub fn pop_slice(&mut self, dst: &mut [f32]) -> usize {
        self.consumer.pop_slice(dst)
    }
}

impl Drop for SystemCapture {
    fn drop(&mut self) {
        self.device.take();
        self.ctx.take();
        self.tap.take();
    }
}

fn exclude_list(exclude_self: bool) -> Result<arc::R<ns::Array<ns::Number>>, String> {
    if !exclude_self {
        return Ok(ns::Array::new());
    }
    let pid = std::process::id() as i32;
    let processes = ca::hardware::System::processes()
        .map_err(|err| format!("枚举音频进程失败: {err}"))?;
    let Some(process) = processes
        .into_iter()
        .find(|process| process.pid().ok() == Some(pid))
    else {
        return Err("找不到本进程的音频对象，无法在本机试听时排除自身".into());
    };
    let number = ns::Number::with_u32(process.0 .0);
    Ok(ns::Array::from_slice(&[number.as_ref()]))
}

fn push_input(ctx: &mut Ctx, input: &cat::AudioBufList<1>) {
    if !ctx.is_float {
        return;
    }
    let count = input.number_buffers as usize;
    if count == 0 {
        return;
    }
    let buffers = unsafe { std::slice::from_raw_parts(input.buffers.as_ptr(), count) };
    if buffers[0].data.is_null() || buffers[0].data_bytes_size == 0 {
        return;
    }
    if ctx.interleaved || buffers.len() == 1 {
        let sample_count = buffers[0].data_bytes_size as usize / std::mem::size_of::<f32>();
        let data = unsafe { std::slice::from_raw_parts(buffers[0].data as *const f32, sample_count) };
        let pushed = ctx.producer.push_slice(data);
        if pushed < data.len() {
            ctx.dropped
                .fetch_add((data.len() - pushed) as u64, Ordering::Relaxed);
        }
        return;
    }
    let sample_count = buffers[0].data_bytes_size as usize / std::mem::size_of::<f32>();
    let left = unsafe { std::slice::from_raw_parts(buffers[0].data as *const f32, sample_count) };
    let right = if buffers.len() > 1 && !buffers[1].data.is_null() {
        unsafe { std::slice::from_raw_parts(buffers[1].data as *const f32, sample_count) }
    } else {
        left
    };
    let mut chunk = [0.0f32; 512];
    let mut offset = 0;
    while offset < sample_count {
        let frames = (sample_count - offset).min(256);
        for (index, slot) in chunk.iter_mut().take(frames * 2).enumerate() {
            let frame = offset + index / 2;
            *slot = if index % 2 == 0 { left[frame] } else { right[frame] };
        }
        let pushed = ctx.producer.push_slice(&chunk[..frames * 2]);
        if pushed < frames * 2 {
            ctx.dropped
                .fetch_add((frames * 2 - pushed) as u64, Ordering::Relaxed);
        }
        offset += frames;
    }
}

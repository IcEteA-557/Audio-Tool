use rubato::{
    Resampler, SincFixedOut, SincInterpolationParameters, SincInterpolationType, WindowFunction,
};

/// 缓冲偏高时返回小于 1 的比例，让播放更快消耗输入。
/// 5ms 误差大约对应 ±0.1%，最大约 ±0.2%。
pub fn speed_ratio(buffered_frames: usize, target_frames: usize, sample_rate: u32) -> f64 {
    if sample_rate == 0 {
        return 1.0;
    }
    let err_ms =
        (buffered_frames as f64 - target_frames as f64) * 1000.0 / f64::from(sample_rate);
    let ratio = 1.0 - (err_ms / 5.0) * 0.001;
    ratio.clamp(0.998, 1.002)
}

pub struct DriftResampler {
    inner: SincFixedOut<f32>,
}

impl DriftResampler {
    pub fn new(channels: usize, chunk: usize) -> Result<Self, String> {
        let params = SincInterpolationParameters {
            sinc_len: 64,
            f_cutoff: 0.95,
            interpolation: SincInterpolationType::Linear,
            oversampling_factor: 128,
            window: WindowFunction::BlackmanHarris2,
        };
        let inner = SincFixedOut::<f32>::new(1.0, 1.1, params, chunk, channels)
            .map_err(|err| err.to_string())?;
        Ok(Self { inner })
    }

    pub fn input_frames_next(&self) -> usize {
        self.inner.input_frames_next()
    }

    pub fn process(
        &mut self,
        input: &[Vec<f32>],
        ratio: f64,
    ) -> Result<Vec<Vec<f32>>, String> {
        self.inner
            .set_resample_ratio(ratio, true)
            .map_err(|err| err.to_string())?;
        self.inner
            .process(input, None)
            .map_err(|err| err.to_string())
    }
}

/// 把采集采样率一次性变到 48 kHz。比例固定，不再跟随缓冲水位。
pub struct RateConverter {
    inner: SincFixedOut<f32>,
}

impl RateConverter {
    pub fn new(input_rate: u32, output_rate: u32, channels: usize, chunk: usize) -> Result<Self, String> {
        let ratio = f64::from(output_rate) / f64::from(input_rate);
        let params = SincInterpolationParameters {
            sinc_len: 64,
            f_cutoff: 0.95,
            interpolation: SincInterpolationType::Linear,
            oversampling_factor: 128,
            window: WindowFunction::BlackmanHarris2,
        };
        let inner = SincFixedOut::<f32>::new(ratio, 1.05, params, chunk, channels)
            .map_err(|err| err.to_string())?;
        Ok(Self { inner })
    }

    pub fn input_frames_next(&self) -> usize {
        self.inner.input_frames_next()
    }

    pub fn process(&mut self, input: &[Vec<f32>]) -> Result<Vec<Vec<f32>>, String> {
        self.inner
            .process(input, None)
            .map_err(|err| err.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn speed_ratio_follows_buffer_level() {
        assert!(speed_ratio(48_000, 24_000, 48_000) < 1.0);
        assert!(speed_ratio(12_000, 24_000, 48_000) > 1.0);
        assert!((speed_ratio(24_000, 24_000, 48_000) - 1.0).abs() < 1e-9);
        let mild = speed_ratio(24_000 + 48_000 / 200, 24_000, 48_000);
        assert!(mild < 1.0 && mild > 0.998);
    }
}

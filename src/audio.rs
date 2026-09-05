use anyhow::{Context, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{Device, Sample, SampleFormat, StreamConfig};
use std::sync::{Arc, Mutex};

pub const WHISPER_SAMPLE_RATE: u32 = 16000;

pub fn list_input_devices() -> Result<Vec<(String, bool)>> {
    let host = cpal::default_host();
    let default_name = host.default_input_device().and_then(|d| d.name().ok());

    let mut result = Vec::new();
    for device in host
        .input_devices()
        .context("Failed to enumerate input devices")?
    {
        if let Ok(name) = device.name() {
            let is_default = Some(&name) == default_name.as_ref();
            result.push((name, is_default));
        }
    }
    Ok(result)
}

pub fn find_device(name_pattern: &str) -> Result<Device> {
    let host = cpal::default_host();
    if name_pattern.is_empty() {
        return host
            .default_input_device()
            .context("No default input device found");
    }
    let pattern_lower = name_pattern.to_lowercase();
    for device in host
        .input_devices()
        .context("Failed to enumerate input devices")?
    {
        if let Ok(name) = device.name() {
            if name.to_lowercase().contains(&pattern_lower) {
                return Ok(device);
            }
        }
    }
    anyhow::bail!("No input device matching '{}' found", name_pattern)
}

pub struct AudioCapture {
    pub buffer: Arc<Mutex<Vec<f32>>>,
    _stream: cpal::Stream,
}

impl AudioCapture {
    pub fn start(device_name: &str) -> Result<Self> {
        let device = find_device(device_name)?;
        let supported = device
            .default_input_config()
            .context("Failed to get default input config")?;

        let native_rate = supported.sample_rate().0;
        let channels = supported.channels() as usize;
        let sample_format = supported.sample_format();

        tracing::info!(
            "Audio capture: device=\"{}\" rate={}Hz channels={} format={:?}",
            device.name().unwrap_or_else(|_| "unknown".to_string()),
            native_rate,
            channels,
            sample_format,
        );

        let stream_config = StreamConfig {
            channels: supported.channels(),
            sample_rate: supported.sample_rate(),
            buffer_size: cpal::BufferSize::Default,
        };

        let buffer = Arc::new(Mutex::new(Vec::<f32>::new()));
        let buffer_clone = buffer.clone();

        let stream = match sample_format {
            SampleFormat::F32 => build_input_stream::<f32>(
                &device,
                &stream_config,
                buffer_clone,
                channels,
                native_rate,
            )?,
            SampleFormat::I16 => build_input_stream::<i16>(
                &device,
                &stream_config,
                buffer_clone,
                channels,
                native_rate,
            )?,
            SampleFormat::U16 => build_input_stream::<u16>(
                &device,
                &stream_config,
                buffer_clone,
                channels,
                native_rate,
            )?,
            _ => anyhow::bail!("Unsupported sample format: {:?}", sample_format),
        };

        stream.play().context("Failed to start audio stream")?;
        Ok(Self {
            buffer,
            _stream: stream,
        })
    }

    /// Drain all buffered samples and return them.
    pub fn take_samples(&self) -> Vec<f32> {
        let mut buf = self.buffer.lock().unwrap();
        std::mem::take(&mut *buf)
    }
}

fn build_input_stream<T>(
    device: &Device,
    config: &StreamConfig,
    buffer: Arc<Mutex<Vec<f32>>>,
    channels: usize,
    native_rate: u32,
) -> Result<cpal::Stream>
where
    T: cpal::Sample + cpal::SizedSample + 'static,
    f32: cpal::FromSample<T>,
{
    let stream = device.build_input_stream(
        config,
        move |data: &[T], _info: &cpal::InputCallbackInfo| {
            // Mix down to mono f32
            let mono: Vec<f32> = data
                .chunks(channels)
                .map(|ch| {
                    let sum: f32 = ch.iter().map(|&s| f32::from_sample(s)).sum();
                    sum / channels as f32
                })
                .collect();

            let resampled = if native_rate != WHISPER_SAMPLE_RATE {
                resample_linear(&mono, native_rate, WHISPER_SAMPLE_RATE)
            } else {
                mono
            };

            if let Ok(mut buf) = buffer.lock() {
                buf.extend_from_slice(&resampled);
            }
        },
        |err| tracing::error!("Audio stream error: {}", err),
        None,
    )?;
    Ok(stream)
}

/// Simple linear interpolation resampler — good enough for voice.
pub fn resample_linear(input: &[f32], from_rate: u32, to_rate: u32) -> Vec<f32> {
    if from_rate == to_rate || input.is_empty() {
        return input.to_vec();
    }
    let ratio = from_rate as f64 / to_rate as f64;
    let output_len = (input.len() as f64 / ratio) as usize;
    let mut output = Vec::with_capacity(output_len);
    for i in 0..output_len {
        let src = i as f64 * ratio;
        let idx = src as usize;
        let frac = src - idx as f64;
        let s0 = input.get(idx).copied().unwrap_or(0.0);
        let s1 = input.get(idx + 1).copied().unwrap_or(s0);
        output.push(s0 + (s1 - s0) * frac as f32);
    }
    output
}

pub fn save_wav(path: &std::path::Path, samples: &[f32], sample_rate: u32) -> Result<()> {
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate,
        bits_per_sample: 32,
        sample_format: hound::SampleFormat::Float,
    };
    let mut writer = hound::WavWriter::create(path, spec).context("Failed to create WAV file")?;
    for &s in samples {
        writer.write_sample(s)?;
    }
    writer.finalize()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    // ---- resample_linear ----

    #[test]
    fn resample_same_rate_returns_identical() {
        let input = vec![0.1_f32, 0.2, 0.3, 0.4];
        assert_eq!(resample_linear(&input, 16000, 16000), input);
    }

    #[test]
    fn resample_empty_input_returns_empty() {
        assert!(resample_linear(&[], 48000, 16000).is_empty());
    }

    #[test]
    fn resample_upsample_output_length() {
        // 8000 Hz → 16000 Hz: ratio = 0.5, output_len = input_len / 0.5
        let input: Vec<f32> = (0..16).map(|i| i as f32 / 16.0).collect();
        let output = resample_linear(&input, 8000, 16000);
        assert_eq!(output.len(), 32);
    }

    #[test]
    fn resample_downsample_output_length() {
        // 48000 Hz → 16000 Hz: ratio = 3.0, output_len = input_len / 3.0
        let input: Vec<f32> = (0..48).map(|i| i as f32 / 48.0).collect();
        let output = resample_linear(&input, 48000, 16000);
        assert_eq!(output.len(), 16);
    }

    #[test]
    fn resample_dc_signal_preserved() {
        // A constant (DC) signal must remain constant after resampling.
        let input = vec![0.5_f32; 100];
        let output = resample_linear(&input, 48000, 16000);
        for &s in &output {
            assert!(
                (s - 0.5).abs() < 1e-5,
                "DC value should be preserved after resampling, got {}",
                s
            );
        }
    }

    // ---- save_wav ----

    #[test]
    fn save_wav_creates_file() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("test.wav");
        save_wav(&path, &[0.1_f32, 0.5, -0.5, 0.0], WHISPER_SAMPLE_RATE).unwrap();
        assert!(path.exists());
    }

    #[test]
    fn save_wav_round_trip_preserves_samples() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("test.wav");
        let samples = vec![0.1_f32, 0.5, -0.5, 0.0, -0.1];
        save_wav(&path, &samples, WHISPER_SAMPLE_RATE).unwrap();

        let mut reader = hound::WavReader::open(&path).unwrap();
        let spec = reader.spec();
        assert_eq!(spec.sample_rate, WHISPER_SAMPLE_RATE);
        assert_eq!(spec.channels, 1);
        assert_eq!(spec.bits_per_sample, 32);

        let read_back: Vec<f32> = reader.samples::<f32>().map(|s| s.unwrap()).collect();
        assert_eq!(read_back.len(), samples.len());
        for (expected, actual) in samples.iter().zip(read_back.iter()) {
            assert!(
                (expected - actual).abs() < 1e-6,
                "sample mismatch: expected {}, got {}",
                expected,
                actual
            );
        }
    }

    #[test]
    fn save_wav_empty_samples_creates_valid_file() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("empty.wav");
        save_wav(&path, &[], WHISPER_SAMPLE_RATE).unwrap();
        let reader = hound::WavReader::open(&path).unwrap();
        assert_eq!(reader.len(), 0);
    }
}

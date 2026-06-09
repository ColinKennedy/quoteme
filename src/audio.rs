use anyhow::{Context, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{Device, Sample, SampleFormat, StreamConfig};
use std::sync::{Arc, Mutex};

pub const WHISPER_SAMPLE_RATE: u32 = 16000;

pub fn list_input_devices() -> Result<Vec<(String, bool)>> {
    let host = cpal::default_host();
    let default_name = host
        .default_input_device()
        .and_then(|d| d.name().ok());

    let mut result = Vec::new();
    for device in host.input_devices().context("Failed to enumerate input devices")? {
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
    for device in host.input_devices().context("Failed to enumerate input devices")? {
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
        Ok(Self { buffer, _stream: stream })
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
    let mut writer =
        hound::WavWriter::create(path, spec).context("Failed to create WAV file")?;
    for &s in samples {
        writer.write_sample(s)?;
    }
    writer.finalize()?;
    Ok(())
}

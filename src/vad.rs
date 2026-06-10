use std::collections::VecDeque;

const SAMPLE_RATE: usize = 16_000;
const FRAME_SAMPLES: usize = SAMPLE_RATE * 30 / 1000; // 480 samples = 30ms at 16kHz

// Estimate noise floor from the first ~0.5s (assume the user hasn't started speaking yet).
const NOISE_ESTIMATION_FRAMES: usize = 16;
// Speech = anything louder than N× the estimated noise floor.
const SPEECH_THRESHOLD_MULTIPLIER: f32 = 4.0;
// Minimum threshold so we don't treat near-zero-signal recordings as all-speech.
const MIN_THRESHOLD: f32 = 0.002;

const PREFILL_FRAMES: usize = 5;   // 150ms pre-roll captured before onset
const HANGOVER_FRAMES: usize = 10; // 300ms tail kept after speech ends

#[inline]
fn rms(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    let sum_sq: f32 = samples.iter().map(|s| s * s).sum();
    (sum_sq / samples.len() as f32).sqrt()
}

/// Strip non-speech frames from audio before Whisper inference.
///
/// Uses an adaptive RMS energy threshold: noise floor is estimated from the
/// first ~0.5 s of audio, and anything louder than 4× that is kept.
/// Falls through unchanged if the result would be empty (e.g. very noisy input).
pub fn filter_silence(audio: &[f32]) -> Vec<f32> {
    let frames: Vec<&[f32]> = audio.chunks(FRAME_SAMPLES).collect();
    if frames.is_empty() {
        return audio.to_vec();
    }

    // Adaptive threshold: estimate noise floor from the leading frames.
    let noise_frames = frames.len().min(NOISE_ESTIMATION_FRAMES);
    let noise_rms: f32 =
        frames[..noise_frames].iter().map(|f| rms(f)).sum::<f32>() / noise_frames as f32;
    let threshold = (noise_rms * SPEECH_THRESHOLD_MULTIPLIER).max(MIN_THRESHOLD);

    let mut prefill: VecDeque<&[f32]> = VecDeque::new();
    let mut speech_out: Vec<f32> = Vec::with_capacity(audio.len());
    let mut hangover: usize = 0;
    let mut in_speech = false;

    for frame in &frames {
        if rms(frame) > threshold {
            if !in_speech {
                for pre in prefill.drain(..) {
                    speech_out.extend_from_slice(pre);
                }
                in_speech = true;
            }
            hangover = HANGOVER_FRAMES;
            speech_out.extend_from_slice(frame);
        } else if in_speech && hangover > 0 {
            hangover -= 1;
            speech_out.extend_from_slice(frame);
            if hangover == 0 {
                in_speech = false;
            }
        } else {
            in_speech = false;
            prefill.push_back(frame);
            while prefill.len() > PREFILL_FRAMES {
                prefill.pop_front();
            }
        }
    }

    if speech_out.is_empty() {
        tracing::debug!("VAD: no speech detected (threshold={:.4}), passing through", threshold);
        return audio.to_vec();
    }

    let original_secs = audio.len() as f64 / SAMPLE_RATE as f64;
    let kept_secs = speech_out.len() as f64 / SAMPLE_RATE as f64;
    tracing::info!(
        "VAD: {:.2}s → {:.2}s ({:.0}% kept, threshold={:.4})",
        original_secs,
        kept_secs,
        100.0 * kept_secs / original_secs.max(0.001),
        threshold,
    );

    speech_out
}

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

#[cfg(test)]
mod tests {
    use super::*;

    /// Build audio from (amplitude, frame_count) pairs.
    fn build_audio(segments: &[(f32, usize)]) -> Vec<f32> {
        segments
            .iter()
            .flat_map(|&(amp, n)| vec![amp; n * FRAME_SAMPLES])
            .collect()
    }

    #[test]
    fn empty_input_returns_empty() {
        assert!(filter_silence(&[]).is_empty());
    }

    #[test]
    fn all_silence_falls_through_unchanged() {
        // Amplitude well below MIN_THRESHOLD (0.002) so no speech is detected.
        let audio = build_audio(&[(0.0001, 32)]);
        let result = filter_silence(&audio);
        assert_eq!(result.len(), audio.len(), "all-silence audio must pass through unchanged");
        assert_eq!(result[0], 0.0001_f32);
    }

    #[test]
    fn speech_frames_are_stripped_to_correct_length() {
        // 20 quiet frames (noise estimation + headroom) → 3 loud speech frames → 20 quiet frames.
        // Noise RMS ≈ 0.0001; threshold = max(4×0.0001, 0.002) = 0.002.
        // Speech RMS = 0.5 >> 0.002 → kept.
        // Expected output: PREFILL_FRAMES + 3 speech + HANGOVER_FRAMES.
        let audio = build_audio(&[(0.0001, 20), (0.5, 3), (0.0001, 20)]);
        let result = filter_silence(&audio);
        let expected = (PREFILL_FRAMES + 3 + HANGOVER_FRAMES) * FRAME_SAMPLES;
        assert_eq!(result.len(), expected, "VAD should keep prefill + speech + hangover");
        assert!(result.len() < audio.len(), "VAD must strip silent regions");
    }

    #[test]
    fn prefill_frames_precede_speech() {
        // The PREFILL_FRAMES quiet frames before speech onset must appear at the start of output.
        let audio = build_audio(&[(0.0001, 20), (0.5, 3), (0.0001, 20)]);
        let result = filter_silence(&audio);
        // First PREFILL_FRAMES frames must be quiet (amplitude 0.0001).
        for &s in &result[..PREFILL_FRAMES * FRAME_SAMPLES] {
            assert_eq!(s, 0.0001_f32, "prefill samples must be from the silent region");
        }
    }

    #[test]
    fn hangover_tail_follows_last_speech_frame() {
        // The last HANGOVER_FRAMES frames must be the quiet hangover, not speech.
        let audio = build_audio(&[(0.0001, 20), (0.5, 3), (0.0001, 20)]);
        let result = filter_silence(&audio);
        let hangover_start = result.len() - HANGOVER_FRAMES * FRAME_SAMPLES;
        for &s in &result[hangover_start..] {
            assert_eq!(s, 0.0001_f32, "hangover samples must come from the silent tail");
        }
    }

    #[test]
    fn two_speech_bursts_separated_by_long_silence() {
        // Two speech bursts separated by >HANGOVER_FRAMES of silence — second burst is
        // also captured (with its own prefill from the gap).
        let silence = 0.0001_f32;
        let loud = 0.5_f32;
        let gap = HANGOVER_FRAMES + PREFILL_FRAMES + 5; // long enough to end hangover
        let audio = build_audio(&[
            (silence, 20),
            (loud, 2),
            (silence, gap),
            (loud, 2),
            (silence, 20),
        ]);
        let result = filter_silence(&audio);
        // Two bursts → both should be in output; result must be shorter than original.
        assert!(result.len() < audio.len());
        // Result must be non-empty.
        assert!(!result.is_empty());
    }
}

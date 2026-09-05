use anyhow::{Context, Result};
use std::path::Path;
use std::time::Instant;
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

use crate::vad;

/// Prefer a natural pause after this much audio, giving Whisper useful context.
pub const STREAM_MIN_CHUNK_SAMPLES: usize = 25 * 16_000;
/// Never let uncommitted audio grow beyond this duration.
pub const STREAM_CHUNK_SAMPLES: usize = 30 * 16_000;
const MIN_FINAL_NEW_SAMPLES: usize = 250 * 16;

pub struct TranscriptionEngine {
    ctx: Option<WhisperContext>,
    model_path: String,
    last_used: Instant,
    unload_after_secs: u64,
}

impl TranscriptionEngine {
    pub fn new(model_path: String, unload_after_secs: u64) -> Self {
        Self {
            ctx: None,
            model_path,
            last_used: Instant::now(),
            unload_after_secs,
        }
    }

    /// Eagerly load the model. Call at daemon startup so the first transcription
    /// doesn't pay the load cost. Errors here are non-fatal — the model will be
    /// retried on the first `transcribe()` call.
    pub fn load(&mut self) -> Result<()> {
        self.ensure_loaded()
    }

    fn ensure_loaded(&mut self) -> Result<()> {
        if self.ctx.is_some() {
            return Ok(());
        }
        if self.model_path.is_empty() {
            anyhow::bail!(
                "No model path configured. Run: quoteme config set transcription.model_path <path>"
            );
        }
        if !Path::new(&self.model_path).exists() {
            anyhow::bail!("Whisper model not found at: {}", self.model_path);
        }

        // use_gpu is true when compiled with the `cuda` feature (quoteme-cuda binary).
        let ctx_params = WhisperContextParameters::default();
        tracing::info!(
            "Loading Whisper model: \"{}\" (use_gpu={}{})",
            self.model_path,
            ctx_params.use_gpu,
            if !ctx_params.use_gpu {
                " — CPU only; build quoteme-cuda with --features cuda for GPU"
            } else {
                ""
            },
        );

        let t = Instant::now();
        let ctx =
            WhisperContext::new_with_params(&self.model_path, ctx_params).with_context(|| {
                format!(
                    "Failed to load Whisper model from \"{}\". \
                     whisper.cpp requires a GGML/GGUF .bin file — not .safetensors or .pt. \
                     Download a compatible model from https://huggingface.co/ggerganov/whisper.cpp",
                    self.model_path
                )
            })?;
        tracing::info!("Whisper model loaded in {:.2}s", t.elapsed().as_secs_f64());

        self.ctx = Some(ctx);
        Ok(())
    }

    pub fn should_unload(&self) -> bool {
        if self.unload_after_secs == 0 {
            return false;
        }
        self.ctx.is_some() && self.last_used.elapsed().as_secs() >= self.unload_after_secs
    }

    pub fn unload(&mut self) {
        if self.ctx.is_some() {
            tracing::info!("Unloading Whisper model (idle timeout)");
            self.ctx = None;
        }
    }

    pub fn update_model(&mut self, new_path: String, unload_after_secs: u64) {
        if new_path != self.model_path {
            tracing::info!(
                "Model path changed ({:?} → {:?}), unloading current model",
                self.model_path,
                new_path,
            );
            self.ctx = None;
            self.model_path = new_path;
        }
        if unload_after_secs != self.unload_after_secs {
            tracing::info!(
                "unload_after_secs changed ({}s → {}s)",
                self.unload_after_secs,
                unload_after_secs,
            );
        }
        self.unload_after_secs = unload_after_secs;
    }

    pub fn transcribe(
        &mut self,
        audio: &[f32],
        language: &str,
        initial_prompt: Option<&str>,
    ) -> Result<String> {
        self.transcribe_impl(audio, language, initial_prompt, true)
    }

    /// Streaming chunks can begin in the middle of speech, so the recording-level
    /// VAD assumption (quiet audio at the beginning) is invalid for them.
    fn transcribe_stream_chunk(
        &mut self,
        audio: &[f32],
        language: &str,
        initial_prompt: Option<&str>,
    ) -> Result<String> {
        self.transcribe_impl(audio, language, initial_prompt, false)
    }

    fn transcribe_impl(
        &mut self,
        audio: &[f32],
        language: &str,
        initial_prompt: Option<&str>,
        apply_vad: bool,
    ) -> Result<String> {
        let audio_secs = audio.len() as f64 / 16_000.0;
        tracing::debug!(
            "Transcription request: {} samples ({:.2}s audio), language={:?}, prompt_chars={}",
            audio.len(),
            audio_secs,
            language,
            initial_prompt.map_or(0, |p| p.len()),
        );

        let load_start = Instant::now();
        self.ensure_loaded()?;
        let load_elapsed = load_start.elapsed().as_secs_f64();
        if load_elapsed > 0.05 {
            tracing::info!("Model load took {:.2}s", load_elapsed);
        }
        self.last_used = Instant::now();

        let filtered;
        let audio = if apply_vad {
            filtered = vad::filter_silence(audio);
            filtered.as_slice()
        } else {
            audio
        };
        let audio_secs = audio.len() as f64 / 16_000.0;

        let ctx = self.ctx.as_ref().unwrap();
        tracing::debug!("Creating Whisper state…");
        let state_start = Instant::now();
        let mut state = ctx
            .create_state()
            .context("Failed to create Whisper state")?;
        tracing::debug!(
            "State created in {:.3}s",
            state_start.elapsed().as_secs_f64()
        );

        let lang_owned = language.to_string();
        let prompt_owned = initial_prompt.unwrap_or("").to_string();

        let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
        params.set_language(Some(lang_owned.as_str()));
        params.set_print_special(false);
        params.set_print_progress(false);
        params.set_print_realtime(false);
        params.set_print_timestamps(false);
        params.set_no_context(true);
        if !prompt_owned.is_empty() {
            params.set_initial_prompt(prompt_owned.as_str());
            tracing::debug!("Using initial prompt ({} chars)", prompt_owned.len());
        }

        tracing::info!("Running Whisper inference on {:.2}s of audio…", audio_secs);
        let inference_start = Instant::now();
        state
            .full(params, audio)
            .context("Whisper transcription failed")?;
        let inference_secs = inference_start.elapsed().as_secs_f64();
        let rtf = inference_secs / audio_secs.max(0.001);
        tracing::info!(
            "Inference complete: {:.2}s wall-clock for {:.2}s audio ({:.2}x realtime factor{})",
            inference_secs,
            audio_secs,
            rtf,
            if rtf > 2.0 {
                " — consider a smaller model or enabling CUDA"
            } else {
                ""
            },
        );

        let n = state
            .full_n_segments()
            .context("Failed to get segment count")?;
        tracing::debug!("Collecting {} segment(s)…", n);
        let mut text = String::new();
        for i in 0..n {
            text.push_str(
                &state
                    .full_get_segment_text(i)
                    .context("Failed to get segment text")?,
            );
        }
        let text = text.trim().to_string();
        tracing::debug!(
            "Transcription result: {:?} ({} chars, {} segments)",
            text,
            text.len(),
            n,
        );
        Ok(text)
    }
}

/// Builds one transcript incrementally. Completed chunks are retained as text,
/// so stopping only needs to infer the short, not-yet-committed tail.
pub struct StreamingTranscriber {
    language: String,
    base_prompt: String,
    text: String,
    completed_chunks: usize,
    failed: bool,
}

impl StreamingTranscriber {
    pub fn new(language: String, base_prompt: String) -> Self {
        Self {
            language,
            base_prompt,
            text: String::new(),
            completed_chunks: 0,
            failed: false,
        }
    }

    /// Transcribe a chunk while recording continues on another thread.
    pub fn push_chunk(&mut self, engine: &mut TranscriptionEngine, audio: &[f32]) -> Result<()> {
        // Once a chunk has proven unreliable, wait for finish() to do one
        // full-context recovery pass rather than spending time on more chunks.
        if self.failed {
            return Ok(());
        }
        let prompt = self.context_prompt();
        match engine.transcribe_stream_chunk(audio, &self.language, prompt.as_deref()) {
            Ok(chunk_text) => {
                if implausibly_sparse(audio, &chunk_text) {
                    self.failed = true;
                    anyhow::bail!(
                        "Whisper returned only {} characters for {:.1}s of streaming audio; \
                         rejecting the incomplete chunk",
                        chunk_text.chars().filter(|c| !c.is_whitespace()).count(),
                        audio.len() as f64 / 16_000.0,
                    );
                }
                self.text = append_transcript(&self.text, &chunk_text);
                self.completed_chunks += 1;
                tracing::info!(
                    "Streaming transcription committed chunk {} ({} chars total)",
                    self.completed_chunks,
                    self.text.len(),
                );
                Ok(())
            }
            Err(error) => {
                // The caller can fall back to a full pass at finish, preserving
                // correctness if an intermediate inference ever fails.
                self.failed = true;
                Err(error)
            }
        }
    }

    /// Finish the transcript. In the normal case only `tail` is inferred. The
    /// full recording is used solely as a correctness fallback after a chunk error.
    pub fn finish(
        mut self,
        engine: &mut TranscriptionEngine,
        tail: &[f32],
        full_audio: &[f32],
    ) -> Result<String> {
        if self.failed {
            tracing::warn!("A streaming chunk failed; retrying the full recording");
            return engine.transcribe(
                full_audio,
                &self.language,
                (!self.base_prompt.is_empty()).then_some(self.base_prompt.as_str()),
            );
        }

        if tail.len() >= MIN_FINAL_NEW_SAMPLES || self.completed_chunks == 0 {
            self.push_chunk(engine, tail)?;
        }
        Ok(self.text)
    }

    fn context_prompt(&self) -> Option<String> {
        (!self.base_prompt.is_empty()).then(|| self.base_prompt.clone())
    }
}

fn implausibly_sparse(audio: &[f32], text: &str) -> bool {
    let audio_secs = audio.len() as f64 / 16_000.0;
    if audio_secs < 10.0 {
        return false;
    }
    let non_whitespace_chars = text.chars().filter(|c| !c.is_whitespace()).count();
    non_whitespace_chars as f64 / audio_secs < 2.0
}

fn append_transcript(existing: &str, next: &str) -> String {
    let left = existing.trim();
    let right = next.trim();
    if left.is_empty() {
        return right.to_string();
    }
    if right.is_empty() {
        return left.to_string();
    }

    format!("{} {}", left, right)
}

pub fn load_word_list(path: &str) -> Result<String> {
    if path.is_empty() {
        return Ok(String::new());
    }
    if !Path::new(path).exists() {
        tracing::warn!("Word list not found: {}", path);
        return Ok(String::new());
    }
    let raw = std::fs::read_to_string(path).context("Failed to read word list")?;
    let words: Vec<&str> = raw
        .lines()
        .flat_map(|l| l.split(','))
        .map(|w| w.trim())
        .filter(|w| !w.is_empty())
        .collect();
    Ok(words.join(", "))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    // ---- TranscriptionEngine state ----

    #[test]
    fn engine_starts_unloaded_and_should_not_unload() {
        let engine = TranscriptionEngine::new("/fake/model.bin".to_string(), 300);
        // Not loaded yet, so should_unload must be false regardless of timeout.
        assert!(!engine.should_unload());
    }

    #[test]
    fn should_unload_false_when_disabled() {
        let engine = TranscriptionEngine::new("/fake/model.bin".to_string(), 0);
        assert!(
            !engine.should_unload(),
            "unload_after_secs=0 means never unload"
        );
    }

    #[test]
    fn load_fails_on_empty_model_path() {
        let mut engine = TranscriptionEngine::new("".to_string(), 300);
        let err = engine.load().unwrap_err();
        assert!(
            err.to_string().contains("No model path"),
            "expected 'No model path' in error, got: {}",
            err
        );
    }

    #[test]
    fn load_fails_on_nonexistent_model_path() {
        let mut engine =
            TranscriptionEngine::new("/absolutely/nonexistent/model.bin".to_string(), 300);
        let err = engine.load().unwrap_err();
        // Error must mention the missing path.
        assert!(
            err.to_string().contains("nonexistent") || err.to_string().contains("not found"),
            "error should mention the missing path, got: {}",
            err
        );
    }

    #[test]
    fn update_model_changes_path_for_next_load() {
        let mut engine = TranscriptionEngine::new("/path/A".to_string(), 300);
        engine.update_model("/path/B".to_string(), 300);
        // Load should fail referencing /path/B (not A).
        let err = engine.load().unwrap_err();
        assert!(
            err.to_string().contains("/path/B") || err.to_string().contains("path/B"),
            "error should reference updated path, got: {}",
            err
        );
    }

    #[test]
    fn update_model_same_path_does_not_error() {
        let mut engine = TranscriptionEngine::new("/path/A".to_string(), 300);
        // Updating with the same path should not panic.
        engine.update_model("/path/A".to_string(), 60);
        // Still fails on load (file doesn't exist), but path is the same.
        let err = engine.load().unwrap_err();
        assert!(err.to_string().contains("/path/A"));
    }

    // ---- load_word_list ----

    #[test]
    fn word_list_empty_path_returns_empty() {
        assert_eq!(load_word_list("").unwrap(), "");
    }

    #[test]
    fn word_list_nonexistent_path_returns_empty() {
        // Missing file is non-fatal (logged as warning only).
        assert_eq!(load_word_list("/no/such/words.txt").unwrap(), "");
    }

    #[test]
    fn word_list_newline_separated_lines() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("words.txt");
        std::fs::write(&path, "hello\nworld\nrust").unwrap();
        assert_eq!(
            load_word_list(path.to_str().unwrap()).unwrap(),
            "hello, world, rust"
        );
    }

    #[test]
    fn word_list_csv_format_splits_commas() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("words.txt");
        std::fs::write(&path, "foo, bar,baz\nqux").unwrap();
        assert_eq!(
            load_word_list(path.to_str().unwrap()).unwrap(),
            "foo, bar, baz, qux"
        );
    }

    #[test]
    fn word_list_trims_whitespace() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("words.txt");
        std::fs::write(&path, "  hello  \n  world  ").unwrap();
        assert_eq!(
            load_word_list(path.to_str().unwrap()).unwrap(),
            "hello, world"
        );
    }

    #[test]
    fn word_list_filters_empty_lines() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("words.txt");
        std::fs::write(&path, "hello\n\nworld\n\n").unwrap();
        assert_eq!(
            load_word_list(path.to_str().unwrap()).unwrap(),
            "hello, world"
        );
    }

    #[test]
    fn word_list_single_word() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("words.txt");
        std::fs::write(&path, "Anthropic").unwrap();
        assert_eq!(load_word_list(path.to_str().unwrap()).unwrap(), "Anthropic");
    }

    #[test]
    fn chunk_transcripts_are_appended_without_altering_words() {
        assert_eq!(
            append_transcript("Hello there,", "there is no overlap."),
            "Hello there, there is no overlap."
        );
    }

    #[test]
    fn transcript_append_adds_one_space() {
        assert_eq!(append_transcript("hello ", " world"), "hello world");
    }

    #[test]
    fn sparse_long_chunk_is_rejected() {
        assert!(implausibly_sparse(&vec![0.1; 25 * 16_000], "We"));
    }

    #[test]
    fn normal_long_chunk_is_accepted() {
        let text = "This is a normal amount of speech for a longer audio chunk. ".repeat(8);
        assert!(!implausibly_sparse(&vec![0.1; 25 * 16_000], &text));
    }

    #[test]
    fn short_tail_is_never_rejected_for_density() {
        assert!(!implausibly_sparse(&vec![0.1; 5 * 16_000], "Hi"));
    }
}

use anyhow::{Context, Result};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Instant;
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

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

    fn ensure_loaded(&mut self) -> Result<()> {
        if self.ctx.is_some() {
            return Ok(());
        }
        if self.model_path.is_empty() {
            anyhow::bail!(
                "No model path configured. Run: quoteme config transcription.model_path <path>"
            );
        }
        if !Path::new(&self.model_path).exists() {
            anyhow::bail!("Whisper model not found at: {}", self.model_path);
        }
        tracing::info!("Loading Whisper model from {}", self.model_path);
        let ctx = WhisperContext::new_with_params(
            &self.model_path,
            WhisperContextParameters::default(),
        )
        .context("Failed to load Whisper model")?;
        self.ctx = Some(ctx);
        tracing::info!("Whisper model loaded");
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

    pub fn transcribe(
        &mut self,
        audio: &[f32],
        language: &str,
        initial_prompt: Option<&str>,
    ) -> Result<String> {
        self.ensure_loaded()?;
        self.last_used = Instant::now();

        let ctx = self.ctx.as_ref().unwrap();
        let mut state = ctx.create_state().context("Failed to create Whisper state")?;

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
        }

        state.full(params, audio).context("Whisper transcription failed")?;

        let n = state.full_n_segments().context("Failed to get segment count")?;
        let mut text = String::new();
        for i in 0..n {
            text.push_str(
                &state
                    .full_get_segment_text(i)
                    .context("Failed to get segment text")?,
            );
        }
        Ok(text.trim().to_string())
    }
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

// ---------------------------------------------------------------------------
// Streaming (chunked) transcriber
// ---------------------------------------------------------------------------

const CHUNK_SAMPLES: usize = (3.0 * 16000.0) as usize; // 3-second chunks at 16 kHz
const OVERLAP_SAMPLES: usize = (0.5 * 16000.0) as usize;

pub struct StreamingTranscriber {
    engine: Arc<Mutex<TranscriptionEngine>>,
    language: String,
    initial_prompt: String,
    audio: Vec<f32>,
    next_chunk_start: usize,
    context_text: String,
}

impl StreamingTranscriber {
    pub fn new(
        engine: Arc<Mutex<TranscriptionEngine>>,
        language: String,
        initial_prompt: String,
    ) -> Self {
        Self {
            engine,
            language,
            initial_prompt,
            audio: Vec::new(),
            next_chunk_start: 0,
            context_text: String::new(),
        }
    }

    pub fn push_audio(&mut self, samples: &[f32]) {
        self.audio.extend_from_slice(samples);
    }

    /// Transcribe the next pending chunk if enough audio has accumulated.
    /// Returns newly transcribed text when a chunk is processed.
    pub fn try_transcribe_chunk(&mut self) -> Option<String> {
        let start = self.next_chunk_start.saturating_sub(OVERLAP_SAMPLES);
        let end = start + CHUNK_SAMPLES;
        if self.audio.len() < end {
            return None;
        }

        let chunk = self.audio[start..end].to_vec();
        let prompt = self.build_prompt();

        match self.engine.lock() {
            Ok(mut engine) => match engine.transcribe(&chunk, &self.language, Some(&prompt)) {
                Ok(text) if !text.is_empty() => {
                    self.context_text.push(' ');
                    self.context_text.push_str(&text);
                    self.next_chunk_start = end;
                    Some(text)
                }
                Ok(_) => {
                    self.next_chunk_start = end;
                    None
                }
                Err(e) => {
                    tracing::error!("Chunk transcription failed: {}", e);
                    None
                }
            },
            Err(_) => None,
        }
    }

    /// Transcribe the entire accumulated audio in one final pass for highest accuracy.
    /// Returns (transcribed_text, raw_audio).
    pub fn finish(self) -> Result<(String, Vec<f32>)> {
        let audio = self.audio;
        if audio.is_empty() {
            return Ok((String::new(), audio));
        }

        let prompt = {
            let mut parts = Vec::new();
            if !self.context_text.is_empty() {
                let ctx = self.context_text.trim();
                let tail = if ctx.len() > 200 { &ctx[ctx.len() - 200..] } else { ctx };
                parts.push(tail.to_string());
            }
            if !self.initial_prompt.is_empty() {
                parts.push(self.initial_prompt.clone());
            }
            parts.join(" ")
        };

        let text = {
            let mut engine = self
                .engine
                .lock()
                .map_err(|_| anyhow::anyhow!("Engine mutex poisoned"))?;
            engine.transcribe(&audio, &self.language, Some(&prompt))?
        };

        Ok((text, audio))
    }

    fn build_prompt(&self) -> String {
        let mut parts = Vec::new();
        if !self.context_text.is_empty() {
            let ctx = self.context_text.trim();
            let tail = if ctx.len() > 200 { &ctx[ctx.len() - 200..] } else { ctx };
            parts.push(tail.to_string());
        }
        if !self.initial_prompt.is_empty() {
            parts.push(self.initial_prompt.clone());
        }
        parts.join(" ")
    }
}

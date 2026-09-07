use anyhow::{Context, Result};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::audio::AudioCapture;
use crate::audio_mute::{mute_system_audio, unmute_system_audio};
use crate::config::{self, Config, RecordingMode};
use crate::history;
use crate::hotkey::{start_hotkey_listener, HotkeyEvent};
use crate::paste;
use crate::transcription::{
    load_word_list, StreamingTranscriber, TranscriptionEngine, STREAM_CHUNK_SAMPLES,
    STREAM_MIN_CHUNK_SAMPLES,
};

// ---------------------------------------------------------------------------
// PID file helpers
// ---------------------------------------------------------------------------

pub fn pid_path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("quoteme")
        .join("daemon.pid")
}

pub fn log_path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("quoteme")
        .join("daemon.log")
}

pub fn start_daemon() -> Result<()> {
    if let Ok(pid_str) = std::fs::read_to_string(pid_path()) {
        let pid: u32 = pid_str.trim().parse().unwrap_or(0);
        if pid > 0 && process_exists(pid) {
            anyhow::bail!("Daemon is already running (PID {})", pid);
        }
    }

    let exe = std::env::current_exe().context("Failed to get current exe path")?;

    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;

        let child = std::process::Command::new(&exe)
            .arg("daemon")
            .creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP)
            .spawn()
            .context("Failed to spawn daemon")?;

        write_pid(child.id())?;
        println!("Daemon started (PID {})", child.id());
    }

    #[cfg(not(windows))]
    {
        let child = std::process::Command::new(&exe)
            .arg("daemon")
            .spawn()
            .context("Failed to spawn daemon")?;

        write_pid(child.id())?;
        println!("Daemon started (PID {})", child.id());
    }

    Ok(())
}

pub fn stop_daemon() -> Result<()> {
    let path = pid_path();
    if !path.exists() {
        println!("No daemon PID file found — is the daemon running?");
        return Ok(());
    }

    let pid_str = std::fs::read_to_string(&path).context("Failed to read PID file")?;
    let pid: u32 = pid_str.trim().parse().context("Invalid PID in file")?;

    kill_process(pid)?;
    let _ = std::fs::remove_file(&path);
    println!("Daemon stopped (PID {})", pid);
    Ok(())
}

fn write_pid(pid: u32) -> Result<()> {
    let path = pid_path();
    std::fs::create_dir_all(path.parent().unwrap())?;
    std::fs::write(&path, pid.to_string())?;
    Ok(())
}

#[cfg(windows)]
fn process_exists(pid: u32) -> bool {
    use windows::Win32::System::Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};
    unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).is_ok() }
}

#[cfg(not(windows))]
fn process_exists(pid: u32) -> bool {
    std::path::Path::new(&format!("/proc/{}", pid)).exists()
}

#[cfg(windows)]
fn kill_process(pid: u32) -> Result<()> {
    use windows::Win32::System::Threading::{OpenProcess, TerminateProcess, PROCESS_TERMINATE};
    unsafe {
        let h = OpenProcess(PROCESS_TERMINATE, false, pid)
            .with_context(|| format!("Failed to open process {}", pid))?;
        TerminateProcess(h, 0).context("TerminateProcess failed")?;
    }
    Ok(())
}

#[cfg(not(windows))]
fn kill_process(pid: u32) -> Result<()> {
    std::process::Command::new("kill")
        .arg(pid.to_string())
        .status()
        .context("kill failed")?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Recording state machine
// ---------------------------------------------------------------------------

#[derive(Clone, Copy)]
enum RecordSignal {
    Stop,
    Cancel,
}

/// Sent by the recording thread when audio capture ends.
enum RecordResult {
    /// Capture has stopped; streaming transcription may still be flushing its tail.
    Stopped,
    /// Recording was cancelled.
    Cancelled { audio: Vec<f32> },
}

enum StreamCommand {
    Chunk(Vec<f32>),
    Finish {
        tail: Vec<f32>,
        full_audio: Vec<f32>,
        duration: f64,
    },
    Cancel,
}

/// Sent by the transcription thread when Whisper inference completes.
enum TranscriptionResult {
    Done {
        text: String,
        audio: Vec<f32>,
        duration: f64,
        pending: history::PendingEntry,
    },
}

/// Slow clipboard and history operations are serialized on a worker so they can
/// never delay hotkey handling. History directories can contain thousands of
/// recordings, and walking them on the daemon loop used to make key presses
/// appear to do nothing until the walk completed.
enum PostprocessCommand {
    Done {
        config: Config,
        text: String,
        audio: Vec<f32>,
        duration: f64,
        pending: history::PendingEntry,
    },
    Cancelled {
        config: Config,
        audio: Vec<f32>,
        pending: history::PendingEntry,
    },
    Repaste {
        config: Config,
    },
}

struct ActiveRecording {
    stop_tx: std::sync::mpsc::SyncSender<RecordSignal>,
    result_rx: std::sync::mpsc::Receiver<RecordResult>,
    /// True once we've already sent a stop/cancel signal.
    signalled: bool,
    pending: history::PendingEntry,
}

impl ActiveRecording {
    fn signal(&mut self, sig: RecordSignal) {
        if !self.signalled {
            let _ = self.stop_tx.try_send(sig);
            self.signalled = true;
        }
    }
}

// ---------------------------------------------------------------------------
// Main daemon loop
// ---------------------------------------------------------------------------

pub fn run_daemon(mut config: Config) -> Result<()> {
    crate::screenshot::enable_dpi_awareness();
    if config.screenshots.phrase.trim().is_empty() {
        anyhow::bail!(
            "Invalid config: screenshots.phrase cannot be empty (live screenshot detection was not started)"
        );
    }
    // Validate: repaste bound to same key as transcribe is only allowed in toggle mode.
    if !config.hotkeys.repaste.is_empty()
        && config
            .hotkeys
            .repaste
            .eq_ignore_ascii_case(&config.hotkeys.transcribe)
        && config.hotkeys.mode == RecordingMode::PushToTalk
    {
        anyhow::bail!(
            "Invalid config: hotkeys.repaste is \"{}\" (same as transcribe) but \
             hotkeys.mode is push_to_talk — hold is already used for recording. \
             Use a different repaste key or switch to toggle mode.",
            config.hotkeys.repaste
        );
    }

    tracing::info!(
        "Daemon started — transcribe={} cancel={} mode={:?} repaste={:?} \
         consume_transcribe_key={} model={:?} paste={:?} silence_timeout={}s",
        config.hotkeys.transcribe,
        config.hotkeys.cancel,
        config.hotkeys.mode,
        config.hotkeys.repaste,
        config.hotkeys.consume_transcribe_key,
        config.transcription.model_path,
        config.paste.method,
        config.recording.silence_timeout_secs,
    );
    println!(
        "quoteme daemon running. Press {} to {}, {} to cancel.",
        config.hotkeys.transcribe,
        if config.hotkeys.mode == RecordingMode::Toggle {
            "toggle"
        } else {
            "hold"
        },
        config.hotkeys.cancel,
    );

    // Discard any reload sentinel left over from a previous daemon instance.
    let _ = std::fs::remove_file(config::reload_path());

    let mut word_list = load_word_list(&config.transcription.word_list_path)?;
    let engine = Arc::new(Mutex::new(TranscriptionEngine::new(
        config.transcription.model_path.clone(),
        config.transcription.unload_after_secs,
    )));
    let command_engine = Arc::new(Mutex::new(TranscriptionEngine::new(
        config.transcription.model_path.clone(),
        config.transcription.unload_after_secs,
    )));

    // Pre-load in the background. Capturing audio does not need the model, so the
    // hotkey listener and daemon loop should be responsive during a cold start.
    {
        let engine = engine.clone();
        std::thread::spawn(move || {
            let mut eng = engine.lock().expect("engine mutex");
            tracing::info!("Pre-loading Whisper model at daemon startup…");
            if let Err(e) = eng.load() {
                tracing::warn!(
                    "Model pre-load failed — daemon will retry on first transcription: {:#}",
                    e
                );
            }
        });
    }

    let (hotkey_tx, hotkey_rx) = std::sync::mpsc::channel::<HotkeyEvent>();
    start_hotkey_listener(
        config.hotkeys.transcribe.clone(),
        config.hotkeys.cancel.clone(),
        Some(config.hotkeys.repaste.clone()).filter(|s| !s.is_empty()),
        Some(config.hotkeys.image_editor.clone()).filter(|s| !s.trim().is_empty()),
        config.hotkeys.consume_transcribe_key,
        hotkey_tx,
    );

    let (transcription_tx, transcription_rx) = std::sync::mpsc::channel::<TranscriptionResult>();
    let (postprocess_tx, postprocess_rx) = std::sync::mpsc::channel::<PostprocessCommand>();
    spawn_postprocessor(postprocess_rx);
    let mut active: Option<ActiveRecording> = None;
    let unload_in_progress = Arc::new(AtomicBool::new(false));

    // Tap-or-hold state: only active when repaste key == transcribe key in toggle mode.
    let repaste_shares_key = !config.hotkeys.repaste.is_empty()
        && config
            .hotkeys
            .repaste
            .eq_ignore_ascii_case(&config.hotkeys.transcribe);
    const HOLD_THRESHOLD: Duration = Duration::from_millis(500);
    let mut key_down_at: Option<Instant> = None;
    let mut key_down_was_idle = false; // idle (not recording) when key went down
    let mut hold_repaste_fired = false;
    let mut stopped_recording_on_down = false;

    loop {
        // ---- Apply pending config reload (written by `quoteme config set`) ----
        // Only apply when no recording is in progress to avoid mid-flight surprises.
        if active.is_none() {
            let rp = config::reload_path();
            if rp.exists() {
                let _ = std::fs::remove_file(&rp);
                match config::load_config() {
                    Ok(new_cfg) => {
                        if new_cfg.hotkeys.transcribe != config.hotkeys.transcribe
                            || new_cfg.hotkeys.cancel != config.hotkeys.cancel
                            || new_cfg.hotkeys.mode != config.hotkeys.mode
                            || new_cfg.hotkeys.image_editor != config.hotkeys.image_editor
                            || new_cfg.hotkeys.consume_transcribe_key
                                != config.hotkeys.consume_transcribe_key
                        {
                            tracing::warn!(
                                "Hotkey config changed — restart the daemon for hotkey changes to take effect"
                            );
                        }
                        {
                            let mut eng = engine.lock().expect("engine mutex");
                            eng.update_model(
                                new_cfg.transcription.model_path.clone(),
                                new_cfg.transcription.unload_after_secs,
                            );
                        }
                        {
                            let mut eng = command_engine.lock().expect("command engine mutex");
                            eng.update_model(
                                new_cfg.transcription.model_path.clone(),
                                new_cfg.transcription.unload_after_secs,
                            );
                        }
                        word_list = load_word_list(&new_cfg.transcription.word_list_path)
                            .unwrap_or_default();

                        // Log every field that actually changed.
                        macro_rules! log_change {
                            ($key:expr, $old:expr, $new:expr) => {
                                if $old != $new {
                                    tracing::info!(
                                        "Config changed: {} {:?} → {:?}",
                                        $key,
                                        $old,
                                        $new
                                    );
                                }
                            };
                        }
                        log_change!(
                            "transcription.language",
                            &config.transcription.language,
                            &new_cfg.transcription.language
                        );
                        log_change!(
                            "screenshots.phrase",
                            &config.screenshots.phrase,
                            &new_cfg.screenshots.phrase
                        );
                        log_change!(
                            "transcription.word_list_path",
                            &config.transcription.word_list_path,
                            &new_cfg.transcription.word_list_path
                        );
                        log_change!(
                            "transcription.unload_after_secs",
                            config.transcription.unload_after_secs,
                            new_cfg.transcription.unload_after_secs
                        );
                        log_change!(
                            "recording.device",
                            &config.recording.device,
                            &new_cfg.recording.device
                        );
                        log_change!(
                            "recording.mute_system_audio",
                            config.recording.mute_system_audio,
                            new_cfg.recording.mute_system_audio
                        );
                        log_change!(
                            "recording.silence_timeout_secs",
                            config.recording.silence_timeout_secs,
                            new_cfg.recording.silence_timeout_secs
                        );
                        log_change!("paste.method", &config.paste.method, &new_cfg.paste.method);
                        log_change!(
                            "paste.restore_clipboard",
                            config.paste.restore_clipboard,
                            new_cfg.paste.restore_clipboard
                        );
                        log_change!(
                            "history.max_recordings",
                            config.history.max_recordings,
                            new_cfg.history.max_recordings
                        );
                        log_change!(
                            "history.max_age_days",
                            config.history.max_age_days,
                            new_cfg.history.max_age_days
                        );
                        log_change!(
                            "history.save_cancelled",
                            config.history.save_cancelled,
                            new_cfg.history.save_cancelled
                        );
                        tracing::info!("Config reloaded");
                        config = new_cfg;
                    }
                    Err(e) => tracing::warn!("Failed to reload config: {:#}", e),
                }
            }
        }

        // ---- Unload model if idle ----
        if !unload_in_progress.load(Ordering::Acquire) {
            if let Ok(eng) = engine.try_lock() {
                if eng.should_unload() {
                    unload_in_progress.store(true, Ordering::Release);
                    drop(eng);
                    let engine = engine.clone();
                    let unload_in_progress = unload_in_progress.clone();
                    std::thread::spawn(move || {
                        if let Ok(mut eng) = engine.lock() {
                            // Activity may have resumed between the check and lock.
                            if eng.should_unload() {
                                eng.unload();
                            }
                        }
                        unload_in_progress.store(false, Ordering::Release);
                    });
                }
            }
        }

        // ---- Poll for completed recording ----
        // Clear `active` IMMEDIATELY when audio is ready so the user can start a new
        // recording before (or while) transcription runs in its own background thread.
        if let Some(rec) = &active {
            match rec.result_rx.try_recv() {
                Ok(RecordResult::Stopped) => {
                    active = None;
                }
                Ok(RecordResult::Cancelled { audio }) => {
                    let pending = rec.pending.clone();
                    active = None;
                    let _ = postprocess_tx.send(PostprocessCommand::Cancelled {
                        config: config.clone(),
                        audio,
                        pending,
                    });
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    tracing::warn!("Recording thread disconnected unexpectedly");
                    active = None;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
            }
        }

        // ---- Poll for completed transcriptions ----
        while let Ok(result) = transcription_rx.try_recv() {
            let TranscriptionResult::Done {
                text,
                audio,
                duration,
                pending,
            } = result;
            let _ = postprocess_tx.send(PostprocessCommand::Done {
                config: config.clone(),
                text,
                audio,
                duration,
                pending,
            });
        }

        // ---- Tap-or-hold: fire repaste when transcribe key held past threshold ----
        if repaste_shares_key && key_down_was_idle && !hold_repaste_fired {
            if let Some(down_at) = key_down_at {
                if down_at.elapsed() >= HOLD_THRESHOLD {
                    hold_repaste_fired = true;
                    let _ = postprocess_tx.send(PostprocessCommand::Repaste {
                        config: config.clone(),
                    });
                }
            }
        }

        // ---- Process hotkey events ----
        while let Ok(event) = hotkey_rx.try_recv() {
            match event {
                HotkeyEvent::TranscribeDown => {
                    tracing::debug!("Hotkey: TranscribeDown (recording={})", active.is_some());
                    key_down_at = Some(Instant::now());
                    key_down_was_idle = active.is_none();
                    hold_repaste_fired = false;

                    if repaste_shares_key {
                        // Stop recording on key-down; start recording is deferred to key-up (tap).
                        if let Some(rec) = &mut active {
                            tracing::info!("Stop requested");
                            rec.signal(RecordSignal::Stop);
                            stopped_recording_on_down = true;
                        } else {
                            stopped_recording_on_down = false;
                        }
                    } else if config.hotkeys.mode == RecordingMode::Toggle {
                        if let Some(rec) = &mut active {
                            tracing::info!("Stop requested");
                            rec.signal(RecordSignal::Stop);
                        } else {
                            active = spawn_recording(
                                &config,
                                engine.clone(),
                                command_engine.clone(),
                                transcription_tx.clone(),
                                word_list.clone(),
                            );
                        }
                    } else {
                        // PushToTalk
                        if active.is_none() {
                            active = spawn_recording(
                                &config,
                                engine.clone(),
                                command_engine.clone(),
                                transcription_tx.clone(),
                                word_list.clone(),
                            );
                        }
                    }
                }
                HotkeyEvent::TranscribeUp => {
                    tracing::debug!("Hotkey: TranscribeUp");
                    if repaste_shares_key && config.hotkeys.mode == RecordingMode::Toggle {
                        // Tap = start recording; hold = repaste (already fired above).
                        if !hold_repaste_fired && !stopped_recording_on_down && active.is_none() {
                            active = spawn_recording(
                                &config,
                                engine.clone(),
                                command_engine.clone(),
                                transcription_tx.clone(),
                                word_list.clone(),
                            );
                        }
                        stopped_recording_on_down = false;
                    } else if config.hotkeys.mode == RecordingMode::PushToTalk {
                        if let Some(rec) = &mut active {
                            tracing::info!("Stop requested");
                            rec.signal(RecordSignal::Stop);
                        }
                    }
                    key_down_at = None;
                    hold_repaste_fired = false;
                }
                HotkeyEvent::Cancel => {
                    tracing::debug!("Hotkey: Cancel (recording={})", active.is_some());
                    if let Some(rec) = &mut active {
                        rec.signal(RecordSignal::Cancel);
                    }
                }
                HotkeyEvent::Repaste => {
                    tracing::debug!("Hotkey: Repaste");
                    let _ = postprocess_tx.send(PostprocessCommand::Repaste {
                        config: config.clone(),
                    });
                }
                HotkeyEvent::OpenImageEditor => {
                    tracing::debug!("Hotkey: OpenImageEditor");
                    launch_image_editor(&config);
                }
            }
        }

        std::thread::sleep(Duration::from_millis(10));
    }
}

fn spawn_postprocessor(rx: std::sync::mpsc::Receiver<PostprocessCommand>) {
    std::thread::spawn(move || {
        while let Ok(command) = rx.recv() {
            let started = Instant::now();
            match command {
                PostprocessCommand::Done {
                    config,
                    text,
                    audio,
                    duration,
                    pending,
                } => handle_done(&config, &text, &audio, duration, &pending),
                PostprocessCommand::Cancelled {
                    config,
                    audio,
                    pending,
                } => handle_cancelled(&config, &audio, &pending),
                PostprocessCommand::Repaste { config } => do_repaste(&config),
            }
            let elapsed = started.elapsed();
            if elapsed >= Duration::from_secs(1) {
                tracing::warn!(
                    "Background paste/history processing took {:.2}s",
                    elapsed.as_secs_f64()
                );
            } else {
                tracing::debug!(
                    "Background paste/history processing finished in {:.3}s",
                    elapsed.as_secs_f64()
                );
            }
        }
    });
}

fn spawn_recording(
    config: &Config,
    engine: Arc<Mutex<TranscriptionEngine>>,
    command_engine: Arc<Mutex<TranscriptionEngine>>,
    transcription_tx: std::sync::mpsc::Sender<TranscriptionResult>,
    prompt: String,
) -> Option<ActiveRecording> {
    let (stop_tx, stop_rx) = std::sync::mpsc::sync_channel::<RecordSignal>(1);
    let (result_tx, result_rx) = std::sync::mpsc::channel::<RecordResult>();

    let device = config.recording.device.clone();
    let mute = config.recording.mute_system_audio;
    let silence_timeout_secs = config.recording.silence_timeout_secs;
    let language = config.transcription.language.clone();
    let phrase = config.screenshots.phrase.trim().to_string();
    let pending_entry = match history::create_pending_entry(&config.history) {
        Ok(entry) => entry,
        Err(error) => {
            tracing::error!("Could not reserve history directory: {:#}", error);
            return None;
        }
    };

    let (stream_tx, stream_rx) = std::sync::mpsc::channel::<StreamCommand>();
    let latest_probe = Arc::new(Mutex::new(None::<Vec<f32>>));
    let screenshots = Arc::new(Mutex::new(Vec::<PathBuf>::new()));
    let probe_stopped = Arc::new(AtomicBool::new(false));
    let (probe_tx, probe_rx) = std::sync::mpsc::sync_channel::<()>(1);
    let cancelled = Arc::new(AtomicBool::new(false));
    if !phrase.is_empty() {
        spawn_command_spotter(
            probe_rx,
            latest_probe.clone(),
            command_engine,
            config.transcription.language.clone(),
            phrase.clone(),
            pending_entry.clone(),
            screenshots.clone(),
            cancelled.clone(),
            probe_stopped.clone(),
        );
    }
    spawn_streaming_transcription(
        stream_rx,
        language,
        prompt,
        engine,
        transcription_tx,
        cancelled.clone(),
        phrase.clone(),
        pending_entry.clone(),
        screenshots,
    );

    if mute {
        mute_system_audio();
    }

    tracing::info!("Recording requested");
    std::thread::spawn(move || {
        recording_thread(
            device,
            mute,
            silence_timeout_secs,
            stop_rx,
            result_tx,
            stream_tx,
            cancelled,
            (!phrase.is_empty()).then_some(latest_probe),
            (!phrase.is_empty()).then_some(probe_tx),
            probe_stopped,
        );
    });

    Some(ActiveRecording {
        stop_tx,
        result_rx,
        signalled: false,
        pending: pending_entry,
    })
}

#[allow(clippy::too_many_arguments)]
fn spawn_command_spotter(
    rx: std::sync::mpsc::Receiver<()>,
    latest_probe: Arc<Mutex<Option<Vec<f32>>>>,
    engine: Arc<Mutex<TranscriptionEngine>>,
    language: String,
    phrase: String,
    pending: history::PendingEntry,
    screenshots: Arc<Mutex<Vec<PathBuf>>>,
    cancelled: Arc<AtomicBool>,
    stopped: Arc<AtomicBool>,
) {
    std::thread::spawn(move || {
        let mut previous_occurrences = 0;
        let mut last_capture_at: Option<Instant> = None;
        while rx.recv().is_ok() {
            if cancelled.load(Ordering::Acquire) || stopped.load(Ordering::Acquire) {
                return;
            }
            let audio = latest_probe.lock().ok().and_then(|mut slot| slot.take());
            let Some(audio) = audio else { continue };
            let probe_text = match engine.lock() {
                Ok(mut eng) => eng
                    .transcribe_command_probe(&audio, &language, &phrase)
                    .unwrap_or_default(),
                Err(_) => String::new(),
            };
            if cancelled.load(Ordering::Acquire) || stopped.load(Ordering::Acquire) {
                return;
            }
            let occurrences = phrase_occurrences(&probe_text, &phrase);
            let cooled_down = last_capture_at
                .map(|instant| instant.elapsed() >= Duration::from_millis(1500))
                .unwrap_or(true);
            let new_occurrences = occurrences.saturating_sub(previous_occurrences);
            if new_occurrences > 0 && (previous_occurrences > 0 || cooled_down) {
                for _ in 0..new_occurrences {
                    let Ok(mut paths) = screenshots.lock() else {
                        return;
                    };
                    let path = crate::screenshot::next_path(&pending.dir, paths.len() + 1);
                    match crate::screenshot::capture_active_window(&path) {
                        Ok(()) => {
                            tracing::info!("Screenshot command captured {}", path.display());
                            paths.push(path);
                            last_capture_at = Some(Instant::now());
                        }
                        Err(error) => tracing::error!("Screenshot capture failed: {:#}", error),
                    }
                }
            }
            previous_occurrences = occurrences;
        }
    });
}

#[allow(clippy::too_many_arguments)]
fn spawn_streaming_transcription(
    rx: std::sync::mpsc::Receiver<StreamCommand>,
    language: String,
    prompt: String,
    engine: Arc<Mutex<TranscriptionEngine>>,
    tx: std::sync::mpsc::Sender<TranscriptionResult>,
    cancelled: Arc<AtomicBool>,
    phrase: String,
    pending: history::PendingEntry,
    screenshots: Arc<Mutex<Vec<PathBuf>>>,
) {
    std::thread::spawn(move || {
        let mut streaming = StreamingTranscriber::new(language, prompt);
        while let Ok(command) = rx.recv() {
            if cancelled.load(Ordering::Acquire) {
                return;
            }
            match command {
                StreamCommand::Chunk(audio) => match engine.lock() {
                    Ok(mut eng) => {
                        if let Err(error) = streaming.push_chunk(&mut eng, &audio) {
                            tracing::error!("Streaming chunk failed: {:#}", error);
                        }
                    }
                    Err(_) => {
                        tracing::error!("Engine mutex poisoned");
                        return;
                    }
                },
                StreamCommand::Finish {
                    tail,
                    full_audio,
                    duration,
                } => {
                    tracing::info!(
                        "Flushing {:.2}s streaming tail for {:.1}s recording…",
                        tail.len() as f64 / 16_000.0,
                        duration,
                    );
                    let result = match engine.lock() {
                        Ok(mut eng) => streaming.finish(&mut eng, &tail, &full_audio),
                        Err(_) => Err(anyhow::anyhow!("Engine mutex poisoned")),
                    };
                    let text = match result {
                        Ok(text) => text,
                        Err(error) => {
                            tracing::error!("Transcription failed: {:#}", error);
                            String::new()
                        }
                    };
                    let mut screenshots = screenshots.lock().map(|v| v.clone()).unwrap_or_default();
                    let expected = phrase_occurrences(&text, &phrase);
                    while screenshots.len() < expected {
                        let path =
                            crate::screenshot::next_path(&pending.dir, screenshots.len() + 1);
                        match crate::screenshot::capture_active_window(&path) {
                            Ok(()) => screenshots.push(path),
                            Err(error) => {
                                tracing::error!("Final screenshot capture failed: {:#}", error);
                                break;
                            }
                        }
                    }
                    let text = crate::screenshot::inject_paths(&text, &phrase, &screenshots);
                    tracing::info!("Transcription complete: {:?}", text.trim());
                    let _ = tx.send(TranscriptionResult::Done {
                        text,
                        audio: full_audio,
                        duration,
                        pending,
                    });
                    return;
                }
                StreamCommand::Cancel => return,
            }
        }
    });
}

// ---------------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
fn recording_thread(
    device: String,
    mute: bool,
    silence_timeout_secs: u64,
    stop_rx: std::sync::mpsc::Receiver<RecordSignal>,
    result_tx: std::sync::mpsc::Sender<RecordResult>,
    stream_tx: std::sync::mpsc::Sender<StreamCommand>,
    cancelled: Arc<AtomicBool>,
    latest_probe: Option<Arc<Mutex<Option<Vec<f32>>>>>,
    probe_tx: Option<std::sync::mpsc::SyncSender<()>>,
    probe_stopped: Arc<AtomicBool>,
) {
    let capture = match AudioCapture::start(&device) {
        Ok(c) => {
            tracing::debug!("Audio capture started (device: {:?})", device);
            c
        }
        Err(e) => {
            tracing::error!("Failed to start audio capture: {:#}", e);
            cancelled.store(true, Ordering::Release);
            let _ = stream_tx.send(StreamCommand::Cancel);
            let _ = result_tx.send(RecordResult::Cancelled { audio: Vec::new() });
            return;
        }
    };

    let mut audio: Vec<f32> = Vec::new();
    let mut pending: Vec<f32> = Vec::with_capacity(STREAM_CHUNK_SAMPLES);
    let mut probe_window: Vec<f32> = Vec::new();
    let mut last_probe = Instant::now();
    let start = Instant::now();
    // Silence auto-stop: reset whenever speech is detected.
    let mut last_speech_at = Instant::now();
    // RMS threshold distinguishing speech from background noise.
    const SILENCE_RMS_THRESHOLD: f32 = 0.01;
    tracing::info!("Recording…");

    loop {
        // Check the stop signal at the top of every iteration. The recording thread
        // no longer runs Whisper, so it responds in < 100ms regardless of model size.
        match stop_rx.try_recv() {
            Ok(RecordSignal::Stop) => {
                let tail = capture.take_samples();
                // Drop the capture stream immediately so the OS mic indicator clears
                // before the (potentially slow) transcription begins in its own thread.
                drop(capture);
                audio.extend_from_slice(&tail);
                pending.extend_from_slice(&tail);
                if mute {
                    unmute_system_audio();
                }
                let duration = start.elapsed().as_secs_f64();
                probe_stopped.store(true, Ordering::Release);
                tracing::info!("Recording stopped ({:.1}s), flushing tail…", duration);
                let _ = stream_tx.send(StreamCommand::Finish {
                    tail: pending,
                    full_audio: audio,
                    duration,
                });
                let _ = result_tx.send(RecordResult::Stopped);
                return;
            }
            Ok(RecordSignal::Cancel) => {
                let tail = capture.take_samples();
                drop(capture);
                audio.extend_from_slice(&tail);
                cancelled.store(true, Ordering::Release);
                probe_stopped.store(true, Ordering::Release);
                let _ = stream_tx.send(StreamCommand::Cancel);
                if mute {
                    unmute_system_audio();
                }
                tracing::info!("Recording cancelled");
                let _ = result_tx.send(RecordResult::Cancelled { audio });
                return;
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
            Err(std::sync::mpsc::TryRecvError::Disconnected) => return,
        }

        let samples = capture.take_samples();

        if !samples.is_empty() {
            let rms: f32 = {
                let sum_sq: f32 = samples.iter().map(|s| s * s).sum();
                (sum_sq / samples.len() as f32).sqrt()
            };
            if rms > SILENCE_RMS_THRESHOLD {
                last_speech_at = Instant::now();
            }
        }

        audio.extend_from_slice(&samples);
        pending.extend_from_slice(&samples);
        if let Some(latest) = &latest_probe {
            probe_window.extend_from_slice(&samples);
            const PROBE_WINDOW_SAMPLES: usize = 4 * 16_000;
            if probe_window.len() > PROBE_WINDOW_SAMPLES {
                probe_window.drain(..probe_window.len() - PROBE_WINDOW_SAMPLES);
            }
            if probe_window.len() >= 24_000 && last_probe.elapsed() >= Duration::from_millis(900) {
                let should_signal = if let Ok(mut slot) = latest.lock() {
                    let was_empty = slot.is_none();
                    *slot = Some(probe_window.clone());
                    was_empty
                } else {
                    false
                };
                if should_signal {
                    if let Some(tx) = &probe_tx {
                        let _ = tx.try_send(());
                    }
                }
                last_probe = Instant::now();
            }
        }
        queue_ready_chunks(&mut pending, &stream_tx);

        if silence_timeout_secs > 0
            && last_speech_at.elapsed() >= Duration::from_secs(silence_timeout_secs)
        {
            let tail = capture.take_samples();
            drop(capture);
            audio.extend_from_slice(&tail);
            pending.extend_from_slice(&tail);
            if mute {
                unmute_system_audio();
            }
            let duration = start.elapsed().as_secs_f64();
            probe_stopped.store(true, Ordering::Release);
            tracing::info!(
                "Auto-stopped after {}s of silence ({:.1}s total), queuing transcription…",
                silence_timeout_secs,
                duration,
            );
            let _ = stream_tx.send(StreamCommand::Finish {
                tail: pending,
                full_audio: audio,
                duration,
            });
            let _ = result_tx.send(RecordResult::Stopped);
            return;
        }

        std::thread::sleep(Duration::from_millis(10));
    }
}

fn queue_ready_chunks(pending: &mut Vec<f32>, tx: &std::sync::mpsc::Sender<StreamCommand>) {
    if pending.len() >= STREAM_MIN_CHUNK_SAMPLES && is_chunk_boundary_silence(pending) {
        let chunk = std::mem::take(pending);
        let _ = tx.send(StreamCommand::Chunk(chunk));
        return;
    }

    while pending.len() >= STREAM_CHUNK_SAMPLES {
        let remainder = pending.split_off(STREAM_CHUNK_SAMPLES);
        let chunk = std::mem::replace(pending, remainder);
        if tx.send(StreamCommand::Chunk(chunk)).is_err() {
            pending.clear();
            return;
        }
    }
}

fn is_chunk_boundary_silence(audio: &[f32]) -> bool {
    // A single quiet callback can occur inside a word. Require a sustained
    // 250 ms pause before making it a transcription boundary.
    const WINDOW_SAMPLES: usize = 250 * 16;
    const BOUNDARY_RMS_THRESHOLD: f32 = 0.008;
    if audio.len() < WINDOW_SAMPLES {
        return false;
    }
    let window = &audio[audio.len() - WINDOW_SAMPLES..];
    let sum_sq: f32 = window.iter().map(|sample| sample * sample).sum();
    (sum_sq / window.len() as f32).sqrt() <= BOUNDARY_RMS_THRESHOLD
}

fn phrase_occurrences(text: &str, phrase: &str) -> usize {
    if phrase.is_empty() {
        0
    } else {
        text.to_lowercase().matches(&phrase.to_lowercase()).count()
    }
}

// ---------------------------------------------------------------------------

fn do_repaste(config: &Config) {
    match history::list_entries(&config.history) {
        Ok(entries) => match entries.into_iter().find(|e| !e.text.is_empty()) {
            Some(entry) => {
                tracing::info!("Repasting last transcription ({} chars)", entry.text.len());
                if let Err(e) = paste::paste_text(
                    &entry.text,
                    &config.paste.method,
                    config.paste.restore_clipboard,
                ) {
                    tracing::error!("Repaste failed: {:#}", e);
                }
            }
            None => tracing::warn!("Repaste: no non-empty history entry found"),
        },
        Err(e) => tracing::error!("Repaste: failed to load history: {:#}", e),
    }
}

fn launch_image_editor(config: &Config) {
    let executable = std::env::current_exe()
        .map(|path| path.with_file_name("quoteme-images.exe"))
        .unwrap_or_else(|_| PathBuf::from("quoteme-images.exe"));
    let history = history::history_dir(&config.history);
    if let Err(error) = std::process::Command::new(&executable)
        .arg("--history")
        .arg(history)
        .spawn()
    {
        tracing::error!(
            "Failed to launch image editor at {}: {}",
            executable.display(),
            error
        );
    }
}

fn handle_done(
    config: &Config,
    text: &str,
    audio: &[f32],
    duration: f64,
    pending: &history::PendingEntry,
) {
    if !text.is_empty() {
        if let Err(e) =
            paste::paste_text(text, &config.paste.method, config.paste.restore_clipboard)
        {
            eprintln!("Paste failed: {}", e);
        }
        if let Err(e) = history::finalize_entry(pending, text, audio, duration, false) {
            tracing::warn!("Failed to save history: {}", e);
        }
    } else {
        tracing::warn!(
            "Transcription returned empty result for {:.1}s of audio — \
             check model path, audio level, and that the correct language is set",
            duration
        );
        let _ = history::discard_pending_entry(pending);
    }
    let _ = history::cleanup(&config.history);
}

fn handle_cancelled(config: &Config, audio: &[f32], pending: &history::PendingEntry) {
    if config.history.save_cancelled && !audio.is_empty() {
        if let Err(e) = history::finalize_entry(pending, "", audio, 0.0, true) {
            tracing::warn!("Failed to save cancelled recording: {}", e);
        }
    } else {
        let _ = history::discard_pending_entry(pending);
    }
    let _ = history::cleanup(&config.history);
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{HistoryConfig, PasteConfig, PasteMethod};

    /// Config wired to a temp directory with paste=None so tests don't touch the clipboard.
    fn test_config(tmp: &tempfile::TempDir) -> Config {
        Config {
            paste: PasteConfig {
                method: PasteMethod::None,
                restore_clipboard: false,
            },
            history: HistoryConfig {
                path: tmp.path().to_str().unwrap().to_string(),
                ..HistoryConfig::default()
            },
            ..Config::default()
        }
    }

    // ---- handle_done ----

    #[test]
    fn handle_done_empty_text_does_not_save_history() {
        let tmp = tempfile::TempDir::new().unwrap();
        let cfg = test_config(&tmp);
        let pending = history::create_pending_entry(&cfg.history).unwrap();
        handle_done(&cfg, "", &[], 1.0, &pending);
        assert!(crate::history::list_entries(&cfg.history)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn handle_done_nonempty_text_saves_history_entry() {
        let tmp = tempfile::TempDir::new().unwrap();
        let cfg = test_config(&tmp);
        let pending = history::create_pending_entry(&cfg.history).unwrap();
        handle_done(&cfg, "hello world", &[0.1_f32, 0.2], 1.5, &pending);
        let entries = crate::history::list_entries(&cfg.history).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].text, "hello world");
        assert!((entries[0].duration_secs - 1.5).abs() < 1e-6);
        assert!(!entries[0].cancelled);
    }

    #[test]
    fn handle_done_respects_max_recordings_cleanup() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mut cfg = test_config(&tmp);
        cfg.history.max_recordings = 2;

        for i in 0..3 {
            std::thread::sleep(std::time::Duration::from_millis(20));
            let pending = history::create_pending_entry(&cfg.history).unwrap();
            handle_done(&cfg, &format!("text {}", i), &[], 1.0, &pending);
        }
        let entries = crate::history::list_entries(&cfg.history).unwrap();
        assert_eq!(
            entries.len(),
            2,
            "cleanup after handle_done should cap at max_recordings"
        );
    }

    // ---- handle_cancelled ----

    #[test]
    fn handle_cancelled_save_false_does_not_save() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mut cfg = test_config(&tmp);
        cfg.history.save_cancelled = false;
        let pending = history::create_pending_entry(&cfg.history).unwrap();
        handle_cancelled(&cfg, &[0.1_f32, 0.2], &pending);
        assert!(crate::history::list_entries(&cfg.history)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn handle_cancelled_save_true_saves_cancelled_entry() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mut cfg = test_config(&tmp);
        cfg.history.save_cancelled = true;
        let pending = history::create_pending_entry(&cfg.history).unwrap();
        handle_cancelled(&cfg, &[0.1_f32, 0.2], &pending);
        let entries = crate::history::list_entries(&cfg.history).unwrap();
        assert_eq!(entries.len(), 1);
        assert!(entries[0].cancelled);
        assert_eq!(entries[0].text, "");
    }

    #[test]
    fn handle_cancelled_save_true_but_empty_audio_does_not_save() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mut cfg = test_config(&tmp);
        cfg.history.save_cancelled = true;
        let pending = history::create_pending_entry(&cfg.history).unwrap();
        handle_cancelled(&cfg, &[], &pending); // empty audio → guard in handle_cancelled
        assert!(crate::history::list_entries(&cfg.history)
            .unwrap()
            .is_empty());
    }

    fn make_active() -> (ActiveRecording, std::sync::mpsc::Sender<RecordResult>) {
        let (stop_tx, _stop_rx) = std::sync::mpsc::sync_channel::<RecordSignal>(1);
        let (result_tx, result_rx) = std::sync::mpsc::channel::<RecordResult>();
        (
            ActiveRecording {
                stop_tx,
                result_rx,
                signalled: false,
                pending: history::PendingEntry {
                    id: "test".to_string(),
                    timestamp: chrono::Utc::now(),
                    dir: std::env::temp_dir().join("quoteme-test-pending"),
                },
            },
            result_tx,
        )
    }

    #[test]
    fn stopped_clears_active() {
        let (active_rec, result_tx) = make_active();
        let mut active: Option<ActiveRecording> = Some(active_rec);

        result_tx.send(RecordResult::Stopped).unwrap();

        if let Some(rec) = &active {
            if let Ok(RecordResult::Stopped) = rec.result_rx.try_recv() {
                active = None;
            }
        }

        assert!(
            active.is_none(),
            "active must clear as soon as capture stops so a new recording can start"
        );
    }

    #[test]
    fn cancelled_clears_active() {
        let (active_rec, result_tx) = make_active();
        let mut active: Option<ActiveRecording> = Some(active_rec);

        result_tx
            .send(RecordResult::Cancelled { audio: vec![] })
            .unwrap();

        if let Some(rec) = &active {
            if let Ok(RecordResult::Cancelled { .. }) = rec.result_rx.try_recv() {
                active = None;
            }
        }

        assert!(active.is_none());
    }

    #[test]
    fn second_recording_can_start_while_first_transcribes() {
        // Simulate: record → stop → record again before transcription finishes.
        let (active_rec1, result_tx1) = make_active();
        let mut active: Option<ActiveRecording> = Some(active_rec1);

        // First recording capture finishes immediately; its streaming tail may still run.
        result_tx1.send(RecordResult::Stopped).unwrap();

        // Daemon loop: receive Stopped and clear active independently of transcription.
        if let Some(rec) = &active {
            if let Ok(RecordResult::Stopped) = rec.result_rx.try_recv() {
                active = None;
            }
        }
        assert!(
            active.is_none(),
            "active must be None before second recording can start"
        );

        // User presses RAlt again — second recording starts immediately.
        let (active_rec2, _result_tx2) = make_active();
        active = Some(active_rec2);
        assert!(
            active.is_some(),
            "second recording started while first is still transcribing"
        );
    }

    #[test]
    fn stop_signal_sent_only_once() {
        let (mut active_rec, _result_tx) = make_active();

        active_rec.signal(RecordSignal::Stop);
        assert!(active_rec.signalled);

        // Second signal must not panic even though channel is already full.
        active_rec.signal(RecordSignal::Cancel);
        assert!(active_rec.signalled);
    }

    #[test]
    fn cancel_signal_sent_only_once() {
        let (mut active_rec, _result_tx) = make_active();

        active_rec.signal(RecordSignal::Cancel);
        assert!(active_rec.signalled);

        active_rec.signal(RecordSignal::Stop);
        assert!(active_rec.signalled);
    }

    #[test]
    fn hard_limit_queues_a_chunk_during_speech() {
        let (tx, rx) = std::sync::mpsc::channel();
        let mut pending = vec![0.25; STREAM_CHUNK_SAMPLES + 123];
        queue_ready_chunks(&mut pending, &tx);

        match rx.try_recv() {
            Ok(StreamCommand::Chunk(chunk)) => assert_eq!(chunk.len(), STREAM_CHUNK_SAMPLES),
            _ => panic!("Expected one streaming chunk"),
        }
        assert_eq!(pending.len(), 123);
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn natural_pause_queues_whole_chunk_after_minimum() {
        let (tx, rx) = std::sync::mpsc::channel();
        let expected = STREAM_MIN_CHUNK_SAMPLES + 123;
        let mut pending = vec![0.25; expected];
        pending.resize(pending.len() + 250 * 16, 0.0);
        queue_ready_chunks(&mut pending, &tx);

        match rx.try_recv() {
            Ok(StreamCommand::Chunk(chunk)) => assert_eq!(chunk.len(), expected + 250 * 16),
            _ => panic!("Expected a pause-delimited streaming chunk"),
        }
        assert!(pending.is_empty());
    }

    #[test]
    fn short_pause_does_not_queue_too_little_context() {
        let (tx, rx) = std::sync::mpsc::channel();
        let mut pending = vec![0.25; STREAM_MIN_CHUNK_SAMPLES - 1];
        queue_ready_chunks(&mut pending, &tx);

        assert!(rx.try_recv().is_err());
        assert_eq!(pending.len(), STREAM_MIN_CHUNK_SAMPLES - 1);
    }

    #[test]
    fn no_result_while_recording_leaves_active() {
        let (active_rec, _result_tx) = make_active();
        let mut active: Option<ActiveRecording> = Some(active_rec);

        // Nothing sent on result_tx — active should remain Some.
        if let Some(rec) = &active {
            if let Ok(RecordResult::Stopped) = rec.result_rx.try_recv() {
                active = None;
            }
        }

        assert!(
            active.is_some(),
            "active must stay Some while recording is in progress"
        );
    }
}

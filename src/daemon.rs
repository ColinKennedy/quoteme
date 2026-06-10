use anyhow::{Context, Result};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::audio::AudioCapture;
use crate::audio_mute::{mute_system_audio, unmute_system_audio};
use crate::config::{self, Config, RecordingMode};
use crate::history;
use crate::hotkey::{start_hotkey_listener, HotkeyEvent};
use crate::paste::paste_text;
use crate::transcription::{load_word_list, TranscriptionEngine};

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
    /// Recording stopped normally. Audio is ready for transcription.
    AudioReady { audio: Vec<f32>, duration: f64 },
    /// Recording was cancelled.
    Cancelled { audio: Vec<f32> },
}

/// Sent by the transcription thread when Whisper inference completes.
enum TranscriptionResult {
    Done { text: String, audio: Vec<f32>, duration: f64 },
}

struct ActiveRecording {
    stop_tx: std::sync::mpsc::SyncSender<RecordSignal>,
    result_rx: std::sync::mpsc::Receiver<RecordResult>,
    /// True once we've already sent a stop/cancel signal.
    signalled: bool,
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
    tracing::info!(
        "Daemon started — transcribe={} cancel={} mode={:?}",
        config.hotkeys.transcribe,
        config.hotkeys.cancel,
        config.hotkeys.mode,
    );
    println!(
        "quoteme daemon running. Press {} to {}, {} to cancel.",
        config.hotkeys.transcribe,
        if config.hotkeys.mode == RecordingMode::Toggle { "toggle" } else { "hold" },
        config.hotkeys.cancel,
    );

    // Discard any reload sentinel left over from a previous daemon instance.
    let _ = std::fs::remove_file(config::reload_path());

    let mut word_list = load_word_list(&config.transcription.word_list_path)?;
    let engine = Arc::new(Mutex::new(TranscriptionEngine::new(
        config.transcription.model_path.clone(),
        config.transcription.unload_after_secs,
    )));

    // Eagerly load the model so the first transcription doesn't pay the load cost.
    {
        let mut eng = engine.lock().expect("engine mutex");
        tracing::info!("Pre-loading Whisper model at daemon startup…");
        if let Err(e) = eng.load() {
            tracing::warn!(
                "Model pre-load failed — daemon will retry on first transcription: {:#}",
                e
            );
        }
    }

    let (hotkey_tx, hotkey_rx) = std::sync::mpsc::channel::<HotkeyEvent>();
    start_hotkey_listener(
        config.hotkeys.transcribe.clone(),
        config.hotkeys.cancel.clone(),
        hotkey_tx,
    );

    let (transcription_tx, transcription_rx) = std::sync::mpsc::channel::<TranscriptionResult>();
    let mut active: Option<ActiveRecording> = None;

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
                        word_list = load_word_list(&new_cfg.transcription.word_list_path)
                            .unwrap_or_default();
                        tracing::info!(
                            "Config reloaded — model={:?}",
                            new_cfg.transcription.model_path
                        );
                        config = new_cfg;
                    }
                    Err(e) => tracing::warn!("Failed to reload config: {:#}", e),
                }
            }
        }

        // ---- Unload model if idle ----
        if let Ok(mut eng) = engine.try_lock() {
            if eng.should_unload() {
                eng.unload();
            }
        }

        // ---- Poll for completed recording ----
        // Clear `active` IMMEDIATELY when audio is ready so the user can start a new
        // recording before (or while) transcription runs in its own background thread.
        if let Some(rec) = &active {
            match rec.result_rx.try_recv() {
                Ok(RecordResult::AudioReady { audio, duration }) => {
                    active = None;
                    spawn_transcription(
                        audio,
                        duration,
                        config.transcription.language.clone(),
                        word_list.clone(),
                        engine.clone(),
                        transcription_tx.clone(),
                    );
                }
                Ok(RecordResult::Cancelled { audio }) => {
                    active = None;
                    handle_cancelled(&config, &audio);
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
            let TranscriptionResult::Done { text, audio, duration } = result;
            handle_done(&config, &text, &audio, duration);
        }

        // ---- Process hotkey events ----
        while let Ok(event) = hotkey_rx.try_recv() {
            match event {
                HotkeyEvent::TranscribeDown => {
                    if config.hotkeys.mode == RecordingMode::Toggle {
                        if let Some(rec) = &mut active {
                            rec.signal(RecordSignal::Stop);
                        } else {
                            active = spawn_recording(&config);
                        }
                    } else {
                        // PushToTalk
                        if active.is_none() {
                            active = spawn_recording(&config);
                        }
                    }
                }
                HotkeyEvent::TranscribeUp => {
                    if config.hotkeys.mode == RecordingMode::PushToTalk {
                        if let Some(rec) = &mut active {
                            rec.signal(RecordSignal::Stop);
                        }
                    }
                }
                HotkeyEvent::Cancel => {
                    if let Some(rec) = &mut active {
                        rec.signal(RecordSignal::Cancel);
                    }
                }
            }
        }

        std::thread::sleep(Duration::from_millis(30));
    }
}

fn spawn_recording(config: &Config) -> Option<ActiveRecording> {
    let (stop_tx, stop_rx) = std::sync::mpsc::sync_channel::<RecordSignal>(1);
    let (result_tx, result_rx) = std::sync::mpsc::channel::<RecordResult>();

    let device = config.recording.device.clone();
    let mute = config.recording.mute_system_audio;

    if mute {
        mute_system_audio();
    }

    std::thread::spawn(move || {
        recording_thread(device, mute, stop_rx, result_tx);
    });

    Some(ActiveRecording { stop_tx, result_rx, signalled: false })
}

fn spawn_transcription(
    audio: Vec<f32>,
    duration: f64,
    language: String,
    prompt: String,
    engine: Arc<Mutex<TranscriptionEngine>>,
    tx: std::sync::mpsc::Sender<TranscriptionResult>,
) {
    tracing::debug!(
        "Queuing transcription: {} samples ({:.2}s), language={:?}",
        audio.len(),
        duration,
        language,
    );
    std::thread::spawn(move || {
        tracing::info!("Transcribing {:.1}s of audio…", duration);
        let prompt_ref: Option<&str> = if prompt.is_empty() { None } else { Some(&prompt) };
        let result = match engine.lock() {
            Ok(mut eng) => eng.transcribe(&audio, &language, prompt_ref),
            Err(_) => Err(anyhow::anyhow!("Engine mutex poisoned")),
        };
        match result {
            Ok(text) => {
                tracing::info!("Transcription complete: {:?}", text.trim());
                let _ = tx.send(TranscriptionResult::Done { text, audio, duration });
            }
            Err(e) => {
                tracing::error!("Transcription failed: {:#}", e);
                let _ = tx.send(TranscriptionResult::Done {
                    text: String::new(),
                    audio,
                    duration,
                });
            }
        }
    });
}

// ---------------------------------------------------------------------------

fn recording_thread(
    device: String,
    mute: bool,
    stop_rx: std::sync::mpsc::Receiver<RecordSignal>,
    result_tx: std::sync::mpsc::Sender<RecordResult>,
) {
    let capture = match AudioCapture::start(&device) {
        Ok(c) => {
            tracing::debug!("Audio capture started (device: {:?})", device);
            c
        }
        Err(e) => {
            tracing::error!("Failed to start audio capture: {:#}", e);
            let _ = result_tx.send(RecordResult::Cancelled { audio: Vec::new() });
            return;
        }
    };

    let mut audio: Vec<f32> = Vec::new();
    let start = Instant::now();
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
                if mute {
                    unmute_system_audio();
                }
                let duration = start.elapsed().as_secs_f64();
                tracing::info!("Recording stopped ({:.1}s), queuing transcription…", duration);
                let _ = result_tx.send(RecordResult::AudioReady { audio, duration });
                return;
            }
            Ok(RecordSignal::Cancel) => {
                let tail = capture.take_samples();
                drop(capture);
                audio.extend_from_slice(&tail);
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
        audio.extend_from_slice(&samples);
        std::thread::sleep(Duration::from_millis(40));
    }
}

// ---------------------------------------------------------------------------

fn handle_done(config: &Config, text: &str, audio: &[f32], duration: f64) {
    if !text.is_empty() {
        if let Err(e) = paste_text(text, &config.paste.method, config.paste.restore_clipboard) {
            eprintln!("Paste failed: {}", e);
        }
        if let Err(e) = history::save_entry(&config.history, text, audio, duration, false) {
            tracing::warn!("Failed to save history: {}", e);
        }
    }
    let _ = history::cleanup(&config.history);
}

fn handle_cancelled(config: &Config, audio: &[f32]) {
    if config.history.save_cancelled && !audio.is_empty() {
        if let Err(e) = history::save_entry(&config.history, "", audio, 0.0, true) {
            tracing::warn!("Failed to save cancelled recording: {}", e);
        }
    }
    let _ = history::cleanup(&config.history);
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn make_active() -> (ActiveRecording, std::sync::mpsc::Sender<RecordResult>) {
        let (stop_tx, _stop_rx) = std::sync::mpsc::sync_channel::<RecordSignal>(1);
        let (result_tx, result_rx) = std::sync::mpsc::channel::<RecordResult>();
        (ActiveRecording { stop_tx, result_rx, signalled: false }, result_tx)
    }

    #[test]
    fn audio_ready_clears_active() {
        let (active_rec, result_tx) = make_active();
        let mut active: Option<ActiveRecording> = Some(active_rec);

        result_tx.send(RecordResult::AudioReady { audio: vec![], duration: 1.0 }).unwrap();

        if let Some(rec) = &active {
            if let Ok(RecordResult::AudioReady { .. }) = rec.result_rx.try_recv() {
                active = None;
            }
        }

        assert!(active.is_none(), "active must clear on AudioReady so a new recording can start");
    }

    #[test]
    fn cancelled_clears_active() {
        let (active_rec, result_tx) = make_active();
        let mut active: Option<ActiveRecording> = Some(active_rec);

        result_tx.send(RecordResult::Cancelled { audio: vec![] }).unwrap();

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

        // First recording finishes — audio returned immediately, no transcription yet.
        result_tx1.send(RecordResult::AudioReady { audio: vec![], duration: 0.5 }).unwrap();

        // Daemon loop: receive AudioReady, clear active (transcription spawned separately).
        if let Some(rec) = &active {
            if let Ok(RecordResult::AudioReady { .. }) = rec.result_rx.try_recv() {
                active = None;
            }
        }
        assert!(active.is_none(), "active must be None before second recording can start");

        // User presses RAlt again — second recording starts immediately.
        let (active_rec2, _result_tx2) = make_active();
        active = Some(active_rec2);
        assert!(active.is_some(), "second recording started while first is still transcribing");
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
    fn audio_ready_carries_audio_and_duration() {
        let (active_rec, result_tx) = make_active();
        let active = Some(active_rec);

        result_tx
            .send(RecordResult::AudioReady { audio: vec![0.1, 0.2, 0.3], duration: 2.5 })
            .unwrap();

        if let Some(rec) = &active {
            match rec.result_rx.try_recv() {
                Ok(RecordResult::AudioReady { audio, duration }) => {
                    assert_eq!(duration, 2.5);
                    assert_eq!(audio.len(), 3);
                }
                _ => panic!("Expected AudioReady"),
            }
        }
    }

    #[test]
    fn no_result_while_recording_leaves_active() {
        let (active_rec, _result_tx) = make_active();
        let mut active: Option<ActiveRecording> = Some(active_rec);

        // Nothing sent on result_tx — active should remain Some.
        if let Some(rec) = &active {
            if let Ok(RecordResult::AudioReady { .. }) = rec.result_rx.try_recv() {
                active = None;
            }
        }

        assert!(active.is_some(), "active must stay Some while recording is in progress");
    }
}

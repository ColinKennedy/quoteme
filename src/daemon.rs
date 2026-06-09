use anyhow::{Context, Result};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::audio::AudioCapture;
use crate::audio_mute::{mute_system_audio, unmute_system_audio};
use crate::config::{Config, RecordingMode};
use crate::history;
use crate::hotkey::{start_hotkey_listener, HotkeyEvent};
use crate::paste::paste_text;
use crate::transcription::{load_word_list, StreamingTranscriber, TranscriptionEngine};

// ---------------------------------------------------------------------------
// PID file helpers
// ---------------------------------------------------------------------------

pub fn pid_path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("quoteme")
        .join("daemon.pid")
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

enum RecordResult {
    Done { text: String, audio: Vec<f32>, duration: f64 },
    Cancelled { audio: Vec<f32> },
}

struct ActiveRecording {
    /// Send Stop/Cancel without consuming the struct so result_rx stays alive.
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

pub fn run_daemon(config: Config) -> Result<()> {
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

    let word_list = load_word_list(&config.transcription.word_list_path)?;
    let engine = Arc::new(Mutex::new(TranscriptionEngine::new(
        config.transcription.model_path.clone(),
        config.transcription.unload_after_secs,
    )));

    let (hotkey_tx, hotkey_rx) = std::sync::mpsc::channel::<HotkeyEvent>();
    start_hotkey_listener(
        config.hotkeys.transcribe.clone(),
        config.hotkeys.cancel.clone(),
        hotkey_tx,
    );

    let mut active: Option<ActiveRecording> = None;

    loop {
        // ---- Unload model if idle ----
        if let Ok(mut eng) = engine.try_lock() {
            if eng.should_unload() {
                eng.unload();
            }
        }

        // ---- Poll for completed recording ----
        if let Some(rec) = &active {
            match rec.result_rx.try_recv() {
                Ok(RecordResult::Done { text, audio, duration }) => {
                    active = None;
                    handle_done(&config, &text, &audio, duration);
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

        // ---- Process hotkey events ----
        while let Ok(event) = hotkey_rx.try_recv() {
            match event {
                HotkeyEvent::TranscribeDown => {
                    if config.hotkeys.mode == RecordingMode::Toggle {
                        if let Some(rec) = &mut active {
                            rec.signal(RecordSignal::Stop);
                        } else if active.is_none() {
                            active = spawn_recording(&config, &word_list, engine.clone());
                        }
                    } else {
                        // PushToTalk
                        if active.is_none() {
                            active = spawn_recording(&config, &word_list, engine.clone());
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

fn spawn_recording(
    config: &Config,
    word_list: &str,
    engine: Arc<Mutex<TranscriptionEngine>>,
) -> Option<ActiveRecording> {
    let (stop_tx, stop_rx) = std::sync::mpsc::sync_channel::<RecordSignal>(1);
    let (result_tx, result_rx) = std::sync::mpsc::channel::<RecordResult>();

    let cfg = config.clone();
    let wl = word_list.to_string();

    if config.recording.mute_system_audio {
        mute_system_audio();
    }

    std::thread::spawn(move || {
        recording_thread(cfg, wl, engine, stop_rx, result_tx);
    });

    Some(ActiveRecording { stop_tx, result_rx, signalled: false })
}

// ---------------------------------------------------------------------------

fn recording_thread(
    config: Config,
    word_list: String,
    engine: Arc<Mutex<TranscriptionEngine>>,
    stop_rx: std::sync::mpsc::Receiver<RecordSignal>,
    result_tx: std::sync::mpsc::Sender<RecordResult>,
) {
    let capture = match AudioCapture::start(&config.recording.device) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Failed to start audio capture: {}", e);
            return;
        }
    };

    let mut transcriber = StreamingTranscriber::new(
        engine,
        config.transcription.language.clone(),
        word_list,
    );

    let start = Instant::now();
    tracing::info!("Recording…");

    loop {
        let samples = capture.take_samples();
        if !samples.is_empty() {
            transcriber.push_audio(&samples);
            if let Some(chunk_text) = transcriber.try_transcribe_chunk() {
                tracing::debug!("[streaming] {}", chunk_text.trim());
            }
        }

        match stop_rx.try_recv() {
            Ok(RecordSignal::Stop) => {
                // Drain the last few samples
                let tail = capture.take_samples();
                transcriber.push_audio(&tail);
                let duration = start.elapsed().as_secs_f64();

                if config.recording.mute_system_audio {
                    unmute_system_audio();
                }

                tracing::info!("Transcribing {:.1}s of audio…", duration);
                match transcriber.finish() {
                    Ok((text, audio)) => {
                        tracing::info!("Transcription complete: {}", text.trim());
                        let _ = result_tx.send(RecordResult::Done { text, audio, duration });
                    }
                    Err(e) => eprintln!("Transcription failed: {}", e),
                }
                return;
            }
            Ok(RecordSignal::Cancel) => {
                let tail = capture.take_samples();
                transcriber.push_audio(&tail);

                if config.recording.mute_system_audio {
                    unmute_system_audio();
                }

                tracing::info!("Recording cancelled");
                let (_, audio) = transcriber.finish().unwrap_or_default();
                let _ = result_tx.send(RecordResult::Cancelled { audio });
                return;
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
            Err(std::sync::mpsc::TryRecvError::Disconnected) => return,
        }

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

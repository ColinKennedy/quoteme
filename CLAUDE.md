# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Commands

```powershell
# Build CPU binary (default)
cargo build --release                          # → target/release/quoteme.exe

# Build CUDA binary (requires NVIDIA GPU + CUDA Toolkit 12.x)
cargo build --release --features cuda --bin quoteme-cuda   # → target/release/quoteme-cuda.exe

# Build only one at a time (skip the other to save compile time)
cargo build --release --bin quoteme            # CPU only
cargo build --release --features cuda --bin quoteme-cuda  # CUDA only

# Run tests
cargo test --release

# Run a single test
cargo test --release <test_name>

# Run the CPU daemon in the foreground (for development)
cargo run --release -- daemon

# Check compilation without linking
cargo check

# List available audio input devices
cargo run -- list microphone
```

The `RUST_LOG` env var controls log verbosity when running the daemon: `RUST_LOG=quoteme=debug cargo run --release -- daemon`.

## Architecture

`quoteme` is a Windows-first Rust CLI that runs as a background daemon and transcribes speech to text using a local Whisper model (via `whisper-rs`, which wraps `whisper.cpp`).

### Process model

`quoteme start` spawns a detached child process (`quoteme daemon`) and writes its PID to `%APPDATA%\quoteme\daemon.pid`. `quoteme stop` reads that PID and kills the process. The daemon is the only long-running process; all other subcommands are one-shot.

### Daemon event loop (`src/daemon.rs`)

The daemon runs a tight polling loop (~30 ms tick) that:
1. Checks whether `TranscriptionEngine` should be unloaded (idle timeout).
2. Polls the `result_rx` channel for a completed or cancelled recording.
3. Drains the `hotkey_rx` channel and drives the recording state machine.

A recording is represented by `ActiveRecording`, which wraps a `SyncSender<RecordSignal>` and a `Receiver<RecordResult>`. When a hotkey fires, `spawn_recording` launches a dedicated recording thread that owns an `AudioCapture` and a `StreamingTranscriber`. The thread polls for audio samples and sends a `RecordResult` back when it receives `Stop` or `Cancel`.

### Transcription (`src/transcription.rs`)

- **`TranscriptionEngine`** — lazy-loads the Whisper model on first use and drops it after `unload_after_secs` of inactivity. Shared across recordings via `Arc<Mutex<TranscriptionEngine>>`.
- **`StreamingTranscriber`** — accumulates raw audio, runs intermediate 3-second chunk transcriptions with 0.5-second overlap for real-time feedback, then calls `finish()` for one final full-audio pass at highest accuracy. The word list is passed as Whisper's `initial_prompt` (single-pass correction, not a second model pass).

### Audio (`src/audio.rs`)

`AudioCapture` uses `cpal` to open the input device and streams samples into a shared `Arc<Mutex<Vec<f32>>>`. Samples are mixed to mono, converted to `f32`, and linearly resampled to 16 kHz (Whisper's required sample rate). `take_samples()` drains the buffer.

### Configuration (`src/config.rs`)

Config lives at `%APPDATA%\quoteme\config.toml` by default; override with `QUOTEME_CONFIGURATION_FILE`. All fields have sane defaults so the file is optional. `quoteme config <key> <value>` writes individual keys using a flat dotted-path notation (`hotkeys.transcribe`, `recording.device`, etc.).

### History (`src/history.rs`)

Each recording is stored as a UUID-named subdirectory under `%APPDATA%\quoteme\history\` (configurable). Each entry contains `transcription.txt`, `audio.wav`, and `metadata.json`. `cleanup()` enforces `max_recordings` and `max_age_days` limits and is called after every completed or cancelled recording.

### Key crates

| Crate | Role |
|---|---|
| `whisper-rs` | Rust bindings to whisper.cpp |
| `cpal` | Cross-platform audio capture |
| `rdev` | Global keyboard hook (hotkey listener) |
| `enigo` | Simulate Ctrl+V for immediate paste |
| `arboard` | Clipboard read/write |
| `windows` | Windows COM APIs for system audio mute |

## Config keys

| Key | Default | Notes |
|---|---|---|
| `hotkeys.transcribe` | `RAlt` | Key to start/stop recording |
| `hotkeys.cancel` | `Escape` | Key to cancel recording |
| `hotkeys.mode` | `toggle` | `toggle` or `push_to_talk` |
| `recording.device` | *(empty — default mic)* | Substring match against device name |
| `recording.mute_system_audio` | `false` | Mute speakers while recording (Windows only) |
| `transcription.model_path` | *(required)* | Path to `.bin` Whisper model file |
| `transcription.language` | `en` | BCP-47 language code |
| `transcription.word_list_path` | *(empty)* | Plain text / CSV of preferred spellings |
| `transcription.unload_after_secs` | `300` | 0 = never unload |
| `paste.method` | `immediate` | `immediate`, `clipboard`, or `none` |
| `paste.restore_clipboard` | `true` | Restore prior clipboard after immediate paste |
| `history.path` | *(default AppData)* | Override history directory |
| `history.max_recordings` | `0` (unlimited) | Prune oldest beyond this count |
| `history.max_age_days` | `0` (unlimited) | Prune entries older than N days |
| `history.save_cancelled` | `false` | Keep cancelled recordings in history |

# quoteme

A Windows Rust CLI that transcribes speech to text in the background using a local Whisper model.

Long recordings are transcribed incrementally in pause-delimited chunks while you speak. When you
stop, only the unprocessed tail normally remains, so completion time does not grow with the full
length of the recording.

## Building

### Prerequisites

- Rust toolchain (stable)
- For CUDA builds: NVIDIA GPU and CUDA Toolkit 12.x

### CPU build (default)

```powershell
cargo build --release
# Output: target/release/quoteme.exe
```

### CUDA build

```powershell
cargo build --release --features cuda --bin quoteme-cuda
# Output: target/release/quoteme-cuda.exe
```

To build only one binary and skip the other:

```powershell
cargo build --release --bin quoteme              # CPU only
cargo build --release --features cuda --bin quoteme-cuda  # CUDA only
```

### Check compilation without linking

```powershell
cargo check
```

## Running

```powershell
# Start the daemon in the foreground (development)
cargo run --release -- daemon

# Control log verbosity
$env:RUST_LOG = "quoteme=debug"; cargo run --release -- daemon

# List available audio input devices
cargo run -- list microphone
```

## Testing

```powershell
cargo test --release

# Run a single test by name
cargo test --release <test_name>
```

## Configuration

The config file lives at `%APPDATA%\quoteme\config.toml`. Override the path with the `QUOTEME_CONFIGURATION_FILE` environment variable. All fields are optional — sane defaults apply.

```powershell
# Set a value
quoteme configuration set transcription.model_path "C:\models\ggml-medium.en.bin"

# Open the file in your editor
quoteme configuration edit
quoteme configuration edit --run-with code
```

| Key | Default | Notes |
|---|---|---|
| `hotkeys.transcribe` | `RAlt` | Key to start/stop recording |
| `hotkeys.cancel` | `Escape` | Key to cancel recording |
| `hotkeys.mode` | `toggle` | `toggle` or `push_to_talk` |
| `recording.device` | *(default mic)* | Substring match against device name |
| `recording.mute_system_audio` | `false` | Mute speakers while recording (Windows only) |
| `recording.silence_timeout_secs` | `20` | Auto-stop after N seconds of silence; minimum 1 |
| `transcription.model_path` | *(required)* | Path to `.bin` Whisper model file |
| `transcription.language` | `en` | BCP-47 language code |
| `transcription.word_list_path` | *(empty)* | Path to a word list file (see below) |
| `transcription.unload_after_secs` | `300` | Seconds idle before unloading the model; `0` = never |
| `paste.method` | `immediate` | `immediate`, `clipboard`, or `none` |
| `paste.restore_clipboard` | `true` | Restore prior clipboard after immediate paste |
| `history.path` | *(AppData)* | Override history directory |
| `history.max_recordings` | `0` | Prune oldest beyond this count; `0` = unlimited |
| `history.max_age_days` | `0` | Prune entries older than N days; `0` = unlimited |
| `history.save_cancelled` | `false` | Keep cancelled recordings in history |

### Word list format

The word list file biases Whisper toward preferred spellings — useful for names, acronyms, or domain-specific terms that the model often gets wrong. It is passed as Whisper's `initial_prompt`.

The file is plain text and supports two formats — you can mix them freely:

```
# One word or phrase per line
Anthropic
QuoteMe
GPT-4

# Or comma-separated on one line
Kubernetes, kubectl, k8s
```

- Lines and individual entries are trimmed of surrounding whitespace.
- Empty lines and blank entries are ignored.
- No header row or special syntax required.

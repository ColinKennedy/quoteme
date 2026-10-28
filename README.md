# quoteme

A Windows Rust CLI that transcribes speech to text in the background using a local Whisper model.

| <!-- --> | <!-- --> |
| --- | --- |
| CI/CD | [![CI](https://img.shields.io/github/actions/workflow/status/ColinKennedy/quoteme/ci.yml?branch=master&style=for-the-badge&label=CI)](https://github.com/ColinKennedy/quoteme/actions/workflows/ci.yml) |
| License | [![License](https://img.shields.io/github/license/ColinKennedy/quoteme?style=for-the-badge)](https://github.com/ColinKennedy/quoteme) |
| Social | [![RSS](https://img.shields.io/badge/rss-F88900?style=for-the-badge&logo=rss&logoColor=white)](https://github.com/ColinKennedy/quoteme/commits/master.atom) |

Long recordings are transcribed incrementally in pause-delimited chunks while you speak. When you
stop, only the unprocessed tail normally remains, so completion time does not grow with the full
length of the recording.

## Installing (prebuilt release)

Prebuilt Windows binaries are attached to each [GitHub release](https://github.com/ColinKennedy/quoteme/releases).

1. **Pick a binary.**
   - `quoteme-cuda-<version>-windows-x86_64.zip` — GPU-accelerated; needs an NVIDIA GPU (Turing / RTX 20-series or newer) and a current NVIDIA driver. The CUDA runtime DLLs are bundled, so no CUDA Toolkit install is needed.
   - `quoteme-<version>-windows-x86_64.zip` — CPU only; works on any machine, but is slower.
2. **Download a Whisper model.** `ggml-medium.en-q5_0.bin` (English, ~540 MB; quantized medium, a good balance of speed and accuracy) is attached to the release as a quick start. For better accuracy, download a different model such as the full-precision `ggml-medium.en.bin` or the smaller `ggml-small.en.bin` from [ggerganov/whisper.cpp on Hugging Face](https://huggingface.co/ggerganov/whisper.cpp/tree/main). Larger models are more accurate but slower and use more memory. Use a non-`.en` model for languages other than English.
3. **Verify the downloads.** Download `SHA256SUMS` from the release into the same folder and compare:

   ```powershell
   Get-FileHash .\quoteme-cuda-v0.1.0-beta.2-windows-x86_64.zip -Algorithm SHA256
   Get-Content .\SHA256SUMS
   ```

   The hashes must match exactly. (On WSL / Bash: `sha256sum -c SHA256SUMS --ignore-missing`.)
4. **Extract the zip** to a permanent folder, e.g. `C:\Tools\quoteme\`, and keep the DLLs next to the `.exe` in the CUDA build. Optionally add that folder to your `PATH`.
5. **Put the model somewhere permanent** (e.g. `C:\models\ggml-medium.en-q5_0.bin`) and point the config at its on-disk path:

   ```powershell
   .\quoteme-cuda.exe configuration set transcription.model_path "C:\models\ggml-medium.en-q5_0.bin"
   ```

   (Use `quoteme.exe` for the CPU build.) Without `transcription.model_path`, transcription cannot start.
6. **Optionally** check your microphone with `quoteme list microphone` and set `recording.device` to part of its name (see [Configuration](#configuration)).
7. **Start the daemon:**

   ```powershell
   .\quoteme-cuda.exe start
   ```

### Using it

- Press the transcribe hotkey (default: **Right Alt**) to start recording and press it again to stop. The text is pasted into the focused window.
- Press **Escape** to cancel a recording.
- `quoteme stop` stops the daemon. Recordings and transcripts are kept under `%APPDATA%\quoteme\history`.
- The model loads on first use, so the first transcription is slower. Run the daemon in the foreground with `quoteme daemon` to see logs if something isn't working.

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

The config file lives at `%APPDATA%\quoteme\config.toml`. Override the path with the `QUOTEME_CONFIGURATION_FILE` environment variable. All fields are optional â€” sane defaults apply.

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
| `hotkeys.consume_transcribe_key` | `false` | Swallow the transcribe key so it isn't also typed (e.g. Space, Tab) |
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

The word list file biases Whisper toward preferred spellings â€” useful for names, acronyms, or domain-specific terms that the model often gets wrong. It is passed as Whisper's `initial_prompt`.

The file is plain text and supports two formats â€” you can mix them freely:

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

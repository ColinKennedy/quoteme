# quoteme

A Windows Rust CLI that transcribes speech to text in the background using a local Whisper model.

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

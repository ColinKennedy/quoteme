mod audio;
mod audio_mute;
mod config;
mod daemon;
mod health;
mod history;
mod hotkey;
mod paste;
mod transcription;
mod vad;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "quoteme", about = "Minimal Whisper speech-to-text transcription CLI")]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Start the transcription daemon in the background.
    Start,
    /// Stop the running transcription daemon.
    Stop,
    /// List information: history, config, microphones, or log path.
    List {
        #[command(subcommand)]
        action: ListAction,
    },
    /// Health and environment checks.
    Check {
        #[command(subcommand)]
        action: CheckAction,
    },
    /// Configure quoteme settings.
    Config {
        #[command(subcommand)]
        action: ConfigAction,
    },
    /// Run the daemon in the foreground (internal — used by `start`).
    #[command(hide = true)]
    Daemon,
}

#[derive(Subcommand)]
enum ListAction {
    /// List all history entries.
    History {
        /// Copy the transcription at index N to the clipboard (1-based, matches list output).
        #[arg(long, value_name = "N")]
        copy_index: Option<usize>,
    },
    /// Print the active configuration.
    Configuration {
        /// Show only values explicitly set in the config file (omit defaults).
        #[arg(long)]
        no_fallbacks: bool,
    },
    /// List available microphone input devices.
    Microphone,
    /// Print the path to the daemon log file.
    Log,
}

#[derive(Subcommand)]
enum CheckAction {
    /// Validate the environment for common configuration problems.
    Health {
        /// Skip slow checks (model load test). Fast path for quick sanity checks.
        #[arg(long)]
        minimal: bool,
    },
}

#[derive(Subcommand)]
enum ConfigAction {
    /// Set a config value: quoteme config set <key> <value>
    Set {
        key: String,
        value: String,
    },
    /// Interactively configure a setting.
    SetInteractive {
        #[command(subcommand)]
        target: ConfigSetTarget,
    },
}

#[derive(Subcommand)]
enum ConfigSetTarget {
    /// Interactively select and set the recording microphone.
    Microphone,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let is_daemon = matches!(cli.command, Some(Command::Daemon));
    init_tracing(is_daemon);

    match cli.command {
        None | Some(Command::Start) => {
            daemon::start_daemon()?;
        }
        Some(Command::Stop) => {
            daemon::stop_daemon()?;
        }
        Some(Command::Daemon) => {
            let cfg = config::load_config()?;
            daemon::run_daemon(cfg)?;
        }
        Some(Command::List { action: ListAction::History { copy_index } }) => {
            cmd_history_list(copy_index)?;
        }
        Some(Command::List { action: ListAction::Configuration { no_fallbacks } }) => {
            cmd_list_configuration(no_fallbacks)?;
        }
        Some(Command::List { action: ListAction::Microphone }) => {
            cmd_list_microphone()?;
        }
        Some(Command::List { action: ListAction::Log }) => {
            cmd_list_log();
        }
        Some(Command::Check { action: CheckAction::Health { minimal } }) => {
            cmd_check_health(minimal)?;
        }
        Some(Command::Config { action: ConfigAction::Set { key, value } }) => {
            config::set_config_value(&key, &value)?;
            println!("Set {} = {}", key, value);
            println!("Config file: {}", config::config_path().display());
        }
        Some(Command::Config { action: ConfigAction::SetInteractive { target: ConfigSetTarget::Microphone } }) => {
            cmd_config_add_microphone()?;
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Tracing init
// ---------------------------------------------------------------------------

fn init_tracing(is_daemon: bool) {
    if is_daemon {
        // The daemon runs detached — stdout goes nowhere. Write everything to the
        // log file so the user can inspect it with `quoteme list log`.
        let log_path = daemon::log_path();
        if let Some(parent) = log_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let filter = tracing_subscriber::EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("quoteme=debug"));
        match std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)
        {
            Ok(file) => {
                tracing_subscriber::fmt()
                    .with_writer(std::sync::Mutex::new(file))
                    .with_ansi(false)
                    .with_env_filter(filter)
                    .with_target(false)
                    .init();
            }
            Err(e) => {
                // Fallback: stderr, so at least something is visible.
                tracing_subscriber::fmt()
                    .with_env_filter(filter)
                    .with_target(false)
                    .init();
                eprintln!("warning: could not open log file {}: {}", log_path.display(), e);
            }
        }
    } else {
        tracing_subscriber::fmt()
            .with_env_filter(
                tracing_subscriber::EnvFilter::try_from_default_env()
                    .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("quoteme=warn")),
            )
            .with_target(false)
            .init();
    }
}

// ---------------------------------------------------------------------------
// list microphone
// ---------------------------------------------------------------------------

fn cmd_list_microphone() -> Result<()> {
    let devices = audio::list_input_devices()?;
    if devices.is_empty() {
        println!("No input devices found.");
        return Ok(());
    }
    println!("Available microphone inputs:");
    println!("  0. [system default]");
    for (i, (name, is_default)) in devices.iter().enumerate() {
        let tag = if *is_default { "  <- system default" } else { "" };
        println!("  {}. {}{}", i + 1, name, tag);
    }
    println!();
    println!("To set a microphone: quoteme config add microphone");
    println!("To set manually:     quoteme config set recording.device \"<name or substring>\"");
    Ok(())
}

// ---------------------------------------------------------------------------
// list log
// ---------------------------------------------------------------------------

fn cmd_list_log() {
    let path = daemon::log_path();
    println!("{}", path.display());
    if !path.exists() {
        println!("(file will be created when the daemon first starts)");
    }
}

// ---------------------------------------------------------------------------
// config add microphone (interactive)
// ---------------------------------------------------------------------------

fn cmd_config_add_microphone() -> Result<()> {
    let devices = audio::list_input_devices()?;

    println!("Available microphone inputs:");
    println!("  0. [system default]");
    for (i, (name, is_default)) in devices.iter().enumerate() {
        let tag = if *is_default { "  <- system default" } else { "" };
        println!("  {}. {}{}", i + 1, name, tag);
    }
    println!();
    print!("Enter number (0–{}) and press Enter (empty to cancel): ", devices.len());
    std::io::Write::flush(&mut std::io::stdout())?;

    let mut input = String::new();
    std::io::BufRead::read_line(&mut std::io::BufReader::new(std::io::stdin()), &mut input)?;
    let input = input.trim();

    if input.is_empty() {
        println!("Cancelled.");
        return Ok(());
    }

    let n: usize = input.parse().context("Expected a number")?;
    let device_value = if n == 0 {
        String::new()
    } else if n <= devices.len() {
        devices[n - 1].0.clone()
    } else {
        anyhow::bail!("Number out of range (0–{})", devices.len());
    };

    config::set_config_value("recording.device", &device_value)?;

    if device_value.is_empty() {
        println!("Set recording.device = [system default]");
    } else {
        println!("Set recording.device = \"{}\"", device_value);
    }
    println!("Config file: {}", config::config_path().display());
    Ok(())
}

// ---------------------------------------------------------------------------
// list configuration
// ---------------------------------------------------------------------------

fn cmd_list_configuration(no_fallbacks: bool) -> Result<()> {
    let path = config::config_path();

    if no_fallbacks {
        if !path.exists() {
            println!("No config file found — no overrides set.");
            println!("(default location: {})", path.display());
        } else {
            let raw = std::fs::read_to_string(&path)?;
            println!("Config overrides ({})\n", path.display());
            print!("{}", raw.trim_end());
            println!();
        }
    } else {
        // Always print the path first so the user knows where to look.
        if path.exists() {
            println!("Active configuration ({})\n", path.display());
        } else {
            println!("Active configuration — all defaults (no config file at {})\n", path.display());
        }

        match config::load_config() {
            Ok(cfg) => {
                print!("{}", toml::to_string_pretty(&cfg)?.trim_end());
                println!();
            }
            Err(e) => {
                println!("[error] Could not parse config: {:#}", e);
                if path.exists() {
                    println!("\nRaw file contents:");
                    let raw = std::fs::read_to_string(&path)?;
                    print!("{}", raw.trim_end());
                    println!();
                }
            }
        }
    }

    // Use defaults for derived paths if config is unparseable.
    let cfg = config::load_config().unwrap_or_default();
    println!();
    println!("# Derived paths (resolved at runtime)");
    println!("history = {}", history::history_dir(&cfg.history).display());
    println!("log     = {}", daemon::log_path().display());
    println!("pid     = {}", daemon::pid_path().display());
    Ok(())
}

// ---------------------------------------------------------------------------
// check health
// ---------------------------------------------------------------------------

fn cmd_check_health(minimal: bool) -> Result<()> {
    let path = config::config_path();

    let cfg = match config::load_config() {
        Ok(c) => {
            if path.exists() {
                print_check(health::CheckItem::ok(format!("Config file: {}", path.display())));
            } else {
                print_check(health::CheckItem::info(format!(
                    "Config file: not found — using defaults ({})",
                    path.display()
                )));
            }
            c
        }
        Err(e) => {
            print_check(health::CheckItem::fail(format!("Config file: parse error — {}", e)));
            return Ok(());
        }
    };

    // Model path
    let model_item = health::check_model_path(&cfg.transcription.model_path);
    let model_ok = model_item.is_ok();
    print_check(model_item);

    if model_ok {
        if !minimal {
            print!("[    ] Model load test: loading… (use --minimal to skip)");
            std::io::Write::flush(&mut std::io::stdout())?;
            use whisper_rs::{WhisperContext, WhisperContextParameters};
            match WhisperContext::new_with_params(
                &cfg.transcription.model_path,
                WhisperContextParameters::default(),
            ) {
                Ok(_) => println!("\r[ OK ] Model load test: loaded successfully          "),
                Err(e) => println!("\r[FAIL] Model load test: {:#}          ", e),
            }
        } else {
            print_check(health::CheckItem::info("Model load test: skipped (--minimal)"));
        }
    }

    // Word list
    print_check(health::check_word_list(&cfg.transcription.word_list_path));

    // History directory
    let hist_dir = history::history_dir(&cfg.history);
    print_check(health::check_history_dir(&hist_dir));

    // Log file
    let log_path = daemon::log_path();
    if log_path.exists() {
        print_check(health::CheckItem::ok(format!("Log file: \"{}\"", log_path.display())));
    } else {
        print_check(health::CheckItem::info(format!(
            "Log file: \"{}\" (will be created when daemon starts)",
            log_path.display()
        )));
    }

    // Recording device
    if cfg.recording.device.is_empty() {
        print_check(health::CheckItem::ok(
            "Recording device: not set (will use system default microphone)",
        ));
    } else {
        match audio::list_input_devices() {
            Ok(devices) => {
                let needle = cfg.recording.device.to_lowercase();
                let matched: Vec<_> = devices
                    .iter()
                    .filter(|(name, _)| name.to_lowercase().contains(&needle))
                    .collect();
                if matched.is_empty() {
                    print_check(health::CheckItem::fail(format!(
                        "Recording device: no device matching \"{}\" found",
                        cfg.recording.device
                    )));
                    print_check(health::CheckItem::info("  Available devices:"));
                    for (name, is_default) in &devices {
                        let tag = if *is_default { " (default)" } else { "" };
                        print_check(health::CheckItem::info(format!("    - {}{}", name, tag)));
                    }
                } else {
                    print_check(health::CheckItem::ok(format!(
                        "Recording device: \"{}\" matches {} device(s)",
                        cfg.recording.device,
                        matched.len()
                    )));
                }
            }
            Err(e) => {
                print_check(health::CheckItem::warn(format!(
                    "Recording device: could not enumerate devices — {}",
                    e
                )));
            }
        }
    }

    // Hotkeys
    print_check(health::check_hotkeys(&cfg.hotkeys.transcribe, &cfg.hotkeys.cancel));
    print_check(health::check_repaste_hotkey(&cfg.hotkeys));

    Ok(())
}

fn print_check(item: health::CheckItem) {
    match item.status {
        health::Status::Ok   => println!("[ OK ] {}", item.message),
        health::Status::Warn => println!("[WARN] {}", item.message),
        health::Status::Fail => println!("[FAIL] {}", item.message),
        health::Status::Info => println!("[INFO] {}", item.message),
    }
}

// ---------------------------------------------------------------------------
// list history
// ---------------------------------------------------------------------------

fn cmd_history_list(copy_index: Option<usize>) -> Result<()> {
    let cfg = config::load_config()?;
    let entries = history::list_entries(&cfg.history)?;

    if entries.is_empty() {
        println!("No history entries.");
        return Ok(());
    }

    if let Some(n) = copy_index {
        if n == 0 || n > entries.len() {
            anyhow::bail!("Index {} out of range (1–{})", n, entries.len());
        }
        let text = &entries[n - 1].text;
        let mut clipboard = arboard::Clipboard::new().context("Failed to open clipboard")?;
        clipboard.set_text(text).context("Failed to copy to clipboard")?;
        println!("Copied entry {} to clipboard ({} chars)", n, text.len());
        return Ok(());
    }

    let lines: Vec<String> = entries
        .iter()
        .enumerate()
        .map(|(i, e)| {
            let status = if e.cancelled { "[cancelled] " } else { "" };
            let preview: String = e.text.chars().take(80).collect();
            let ellipsis = if e.text.len() > 80 { "…" } else { "" };
            format!(
                "{:>3}. {}  {:.1}s  {}{}{}",
                i + 1,
                e.timestamp.format("%Y-%m-%d %H:%M:%S"),
                e.duration_secs,
                status,
                preview,
                ellipsis,
            )
        })
        .collect();

    const INLINE_THRESHOLD: usize = 40;
    if lines.len() > INLINE_THRESHOLD {
        let tmp = std::env::temp_dir().join("quoteme_history.txt");
        std::fs::write(&tmp, lines.join("\n"))?;
        println!(
            "History ({} entries) written to: {}",
            lines.len(),
            tmp.display()
        );
    } else {
        for line in &lines {
            println!("{}", line);
        }
        println!("\n{} entries total.", lines.len());
    }

    Ok(())
}

mod audio;
mod audio_mute;
mod config;
mod daemon;
mod history;
mod hotkey;
mod paste;
mod transcription;

use anyhow::Result;
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
    /// List available audio input devices.
    Devices,
    /// History subcommands.
    History {
        #[command(subcommand)]
        action: HistoryAction,
    },
    /// Set a config value: quoteme config <key> <value>
    Config {
        key: String,
        value: String,
    },
    /// Run the daemon in the foreground (internal — used by `start`).
    #[command(hide = true)]
    Daemon,
}

#[derive(Subcommand)]
enum HistoryAction {
    /// List all history entries.
    List,
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    // For the daemon subcommand, set up tracing before anything else.
    // Other commands get minimal (warn-level) tracing so they stay quiet.
    let is_daemon = matches!(cli.command, Some(Command::Daemon));
    let default_filter = if is_daemon { "quoteme=info" } else { "quoteme=warn" };
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(default_filter)),
        )
        .with_target(false)
        .init();

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
        Some(Command::Devices) => {
            cmd_devices()?;
        }
        Some(Command::History { action: HistoryAction::List }) => {
            cmd_history_list()?;
        }
        Some(Command::Config { key, value }) => {
            config::set_config_value(&key, &value)?;
            println!("Set {} = {}", key, value);
            println!("Config file: {}", config::config_path().display());
        }
    }

    Ok(())
}

fn cmd_devices() -> Result<()> {
    let devices = audio::list_input_devices()?;
    if devices.is_empty() {
        println!("No input devices found.");
        return Ok(());
    }
    println!("Available input devices:");
    for (name, is_default) in &devices {
        let marker = if *is_default { " (default)" } else { "" };
        println!("  {}{}", name, marker);
    }
    println!();
    println!(
        "Use the device name (or a substring) in your config:\n  quoteme config recording.device \"<name>\""
    );
    Ok(())
}

fn cmd_history_list() -> Result<()> {
    let cfg = config::load_config()?;
    let entries = history::list_entries(&cfg.history)?;

    if entries.is_empty() {
        println!("No history entries.");
        return Ok(());
    }

    let lines: Vec<String> = entries
        .iter()
        .enumerate()
        .map(|(i, e)| {
            let status = if e.cancelled { "[cancelled]" } else { "" };
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

    // If the list is large, write to a temp file instead of flooding the terminal.
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

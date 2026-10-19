//! Hyprland/Wayland desktop backend. Bundle algorithms stay in the shared modules.
pub mod desktop;
pub mod media;
pub mod session;

use std::path::{Path, PathBuf};
use std::process::Command as Process;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail, ensure};
use clap::{Args, Parser, Subcommand, ValueEnum};
use serde_json::{Value, json};

#[derive(Parser)]
#[command(
    name = "scrawl",
    version,
    about = "Screen, voice and drawing capture for agents — Linux/Wayland"
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum Mode {
    Screen,
    Window,
    Area,
}

#[derive(Args)]
struct CaptureArgs {
    #[arg(long, value_enum, default_value = "screen")]
    mode: Mode,
    /// Record without a microphone.
    #[arg(long)]
    no_audio: bool,
    /// New .scrawl directory. Existing paths are never overwritten.
    #[arg(long)]
    out: Option<PathBuf>,
    #[arg(long)]
    note: Option<String>,
    /// Leave the desktop control window closed.
    #[arg(long)]
    no_ui: bool,
}

impl CaptureArgs {
    fn request(&self, command: &str) -> Result<Value> {
        let out = self.out.as_ref().map(std::path::absolute).transpose()?;
        Ok(
            json!({"command": command, "mode": match self.mode { Mode::Screen=>"screen", Mode::Window=>"window", Mode::Area=>"area" },
            "noAudio":self.no_audio,"out":out,"note":self.note}),
        )
    }
}

#[derive(Subcommand)]
enum Command {
    /// Open native recording controls (default).
    App,
    /// Start recording, or finish the current recording. Bind to Alt+Shift+S.
    Toggle(CaptureArgs),
    /// Begin recording until `stop` or the shortcut is pressed.
    Start(CaptureArgs),
    /// Record for a fixed duration and wait for the completed bundle.
    Record {
        #[command(flatten)]
        args: CaptureArgs,
        #[arg(long, default_value_t = 5, value_parser = clap::value_parser!(u64).range(1..=1800))]
        duration: u64,
    },
    /// Finish recording and wait for the completed bundle.
    Stop,
    /// Discard the active recording.
    Cancel,
    /// Toggle the drawing surface while recording. Escape leaves Draw mode.
    Draw,
    /// Remove the most recent drawing stroke.
    Undo,
    /// Print recorder state; suitable for a status bar.
    Status {
        #[arg(long)]
        json: bool,
    },
    /// Stop the service, preserving any in-progress recording for reprocess.
    Quit,
    /// Check desktop and runtime dependencies.
    Doctor,
    /// Generate an annotated sample bundle without capturing anything.
    Demo {
        #[arg(long)]
        out: Option<PathBuf>,
    },
    /// Show the summary or machine-readable manifest.
    Inspect {
        #[arg(default_value = "latest")]
        bundle: PathBuf,
        #[arg(long)]
        json: bool,
    },
    /// Open a bundle's contact sheet with the desktop image viewer.
    Preview {
        #[arg(default_value = "latest")]
        bundle: PathBuf,
    },
    /// Copy a plain-text summary and absolute artifact paths.
    Send {
        #[arg(default_value = "latest")]
        bundle: PathBuf,
        #[arg(long, required = true)]
        clipboard: bool,
    },
    /// Rebuild from an existing recording, in a new bundle.
    Reprocess {
        #[arg(default_value = "latest")]
        bundle: PathBuf,
        #[arg(long)]
        out: PathBuf,
    },
    /// Extract frames from a recording.
    Frames {
        #[arg(default_value = "latest")]
        bundle: PathBuf,
        #[arg(long)]
        from: String,
        #[arg(long)]
        to: String,
        #[arg(long, default_value_t = 4)]
        fps: u32,
    },
    #[command(hide = true)]
    Daemon,
    #[command(hide = true)]
    Ui {
        #[arg(long)]
        check: bool,
        #[arg(long)]
        smoke: bool,
    },
}

pub fn now_epoch_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

pub fn make_bundle_id() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNT: AtomicU64 = AtomicU64::new(0);
    format!(
        "scrawl-{}-{:x}-{}",
        now_epoch_ms(),
        std::process::id(),
        COUNT.fetch_add(1, Ordering::Relaxed)
    )
}

pub fn movie_frames_at(
    movie: &Path,
    times: &[u64],
    offset_ms: u64,
    max_edge: u32,
) -> Result<Vec<crate::model::RgbaFrame>> {
    media::frames_at(movie, times, offset_ms, max_edge)
}

pub fn new_bundle_path(out: Option<PathBuf>) -> Result<PathBuf> {
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
    let path = out.unwrap_or_else(|| {
        crate::handoff::default_captures_dir().join(format!("{}.scrawl", make_bundle_id()))
    });
    ensure!(
        path.extension().is_some_and(|e| e == "scrawl"),
        "Output must be a new .scrawl directory"
    );
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&path)
        .with_context(|| {
            format!(
                "Could not create {}; choose a new output path",
                path.display()
            )
        })?;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700))?;
    Ok(path.canonicalize()?)
}

fn ui(check: bool, smoke: bool) -> Result<()> {
    let mut cmd = Process::new("python3");
    cmd.args(["-c", include_str!("../../helper/linux.py"), "--binary"])
        .arg(std::env::current_exe()?)
        .arg("--socket")
        .arg(session::socket_path()?);
    if check {
        cmd.arg("--check");
    }
    if smoke {
        cmd.arg("--smoke");
    }
    let status = cmd.status().context(
        "Could not start the GTK helper; install python-gobject, python-cairo, gtk4-layer-shell",
    )?;
    ensure!(
        status.success(),
        "The GTK helper exited with {status}; run scrawl doctor"
    );
    Ok(())
}

fn print_status(status: &Value, as_json: bool) {
    if as_json {
        println!("{status}");
    } else {
        println!("{}", status["state"].as_str().unwrap_or("unknown"));
        if let Some(path) = status["bundle"].as_str() {
            println!("Bundle: {path}");
        }
        if let Some(error) = status["error"].as_str() {
            eprintln!("{error}");
        }
    }
}

fn start_command(args: CaptureArgs, toggle: bool) -> Result<()> {
    let status = session::send(
        &args.request(if toggle { "toggle" } else { "start" })?,
        true,
    )?;
    if !args.no_ui {
        session::show_ui()?;
    }
    print_status(&status, false);
    Ok(())
}

fn wait_until_ready() -> Result<Value> {
    let began = std::time::Instant::now();
    loop {
        let status = session::send(&json!({"command":"status"}), false)?;
        match status["state"].as_str() {
            Some("starting" | "processing") => {}
            _ if status["flushRequested"] == true => {}
            Some("error") => bail!("{}", status["error"].as_str().unwrap_or("Recording failed")),
            _ => return Ok(status),
        }
        ensure!(
            began.elapsed() < Duration::from_secs(3600),
            "Processing is still running; use scrawl status to check progress"
        );
        std::thread::sleep(Duration::from_millis(200));
    }
}

pub fn run() -> Result<()> {
    match Cli::parse().command.unwrap_or(Command::App) {
        Command::App => {
            session::ensure_daemon()?;
            ui(false, false)?;
        }
        Command::Ui { check, smoke } => ui(check, smoke)?,
        Command::Daemon => session::serve()?,
        Command::Doctor => desktop::doctor()?,
        Command::Toggle(args) => start_command(args, true)?,
        Command::Start(args) => start_command(args, false)?,
        Command::Record { args, duration } => {
            session::send(&args.request("start")?, true)?;
            if !args.no_ui {
                session::show_ui()?;
            }
            let status = wait_until_ready()?;
            ensure!(status["state"] == "recording", "Recording did not start");
            std::thread::sleep(Duration::from_secs(duration));
            session::send(&json!({"command":"stop"}), false)?;
            print_status(&wait_until_ready()?, false);
        }
        Command::Stop => {
            session::send(&json!({"command":"stop"}), false)?;
            print_status(&wait_until_ready()?, false);
        }
        Command::Cancel => {
            print_status(&session::send(&json!({"command":"cancel"}), false)?, false)
        }
        Command::Draw => {
            print_status(&session::send(&json!({"command":"draw"}), false)?, false);
            session::show_ui()?;
        }
        Command::Undo => print_status(&session::send(&json!({"command":"undo"}), false)?, false),
        Command::Quit => {
            session::send(&json!({"command":"shutdown"}), false)?;
        }
        Command::Status { json: as_json } => {
            let status = session::status_or_idle()?;
            print_status(&status, as_json);
        }
        Command::Demo { out } => {
            let path = new_bundle_path(out)?;
            media::demo(&path)?;
            println!("{}", path.display());
        }
        Command::Inspect {
            bundle,
            json: as_json,
        } => {
            let (dir, bundle) = crate::handoff::load_bundle(&bundle)?;
            if as_json {
                println!("{}", serde_json::to_string_pretty(&bundle)?);
            } else {
                let summary = std::fs::read_to_string(dir.join("summary.md"))
                    .unwrap_or_else(|_| crate::sketch::summary_markdown(&bundle, &dir));
                print!("{summary}");
            }
        }
        Command::Preview { bundle } => {
            let (dir, bundle) = crate::handoff::load_bundle(&bundle)?;
            ensure!(
                Process::new("xdg-open")
                    .arg(dir.join(&bundle.contact_sheet))
                    .status()?
                    .success(),
                "Could not open the contact sheet"
            );
        }
        Command::Send { bundle, .. } => {
            crate::handoff::copy_bundle_to_clipboard(&bundle)?;
            println!("Copied summary and artifact paths");
        }
        Command::Reprocess { bundle, out } => {
            let original = crate::handoff::resolve_bundle_dir(&bundle)?;
            let metadata: media::CaptureMetadata =
                match std::fs::read(original.join("capture.json")) {
                    Ok(bytes) => serde_json::from_slice(&bytes)?,
                    Err(_) => {
                        let (_, b) = crate::handoff::load_bundle(&original)?;
                        ensure!(
                            b.source.as_deref() == Some("linux"),
                            "Linux reprocess requires a Linux capture"
                        );
                        media::CaptureMetadata {
                            target: b.target,
                            strokes: b.strokes,
                            events: b.events,
                            cursor: b.cursor,
                            windows: b.windows,
                            note: b.note,
                            ink_in_video: true,
                            ink_sessions: vec![],
                        }
                    }
                };
            ensure!(
                original.join("recording.mp4").is_file(),
                "The source recording is missing"
            );
            let path = new_bundle_path(Some(out))?;
            std::fs::copy(original.join("recording.mp4"), path.join("recording.mp4"))?;
            std::fs::write(
                path.join("capture.json"),
                serde_json::to_vec_pretty(&metadata)?,
            )?;
            media::build_bundle(&path, &metadata)?;
            println!("{}", path.display());
        }
        Command::Frames {
            bundle,
            from,
            to,
            fps,
        } => {
            let (dir, b) = crate::handoff::load_bundle(&bundle)?;
            let start = crate::model::parse_clock_ms(&from).context("Invalid --from time")?;
            let end = crate::model::parse_clock_ms(&to).context("Invalid --to time")?;
            ensure!(
                (1..=30).contains(&fps)
                    && end > start
                    && end <= b.duration_ms
                    && end - start <= 60_000,
                "Choose 1–30 fps and an interval of at most 60 seconds inside the recording"
            );
            let movie = dir.join(b.video.context("Bundle has no recording")?);
            let times: Vec<_> = (start..end).step_by((1000 / fps) as usize).collect();
            let frames = media::frames_at(&movie, &times, b.video_offset_ms, 1600)?;
            let out = dir.join(format!("extracted-{}", make_bundle_id()));
            std::fs::create_dir(&out)?;
            for (i, frame) in frames.iter().enumerate() {
                crate::render::save_png(frame, &out.join(format!("frame-{i:03}.png")))?;
            }
            println!("{}", out.display());
        }
    }
    Ok(())
}

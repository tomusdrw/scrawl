//! Hyprland geometry in logical desktop coordinates, matching slurp and wf-recorder.
use std::io::Read;
use std::os::fd::AsRawFd;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};

use crate::model::{CaptureMode, TargetFrame, TargetInfo};

const QUERY_TIMEOUT: Duration = Duration::from_secs(3);
const OUTPUT_LIMIT: usize = 1024 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Monitor {
    pub name: String,
    pub x: i32,
    pub y: i32,
    /// Logical dimensions, after output transform and fractional scaling.
    pub width: u32,
    pub height: u32,
    pub scale: f64,
}

#[derive(Debug)]
pub struct Selection {
    pub target: TargetInfo,
    pub monitor: Monitor,
    pub geometry: String,
}

struct Output {
    id: u32,
    focused: bool,
    monitor: Monitor,
}

#[derive(Clone, Copy, Deserialize)]
struct Point {
    x: f64,
    y: f64,
}

#[derive(Clone, Copy, PartialEq)]
struct Rect {
    x: i32,
    y: i32,
    width: u32,
    height: u32,
}

impl Rect {
    fn right(self) -> i64 {
        i64::from(self.x) + i64::from(self.width)
    }
    fn bottom(self) -> i64 {
        i64::from(self.y) + i64::from(self.height)
    }
    fn contains(self, p: Point) -> bool {
        p.x >= f64::from(self.x)
            && p.y >= f64::from(self.y)
            && p.x < self.right() as f64
            && p.y < self.bottom() as f64
    }
    fn intersection(self, other: Self) -> Option<Self> {
        let x = self.x.max(other.x);
        let y = self.y.max(other.y);
        let width = self.right().min(other.right()) - i64::from(x);
        let height = self.bottom().min(other.bottom()) - i64::from(y);
        (width > 0 && height > 0).then_some(Self {
            x,
            y,
            width: width as u32,
            height: height as u32,
        })
    }
}

impl Monitor {
    fn rect(&self) -> Rect {
        Rect {
            x: self.x,
            y: self.y,
            width: self.width,
            height: self.height,
        }
    }
}

fn parse_monitors(json: &str) -> Result<Vec<Output>> {
    #[derive(Deserialize)]
    struct Raw {
        id: u32,
        name: String,
        x: i32,
        y: i32,
        width: u32,
        height: u32,
        scale: f64,
        #[serde(default)]
        transform: u8,
        #[serde(default)]
        focused: bool,
        #[serde(default)]
        disabled: bool,
        #[serde(default, rename = "mirrorOf")]
        mirror_of: Option<String>,
    }
    let raw: Vec<Raw> = serde_json::from_str(json).context("invalid hyprctl monitors response")?;
    let mut outputs = Vec::new();
    for r in raw {
        if r.disabled
            || r.mirror_of
                .as_deref()
                .is_some_and(|s| s != "none" && !s.is_empty())
        {
            continue;
        }
        ensure!(
            !r.name.is_empty() && !r.name.chars().any(char::is_control),
            "invalid output name"
        );
        ensure!(
            r.scale.is_finite() && r.scale > 0.0 && r.transform <= 7,
            "invalid scale/transform for {}",
            r.name
        );
        // Hyprland's logicalBox uses (transformed pixel size / scale).round().
        let (w, h) = if r.transform % 2 == 1 {
            (r.height, r.width)
        } else {
            (r.width, r.height)
        };
        let width = (f64::from(w) / r.scale).round();
        let height = (f64::from(h) / r.scale).round();
        ensure!(
            width >= 1.0
                && height >= 1.0
                && width <= f64::from(i32::MAX)
                && height <= f64::from(i32::MAX),
            "invalid size for output {}",
            r.name
        );
        outputs.push(Output {
            id: r.id,
            focused: r.focused,
            monitor: Monitor {
                name: r.name,
                x: r.x,
                y: r.y,
                width: width as u32,
                height: height as u32,
                scale: r.scale,
            },
        });
    }
    ensure!(
        !outputs.is_empty(),
        "Hyprland has no enabled, independent outputs"
    );
    Ok(outputs)
}

fn selection(output: &Output, rect: Rect, mode: CaptureMode) -> Selection {
    Selection {
        monitor: output.monitor.clone(),
        geometry: format!("{},{} {}x{}", rect.x, rect.y, rect.width, rect.height),
        target: TargetInfo {
            mode,
            app: None,
            bundle_id: None,
            window_title: None,
            window_id: None,
            display_id: output.id,
            frame: TargetFrame {
                x: f64::from(rect.x),
                y: f64::from(rect.y),
                width: f64::from(rect.width),
                height: f64::from(rect.height),
                scale: output.monitor.scale,
            },
        },
    }
}

fn choose_screen(outputs: &[Output], pointer: Option<Point>) -> Result<Selection> {
    let output = pointer
        .and_then(|p| outputs.iter().find(|o| o.monitor.rect().contains(p)))
        .or_else(|| outputs.iter().find(|o| o.focused))
        .context("no display under the pointer or focused; focus a display and retry")?;
    Ok(selection(
        output,
        output.monitor.rect(),
        CaptureMode::Display,
    ))
}

fn parse_geometry(text: &str) -> Result<Rect> {
    let mut pieces = text.split_whitespace();
    let (x, y) = pieces
        .next()
        .and_then(|p| p.split_once(','))
        .context("area selection cancelled or malformed")?;
    let (w, h) = pieces
        .next()
        .and_then(|p| p.split_once('x'))
        .context("malformed area dimensions")?;
    ensure!(
        pieces.next().is_none(),
        "unexpected data after area geometry"
    );
    let rect = Rect {
        x: x.parse().context("invalid area x")?,
        y: y.parse().context("invalid area y")?,
        width: w.parse().context("invalid area width")?,
        height: h.parse().context("invalid area height")?,
    };
    ensure!(rect.width > 0 && rect.height > 0, "selected area is empty");
    Ok(rect)
}

fn choose_area(outputs: &[Output], geometry: &str) -> Result<Selection> {
    let rect = parse_geometry(geometry)?;
    let output = outputs.iter().find(|o| o.monitor.rect().intersection(rect) == Some(rect))
        .context("area is outside a display or crosses outputs; select an area entirely inside one output")?;
    Ok(selection(output, rect, CaptureMode::Region))
}

fn choose_window(outputs: &[Output], json: &str) -> Result<Selection> {
    #[derive(Deserialize)]
    struct Window {
        at: [i32; 2],
        size: [u32; 2],
        monitor: u32,
        #[serde(default)]
        class: String,
        #[serde(default)]
        title: String,
        #[serde(default)]
        hidden: bool,
        #[serde(default)]
        mapped: Option<bool>,
    }
    let w: Window =
        serde_json::from_str(json).context("no active window; focus a window and retry")?;
    ensure!(
        !w.hidden && w.mapped != Some(false),
        "the active window is not visible"
    );
    ensure!(
        w.size[0] > 0 && w.size[1] > 0,
        "the active window has no visible area"
    );
    let output = outputs
        .iter()
        .find(|o| o.id == w.monitor)
        .context("the active window's display is unavailable")?;
    let rect = Rect {
        x: w.at[0],
        y: w.at[1],
        width: w.size[0],
        height: w.size[1],
    };
    ensure!(
        !outputs
            .iter()
            .any(|o| o.id != output.id && o.monitor.rect().intersection(rect).is_some()),
        "window crosses outputs; move it inside one display or use --mode area"
    );
    let clipped = output
        .monitor
        .rect()
        .intersection(rect)
        .context("active window is outside its display")?;
    let mut result = selection(output, clipped, CaptureMode::Window);
    result.target.app = (!w.class.is_empty()).then_some(w.class.clone());
    result.target.bundle_id = (!w.class.is_empty()).then_some(w.class);
    result.target.window_title = (!w.title.is_empty()).then_some(w.title);
    // Hyprland's address is a pointer, not a stable u32 window identifier.
    Ok(result)
}

/// Select one output, a fixed active-window rectangle, or an interactive slurp area.
pub fn select(mode: &str) -> Result<Selection> {
    select_with_cancel(mode, &AtomicBool::new(false))
}

/// The service can cancel a pending area picker or desktop query during shutdown.
pub fn select_with_cancel(mode: &str, cancel: &AtomicBool) -> Result<Selection> {
    let query = |program: &str, args: &[&str], timeout: Duration| {
        command_output_with_cancel(program, args, timeout, Some(cancel))
    };
    ensure!(
        matches!(mode, "screen" | "window" | "area"),
        "unknown capture mode {mode}; use screen, window or area"
    );
    let outputs = parse_monitors(
        &query("hyprctl", &["-j", "monitors"], QUERY_TIMEOUT)
            .context("Scrawl requires a running Hyprland session")?,
    )?;
    let selected = match mode {
        "screen" => {
            let pointer = query("hyprctl", &["-j", "cursorpos"], QUERY_TIMEOUT)
                .ok()
                .and_then(|json| serde_json::from_str(&json).ok());
            choose_screen(&outputs, pointer)
        }
        "window" => choose_window(
            &outputs,
            &query("hyprctl", &["-j", "activewindow"], QUERY_TIMEOUT)?,
        ),
        "area" => {
            let geometry = query("slurp", &["-f", "%x,%y %wx%h"], Duration::from_secs(120))
                .context("area selection cancelled or failed (install slurp for --mode area)")?;
            // A monitor may have moved/disconnected while the picker was open.
            let current = parse_monitors(&query("hyprctl", &["-j", "monitors"], QUERY_TIMEOUT)?)?;
            choose_area(&current, &geometry)
        }
        _ => unreachable!(),
    };
    ensure!(!cancel.load(Ordering::Relaxed), "Selection cancelled");
    selected
}

fn normalize_cursor(frame: &TargetFrame, pointer: Point) -> Option<[f32; 2]> {
    if !frame.width.is_finite()
        || !frame.height.is_finite()
        || frame.width <= 0.0
        || frame.height <= 0.0
    {
        return None;
    }
    let x = (pointer.x - frame.x) / frame.width;
    let y = (pointer.y - frame.y) / frame.height;
    ((0.0..1.0).contains(&x) && (0.0..1.0).contains(&y)).then_some([x as f32, y as f32])
}

pub fn sample_cursor(target: &TargetInfo) -> Option<[f32; 2]> {
    let json = command_output("hyprctl", &["-j", "cursorpos"], Duration::from_millis(500)).ok()?;
    normalize_cursor(&target.frame, serde_json::from_str(&json).ok()?)
}

/// Bound process lifetime and combined output, including when a child fills a pipe.
pub(crate) fn command_output(program: &str, args: &[&str], timeout: Duration) -> Result<String> {
    command_output_with_cancel(program, args, timeout, None)
}

fn command_output_with_cancel(
    program: &str,
    args: &[&str],
    timeout: Duration,
    cancel: Option<&AtomicBool>,
) -> Result<String> {
    let cancelled = || cancel.is_some_and(|flag| flag.load(Ordering::Relaxed));
    ensure!(!cancelled(), "Selection cancelled");
    struct Guard(Child);
    impl Drop for Guard {
        fn drop(&mut self) {
            // Every command has its own process group; also clean up inherited-pipe children.
            unsafe {
                libc::kill(-(self.0.id() as i32), libc::SIGKILL);
            }
            let _ = self.0.wait();
        }
    }
    fn nonblocking(pipe: &impl AsRawFd) -> std::io::Result<()> {
        let fd = pipe.as_raw_fd();
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
        if flags == -1 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } == -1
        {
            return Err(std::io::Error::last_os_error());
        }
        Ok(())
    }
    fn drain(pipe: &mut impl Read, output: &mut Vec<u8>) -> Result<()> {
        let mut buf = [0; 8192];
        loop {
            match pipe.read(&mut buf) {
                Ok(0) => return Ok(()),
                Ok(n) => {
                    ensure!(
                        output.len() + n <= OUTPUT_LIMIT,
                        "command output exceeds 1 MiB"
                    );
                    output.extend_from_slice(&buf[..n]);
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => return Ok(()),
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e.into()),
            }
        }
    }
    let mut child = Guard(
        Command::new(program)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0)
            .spawn()
            .with_context(|| {
                format!("could not run {program}; install it and ensure it is on PATH")
            })?,
    );
    let mut stdout = child.0.stdout.take().context("missing command stdout")?;
    let mut stderr = child.0.stderr.take().context("missing command stderr")?;
    nonblocking(&stdout)?;
    nonblocking(&stderr)?;
    let start = Instant::now();
    let mut out = Vec::new();
    let mut err = Vec::new();
    loop {
        ensure!(!cancelled(), "Selection cancelled");
        drain(&mut stdout, &mut out)?;
        drain(&mut stderr, &mut err)?;
        ensure!(
            out.len() + err.len() <= OUTPUT_LIMIT,
            "{program} output exceeds 1 MiB"
        );
        if let Some(status) = child.0.try_wait()? {
            drain(&mut stdout, &mut out)?;
            drain(&mut stderr, &mut err)?;
            if !status.success() {
                let detail = String::from_utf8_lossy(&err);
                bail!(
                    "{program} exited with {status}: {}",
                    detail.trim().chars().take(500).collect::<String>()
                );
            }
            return String::from_utf8(out)
                .with_context(|| format!("{program} returned non-UTF-8 output"));
        }
        ensure!(
            start.elapsed() < timeout,
            "{program} timed out after {:.1}s",
            timeout.as_secs_f64()
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn executable(name: &str) -> Option<PathBuf> {
    let valid = |path: &Path| {
        path.metadata()
            .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
    };
    if name.contains('/') {
        return valid(Path::new(name)).then(|| PathBuf::from(name));
    }
    std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
        .map(|p| p.join(name))
        .find(|p| valid(p))
}

pub fn doctor() -> Result<()> {
    let mut missing = Vec::new();
    println!("Linux backend: Hyprland / Wayland");
    for (program, package) in [
        ("hyprctl", "hyprland"),
        ("wf-recorder", "wf-recorder"),
        ("ffmpeg", "ffmpeg"),
        ("ffprobe", "ffmpeg"),
        ("wl-copy", "wl-clipboard"),
        ("slurp", "slurp"),
        ("pactl", "libpulse"),
        ("python3", "python"),
    ] {
        match executable(program) {
            Some(path) => println!("  OK       {program}: {}", path.display()),
            None => {
                println!("  MISSING  {program}: install {package}");
                missing.push(program);
            }
        }
    }
    let gtk = command_output(
        "python3",
        &[
            "-c",
            "import gi; gi.require_version('Gtk', '4.0'); gi.require_version('Gtk4LayerShell', '1.0'); from gi.repository import Gtk, Gtk4LayerShell; import cairo; print('GTK4 + layer shell + Cairo')",
        ],
        QUERY_TIMEOUT,
    );
    match gtk {
        Ok(s) => println!("  OK       {}", s.trim()),
        Err(_) => {
            println!("  MISSING  GTK helper: install python-gobject python-cairo gtk4-layer-shell");
            missing.push("Python GTK4/layer-shell/Cairo");
        }
    }
    match command_output("hyprctl", &["-j", "monitors"], QUERY_TIMEOUT)
        .and_then(|s| parse_monitors(&s))
    {
        Ok(outputs) => println!(
            "  OK       Hyprland connection: {} output(s)",
            outputs.len()
        ),
        Err(e) => {
            println!("  MISSING  Hyprland session: {e}");
            missing.push("Hyprland session");
        }
    }
    if command_output("pactl", &["info"], QUERY_TIMEOUT).is_err() {
        println!(
            "  OPTIONAL audio server unavailable: use --no-audio or start PipeWire Pulse/PulseAudio"
        );
    }
    let config: serde_json::Value = std::fs::File::open(crate::handoff::default_config_path())
        .ok()
        .and_then(|file| serde_json::from_reader(file.take(OUTPUT_LIMIT as u64)).ok())
        .unwrap_or_default();
    let whisper = std::env::var("SCRAWL_WHISPER_BIN")
        .ok()
        .or_else(|| config["linux"]["whisperBin"].as_str().map(str::to_owned))
        .unwrap_or_else(|| "whisper-cli".into());
    let model = std::env::var("SCRAWL_WHISPER_MODEL")
        .ok()
        .or_else(|| config["linux"]["whisperModel"].as_str().map(str::to_owned));
    match (
        executable(&whisper),
        model.as_deref().filter(|p| Path::new(p).is_file()),
    ) {
        (Some(bin), Some(model)) => println!(
            "  OK       optional transcription: {} (model {model})",
            bin.display()
        ),
        _ => println!(
            "  OPTIONAL transcription unavailable: set SCRAWL_WHISPER_BIN and SCRAWL_WHISPER_MODEL; voice is still recorded"
        ),
    }
    ensure!(
        missing.is_empty(),
        "missing Linux dependencies: {}; see docs/linux.md",
        missing.join(", ")
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    // Physical modes; LEFT is rotated 90 degrees and fractional-scaled.
    const MONITORS: &str = r#"[
      {"id": 7, "name":"LEFT", "x":-960, "y":-100, "width":2400,
       "height":1200, "scale":1.25, "transform":1, "focused":false},
      {"id": 2, "name":"MAIN", "x":0, "y":0, "width":5120,
       "height":2160, "scale":1.6, "transform":0, "focused":true}
    ]"#;

    #[test]
    fn command_reads_output_and_reports_a_failed_exit() {
        assert_eq!(
            command_output("sh", &["-c", "printf hello"], Duration::from_secs(2)).unwrap(),
            "hello"
        );
        let error = command_output(
            "sh",
            &["-c", "printf failure >&2; exit 7"],
            Duration::from_secs(2),
        )
        .unwrap_err();
        assert!(error.to_string().contains("failure"));
    }

    #[test]
    fn cancelling_selection_interrupts_a_waiting_child_before_its_deadline() {
        let cancelled = AtomicBool::new(false);
        let start = Instant::now();
        std::thread::scope(|scope| {
            scope.spawn(|| {
                std::thread::sleep(Duration::from_millis(50));
                cancelled.store(true, Ordering::Relaxed);
            });
            let error = command_output_with_cancel(
                "sh",
                &["-c", "sleep 10"],
                Duration::from_secs(2),
                Some(&cancelled),
            )
            .unwrap_err();
            assert!(error.to_string().contains("Selection cancelled"), "{error}");
            assert!(start.elapsed() < Duration::from_secs(1));
        });
    }

    #[test]
    fn command_timeout_stops_the_process_group() {
        let start = Instant::now();
        let error =
            command_output("sh", &["-c", "sleep 10"], Duration::from_millis(50)).unwrap_err();
        assert!(error.to_string().contains("timed out"));
        assert!(start.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn command_bounds_output_without_deadlocking_full_pipes() {
        let start = Instant::now();
        let error = command_output(
            "sh",
            &["-c", "head -c 2097152 /dev/zero"],
            Duration::from_secs(2),
        )
        .unwrap_err();
        assert!(error.to_string().contains("exceeds"));
        assert!(start.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn screen_uses_rotated_logical_size_and_pointer_over_focus() {
        let outputs = parse_monitors(MONITORS).unwrap();
        let selection = choose_screen(
            &outputs,
            Some(Point {
                x: -500.0,
                y: 100.0,
            }),
        )
        .unwrap();
        assert_eq!(selection.geometry, "-960,-100 960x1920");
        assert_eq!(selection.target.display_id, 7);
        assert_eq!(selection.target.frame.scale, 1.25);
        assert_eq!(selection.target.mode, CaptureMode::Display);
    }

    #[test]
    fn pointer_at_shared_edge_belongs_to_right_output() {
        let outputs = parse_monitors(MONITORS).unwrap();
        let selection = choose_screen(&outputs, Some(Point { x: 0.0, y: 100.0 })).unwrap();
        assert_eq!(selection.geometry, "0,0 3200x1350");
        assert_eq!(selection.target.display_id, 2);
    }

    #[test]
    fn missing_or_outside_pointer_uses_focused_output() {
        let outputs = parse_monitors(MONITORS).unwrap();
        for pointer in [
            None,
            Some(Point {
                x: -2000.0,
                y: -2000.0,
            }),
        ] {
            assert_eq!(
                choose_screen(&outputs, pointer).unwrap().target.display_id,
                2
            );
        }
    }

    #[test]
    fn area_keeps_negative_origin_without_scaling_twice() {
        let selection =
            choose_area(&parse_monitors(MONITORS).unwrap(), "-900,-50 800x600\n").unwrap();
        assert_eq!(selection.geometry, "-900,-50 800x600");
        assert_eq!(selection.target.frame.width, 800.0);
        assert_eq!(selection.target.frame.scale, 1.25);
        assert_eq!(selection.target.mode, CaptureMode::Region);
    }

    #[test]
    fn area_rejects_cancel_malformed_outside_and_cross_output_rectangles() {
        let outputs = parse_monitors(MONITORS).unwrap();
        for geometry in [
            "",
            "0,0",
            "0,0 10x10 extra",
            "0,0 0x20",
            "0,0 -3x20",
            "0,0 2.5x20",
            "-100,100 200x200",
            "3100,0 200x20",
            "0,0 4294967295x20",
        ] {
            assert!(
                choose_area(&outputs, geometry).is_err(),
                "accepted {geometry:?}"
            );
        }
    }

    #[test]
    fn offscreen_window_clips_to_its_monitor_and_preserves_identity() {
        let selection = choose_window(
            &parse_monitors(MONITORS).unwrap(),
            r#"{
          "address":"0x1234", "at":[100,-30], "size":[800,600], "monitor":2,
          "mapped":true, "hidden":false, "class":"org.example.Editor", "title":"notes"
        }"#,
        )
        .unwrap();
        assert_eq!(selection.geometry, "100,0 800x570");
        assert_eq!(selection.target.mode, CaptureMode::Window);
        assert_eq!(selection.target.app.as_deref(), Some("org.example.Editor"));
        assert_eq!(selection.target.window_title.as_deref(), Some("notes"));
    }

    #[test]
    fn window_rejects_no_active_window_cross_output_and_invisible_rectangles() {
        let outputs = parse_monitors(MONITORS).unwrap();
        for json in [
            "{}",
            r#"{"at":[0,0],"size":[0,100],"monitor":2}"#,
            r#"{"at":[-100,100],"size":[200,200],"monitor":2}"#,
            r#"{"at":[4000,0],"size":[200,200],"monitor":2}"#,
            r#"{"at":[0,0],"size":[200,200],"monitor":2,"hidden":true}"#,
        ] {
            assert!(choose_window(&outputs, json).is_err(), "accepted {json}");
        }
    }

    #[test]
    fn invalid_or_empty_monitor_data_fails_without_panicking() {
        for json in [
            "[]",
            "{}",
            "not json",
            r#"[{"id":1,"name":"bad","x":0,"y":0,"width":100,"height":100,"scale":0}]"#,
            r#"[{"id":1,"name":"bad","x":0,"y":0,"width":0,"height":100,"scale":1}]"#,
        ] {
            assert!(parse_monitors(json).is_err(), "accepted {json}");
        }
    }

    #[test]
    fn mirrored_transforms_swap_dimensions_and_logical_size_rounds() {
        let outputs = parse_monitors(
            r#"[{"id":1,"name":"ROTATED","x":0,"y":0,
            "width":1920,"height":1080,"scale":1.3,"transform":5,"focused":true}]"#,
        )
        .unwrap();
        assert_eq!(
            choose_screen(&outputs, None).unwrap().geometry,
            "0,0 831x1477"
        );
    }

    #[test]
    fn cursor_normalizes_logical_coordinates_and_omits_outside_samples() {
        let frame = TargetFrame {
            x: -900.0,
            y: -50.0,
            width: 800.0,
            height: 600.0,
            scale: 1.25,
        };
        assert_eq!(
            normalize_cursor(
                &frame,
                Point {
                    x: -500.0,
                    y: 250.0
                }
            ),
            Some([0.5, 0.5])
        );
        assert_eq!(
            normalize_cursor(
                &frame,
                Point {
                    x: -901.0,
                    y: 250.0
                }
            ),
            None
        );
        assert_eq!(
            normalize_cursor(
                &frame,
                Point {
                    x: -100.0,
                    y: 250.0
                }
            ),
            None
        );
        assert_eq!(
            normalize_cursor(
                &frame,
                Point {
                    x: f64::NAN,
                    y: 250.0
                }
            ),
            None
        );
    }
}

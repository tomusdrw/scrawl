//! A single owner for recorder children; clients never signal a PID from a file.
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail, ensure};
use serde::Deserialize;
use serde_json::{Value, json};

use super::{desktop, media};
use crate::model::Stroke;
use crate::windows::CursorSample;

const MAX_REQUEST: u64 = 1024 * 1024;
const MAX_RECORDING: Duration = Duration::from_secs(1800);
static SHUTDOWN: AtomicBool = AtomicBool::new(false);

extern "C" fn shutdown_signal(_: libc::c_int) {
    SHUTDOWN.store(true, Ordering::Relaxed);
}

pub fn runtime_dir() -> Result<PathBuf> {
    let path = std::env::var_os("SCRAWL_RUNTIME_DIR")
        .map(PathBuf::from)
        .or_else(|| dirs::runtime_dir().map(|p| p.join("scrawl")))
        .context("XDG_RUNTIME_DIR is not set; run Scrawl inside your desktop session")?;
    private_dir(&path)?;
    Ok(path)
}

fn private_dir(path: &Path) -> Result<()> {
    match std::fs::DirBuilder::new().mode(0o700).create(path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error.into()),
    }
    let metadata = std::fs::symlink_metadata(path)?;
    ensure!(
        metadata.is_dir()
            && !metadata.file_type().is_symlink()
            && metadata.uid() == unsafe { libc::geteuid() },
        "Scrawl runtime directory is not owned by this user: {}",
        path.display()
    );
    ensure!(
        metadata.mode() & 0o077 == 0,
        "Scrawl runtime directory must be private (mode 0700): {}",
        path.display()
    );
    Ok(())
}

pub fn socket_path() -> Result<PathBuf> {
    Ok(runtime_dir()?.join("control.sock"))
}

fn log_file(name: &str) -> Result<File> {
    Ok(OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(runtime_dir()?.join(name))?)
}

fn detached(cmd: &mut Command, log: File) -> Result<Child> {
    cmd.stdin(Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log);
    unsafe {
        cmd.pre_exec(|| {
            if libc::setsid() < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    Ok(cmd.spawn()?)
}

pub fn ensure_daemon() -> Result<()> {
    let path = socket_path()?;
    if UnixStream::connect(&path).is_ok() {
        return Ok(());
    }
    let mut child = detached(
        Command::new(std::env::current_exe()?).arg("daemon"),
        log_file("daemon.log")?,
    )?;
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(5) {
        if UnixStream::connect(&path).is_ok() {
            // Reap when this client stays alive; an exiting client is reparented normally.
            std::thread::spawn(move || {
                let _ = child.wait();
            });
            return Ok(());
        }
        if let Some(status) = child.try_wait()? {
            // Another simultaneous launcher may have won the lock.
            if UnixStream::connect(&path).is_ok() {
                return Ok(());
            }
            bail!(
                "Scrawl service exited ({status}); see {}",
                runtime_dir()?.join("daemon.log").display()
            );
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let _ = child.kill();
    let _ = child.wait();
    bail!(
        "Scrawl service did not start; see {}",
        runtime_dir()?.join("daemon.log").display()
    )
}

pub fn show_ui() -> Result<()> {
    let mut child = detached(
        Command::new(std::env::current_exe()?).arg("ui"),
        log_file("ui.log")?,
    )?;
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

pub fn send(request: &Value, auto_start: bool) -> Result<Value> {
    if auto_start {
        ensure_daemon()?;
    }
    let mut stream = UnixStream::connect(socket_path()?)
        .context("Scrawl is not running; use scrawl app or scrawl start")?;
    stream.set_read_timeout(Some(Duration::from_secs(15)))?;
    stream.set_write_timeout(Some(Duration::from_secs(3)))?;
    serde_json::to_writer(&mut stream, request)?;
    stream.write_all(b"\n")?;
    let mut line = String::new();
    BufReader::new(stream)
        .take(MAX_REQUEST + 1)
        .read_line(&mut line)?;
    ensure!(
        line.len() as u64 <= MAX_REQUEST,
        "Service response is too large"
    );
    let reply: Value = serde_json::from_str(&line)?;
    ensure!(
        reply["ok"] == true,
        "{}",
        reply["error"].as_str().unwrap_or("Scrawl request failed")
    );
    Ok(reply)
}

pub fn status_or_idle() -> Result<Value> {
    match UnixStream::connect(socket_path()?) {
        Ok(_) => send(&json!({"command":"status"}), false),
        Err(e)
            if matches!(
                e.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
            ) =>
        {
            Ok(json!({"ok":true,"state":"idle","elapsedMs":0}))
        }
        Err(e) => Err(e.into()),
    }
}

/// Owns a running process until it is reaped, including on errors and unwinding.
struct Recorder {
    child: Child,
}
impl Recorder {
    fn stop(&mut self) -> Result<()> {
        if let Some(status) = self.child.try_wait()? {
            ensure!(status.success(), "Recorder exited unexpectedly: {status}");
            return Ok(());
        }
        unsafe {
            libc::kill(self.child.id() as i32, libc::SIGINT);
        }
        let start = Instant::now();
        while start.elapsed() < Duration::from_secs(8) {
            if let Some(status) = self.child.try_wait()? {
                ensure!(
                    status.success(),
                    "Recorder could not finalize the movie: {status}"
                );
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(30));
        }
        self.child.kill()?;
        self.child.wait()?;
        bail!("Recorder did not stop within 8 seconds; raw files were preserved")
    }
}
impl Drop for Recorder {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Request {
    command: String,
    mode: Option<String>,
    #[serde(default)]
    no_audio: bool,
    out: Option<PathBuf>,
    note: Option<String>,
    enabled: Option<bool>,
    stroke: Option<Stroke>,
    client: Option<String>,
    #[serde(default)]
    flushed: bool,
}

struct Active {
    recorder: Recorder,
    metadata: media::CaptureMetadata,
    monitor: desktop::Monitor,
    dir: PathBuf,
    started: Instant,
    ready: bool,
    drawing: bool,
    last_cursor: Instant,
}

struct Prepared {
    request: Request,
    selection: desktop::Selection,
    source: Option<String>,
}

impl Prepared {
    fn select(request: Request, cancel: &AtomicBool) -> Result<Self> {
        let selection =
            desktop::select_with_cancel(request.mode.as_deref().unwrap_or("screen"), cancel)?;
        // Ask PulseAudio/PipeWire for its input source explicitly: wf-recorder's default
        // is the output monitor on some installations, which would omit narration.
        let source = if request.no_audio {
            None
        } else {
            let source =
                desktop::command_output("pactl", &["get-default-source"], Duration::from_secs(5))?;
            ensure!(
                !source.trim().is_empty(),
                "No default microphone; choose an input or use --no-audio"
            );
            Some(source.trim().to_owned())
        };
        Ok(Self {
            request,
            selection,
            source,
        })
    }
}

impl Active {
    // Spawn on the daemon's main thread: Linux PDEATHSIG observes the creating
    // thread, so spawning in the short-lived selection worker would stop recording
    // as soon as that worker returned.
    fn begin(prepared: Prepared, cancel: &AtomicBool) -> Result<Self> {
        let Prepared {
            request,
            selection,
            source,
        } = prepared;
        ensure!(!cancel.load(Ordering::Relaxed), "Selection cancelled");
        let dir = super::new_bundle_path(request.out)?;
        let metadata = media::CaptureMetadata {
            target: selection.target,
            strokes: vec![],
            events: vec![],
            cursor: vec![],
            windows: vec![],
            note: request.note,
            ink_in_video: true,
            ink_sessions: vec![],
        };
        write_metadata(&dir, &metadata)?;
        let log = OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(dir.join("recorder.log"))?;
        let mut cmd = Command::new("wf-recorder");
        cmd.args(["--file"]).arg(dir.join("recording.mp4"))
            .args(["--output",&selection.monitor.name,"--geometry",&selection.geometry,"--framerate","30",
                   "--codec","libx264","--pixel-format","yuv420p","--codec-param","preset=ultrafast",
                   "--codec-param","crf=20","--filter",
                   "scale=w='min(iw,2560)':h='min(ih,2560)':force_original_aspect_ratio=decrease:force_divisible_by=2"])
            .stdin(Stdio::null()).stdout(log.try_clone()?).stderr(log);
        if let Some(source) = source {
            cmd.arg(format!("--audio={source}"));
        }
        let parent = std::process::id() as libc::pid_t;
        unsafe {
            cmd.pre_exec(move || {
                if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGINT) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                if libc::getppid() != parent {
                    return Err(std::io::Error::other("Recorder owner exited"));
                }
                Ok(())
            });
        }
        let started = Instant::now();
        let child = cmd
            .spawn()
            .context("Could not start wf-recorder; install wf-recorder and run scrawl doctor")?;
        let recorder = Recorder { child };
        Ok(Self {
            recorder,
            metadata,
            monitor: selection.monitor,
            dir,
            started,
            ready: false,
            drawing: false,
            last_cursor: Instant::now(),
        })
    }
    fn elapsed(&self) -> u64 {
        if self.ready {
            self.started.elapsed().as_millis() as u64
        } else {
            0
        }
    }
    fn tick(&mut self) -> Result<()> {
        if let Some(status) = self.recorder.child.try_wait()? {
            let log = std::fs::read_to_string(self.dir.join("recorder.log")).unwrap_or_default();
            bail!(
                "wf-recorder exited unexpectedly ({status}): {}\nSee {}",
                log.chars()
                    .rev()
                    .take(1600)
                    .collect::<String>()
                    .chars()
                    .rev()
                    .collect::<String>(),
                self.dir.join("recorder.log").display(),
            );
        }
        if !self.ready {
            // wf-recorder opens/writes the MP4 header when its first screen frame
            // initializes the writer. Start telemetry here, after recorder setup,
            // with one service tick (~30 ms) of observation uncertainty.
            if std::fs::metadata(self.dir.join("recording.mp4")).is_ok_and(|m| m.len() > 0) {
                self.started = Instant::now();
                self.ready = true;
            } else {
                ensure!(
                    self.started.elapsed() < Duration::from_secs(8),
                    "No screen frame arrived within 8 seconds. Unlock the desktop and check the capture protocol; recording files: {}",
                    self.dir.display()
                );
                return Ok(());
            }
        }
        if self.last_cursor.elapsed() >= Duration::from_millis(200) {
            self.last_cursor = Instant::now();
            if let Some([x, y]) = desktop::sample_cursor(&self.metadata.target) {
                let changed = self
                    .metadata
                    .cursor
                    .last()
                    .is_none_or(|p| (p.1 - x).abs() + (p.2 - y).abs() > 0.001);
                if changed {
                    self.metadata
                        .cursor
                        .push(CursorSample(self.elapsed(), x, y));
                }
            }
        }
        Ok(())
    }
    fn stroke(&mut self, stroke: Stroke) -> Result<()> {
        validate_stroke(&stroke, self.elapsed())?;
        ensure!(
            self.metadata.strokes.len() < 512,
            "This recording has reached the 512-stroke limit"
        );
        ensure!(
            self.metadata
                .strokes
                .iter()
                .map(|s| s.pts.len())
                .sum::<usize>()
                + stroke.pts.len()
                <= 60_000,
            "This recording has reached its drawing point limit"
        );
        self.metadata.strokes.push(stroke);
        write_metadata(&self.dir, &self.metadata)
    }
    fn set_drawing(&mut self, enabled: bool) -> Result<()> {
        ensure!(
            self.ready,
            "Wait for the recorder's first frame before drawing"
        );
        if enabled != self.drawing {
            let t = self.elapsed();
            if enabled {
                self.metadata.ink_sessions.push([t, u64::MAX]);
            } else if let Some(last) = self.metadata.ink_sessions.last_mut() {
                last[1] = t;
            }
            self.drawing = enabled;
            write_metadata(&self.dir, &self.metadata)?;
        }
        Ok(())
    }
}

fn validate_stroke(stroke: &Stroke, elapsed: u64) -> Result<()> {
    ensure!(
        !stroke.pts.is_empty() && stroke.pts.len() % 3 == 0 && stroke.pts.len() <= 60_000,
        "Invalid drawing points"
    );
    ensure!(
        stroke.t1 >= stroke.t0 && stroke.t1 <= elapsed + 2000 && stroke.fw > 0 && stroke.fh > 0,
        "Invalid stroke timing or dimensions"
    );
    let mut last = -1;
    for p in stroke.pts.chunks_exact(3) {
        ensure!(
            (0..=10_000).contains(&p[0])
                && (0..=10_000).contains(&p[1])
                && p[2] >= last
                && p[2] >= 0,
            "Invalid stroke coordinates or time order"
        );
        ensure!(
            p[2] as u64 <= stroke.t1 - stroke.t0 + 100,
            "Stroke points extend beyond the stroke"
        );
        last = p[2];
    }
    Ok(())
}

fn write_metadata(dir: &Path, metadata: &media::CaptureMetadata) -> Result<()> {
    let temp = dir.join(".capture.json.tmp");
    std::fs::write(&temp, serde_json::to_vec_pretty(metadata)?)?;
    std::fs::rename(temp, dir.join("capture.json"))?;
    Ok(())
}

enum Worker {
    Starting(Receiver<Result<Prepared>>, Arc<AtomicBool>),
    Finishing(Receiver<Result<PathBuf>>),
}
#[derive(Clone, Copy)]
enum FlushAction {
    Finish,
    DrawOff,
}
#[derive(Default)]
struct Service {
    active: Option<Active>,
    worker: Option<Worker>,
    bundle: Option<PathBuf>,
    error: Option<String>,
    ui_seen: Option<Instant>,
    pending_flush: Option<(FlushAction, Instant)>,
}

impl Service {
    fn status(&self) -> Value {
        let state = match (&self.active, &self.worker, &self.error) {
            (Some(a), _, _) => {
                if a.ready {
                    "recording"
                } else {
                    "starting"
                }
            }
            (_, Some(Worker::Starting(..)), _) => "starting",
            (_, Some(Worker::Finishing(_)), _) => "processing",
            (_, _, Some(_)) => "error",
            _ => "idle",
        };
        json!({"ok":true,"state":state,"elapsedMs":self.active.as_ref().map_or(0,Active::elapsed),
            "target":self.active.as_ref().map(|a|&a.metadata.target),"monitor":self.active.as_ref().map(|a|&a.monitor),
            "drawing":self.active.as_ref().is_some_and(|a|a.drawing),"strokes":self.active.as_ref().map(|a|&a.metadata.strokes),
            "sessionId":self.active.as_ref().map(|a|a.dir.file_name().unwrap_or_default().to_string_lossy()),
            "bundle":self.bundle,"error":self.error,"flushRequested":self.pending_flush.is_some()})
    }
    fn request(&mut self, request: Request) -> Result<()> {
        match request.command.as_str() {
            "status" => {
                if request.client.as_deref() == Some("ui") {
                    self.ui_seen = Some(Instant::now());
                }
            }
            "start" | "toggle" if self.active.is_none() => {
                ensure!(
                    self.worker.is_none(),
                    "Scrawl is already selecting or processing a recording"
                );
                self.error = None;
                self.bundle = None;
                let cancel = Arc::new(AtomicBool::new(false));
                let worker_cancel = Arc::clone(&cancel);
                let (tx, rx) = mpsc::channel();
                self.worker = Some(Worker::Starting(rx, cancel));
                std::thread::spawn(move || {
                    let _ = tx.send(Prepared::select(request, &worker_cancel));
                });
            }
            "start" => bail!("A recording is already active"),
            "stop" | "toggle" => self.flush_or_apply(FlushAction::Finish, request.flushed)?,
            "cancel" => {
                if let Some(Worker::Starting(_, cancel)) = &self.worker {
                    cancel.store(true, Ordering::Relaxed);
                    return Ok(());
                }
                ensure!(self.worker.is_none(), "Wait for processing to complete");
                let mut active = self.active.take().context("No recording is active")?;
                self.pending_flush = None;
                let _ = active.recorder.stop();
                let dir = active.dir.clone();
                drop(active);
                std::fs::remove_dir_all(dir)?;
                self.error = None;
                self.bundle = None;
            }
            "draw" => {
                let a = self
                    .active
                    .as_mut()
                    .context("Start a recording before drawing")?;
                let enabled = request.enabled.unwrap_or(!a.drawing);
                if enabled {
                    a.set_drawing(true)?;
                } else {
                    self.flush_or_apply(FlushAction::DrawOff, request.flushed)?;
                }
            }
            "drawing-flushed" => {
                if let Some((action, _)) = self.pending_flush.take() {
                    self.apply_flush(action)?;
                }
            }
            "undo" => {
                let a = self.active.as_mut().context("No recording is active")?;
                a.metadata.strokes.pop();
                write_metadata(&a.dir, &a.metadata)?;
            }
            "stroke" => {
                let a = self.active.as_mut().context("No recording is active")?;
                a.stroke(request.stroke.context("Missing stroke")?)?;
            }
            "shutdown" => {
                if let Some(Worker::Starting(_, cancel)) = &self.worker {
                    cancel.store(true, Ordering::Relaxed);
                }
                ensure!(
                    !matches!(self.worker, Some(Worker::Finishing(_))),
                    "Wait for processing before quitting"
                );
                if let Some(mut a) = self.active.take() {
                    let result = a.recorder.stop();
                    write_metadata(&a.dir, &a.metadata)?;
                    result?;
                    self.bundle = Some(a.dir.clone());
                }
                SHUTDOWN.store(true, Ordering::Relaxed);
            }
            _ => bail!("Unknown Scrawl command"),
        }
        Ok(())
    }
    fn finish(&mut self) -> Result<()> {
        let mut active = self.active.take().context("No recording is active")?;
        self.pending_flush = None;
        if active.ready {
            active.set_drawing(false)?;
        }
        self.bundle = Some(active.dir.clone());
        let (tx, rx) = mpsc::channel();
        self.worker = Some(Worker::Finishing(rx));
        std::thread::spawn(move || {
            let result = (|| -> Result<PathBuf> {
                write_metadata(&active.dir, &active.metadata)?;
                active.recorder.stop()?;
                media::build_bundle(&active.dir, &active.metadata)?;
                if let Err(error) = crate::handoff::copy_bundle_to_clipboard(&active.dir) {
                    eprintln!("Saved capture, but clipboard failed: {error:#}");
                }
                Ok(active.dir.clone())
            })();
            let _ = tx.send(result);
        });
        Ok(())
    }
    fn flush_or_apply(&mut self, action: FlushAction, flushed: bool) -> Result<()> {
        let active = self.active.as_ref().context("No recording is active")?;
        if !flushed
            && active.drawing
            && self
                .ui_seen
                .is_some_and(|t| t.elapsed() < Duration::from_secs(1))
        {
            self.pending_flush.get_or_insert((action, Instant::now()));
            if matches!(action, FlushAction::Finish) {
                self.pending_flush.as_mut().unwrap().0 = action;
            }
            Ok(())
        } else {
            self.apply_flush(action)
        }
    }
    fn apply_flush(&mut self, action: FlushAction) -> Result<()> {
        // A local Draw-off also acknowledges flushed ink. It must complete a
        // pending Stop, even if it arrives before the explicit acknowledgement.
        let action = match self.pending_flush.take() {
            Some((FlushAction::Finish, _)) => FlushAction::Finish,
            _ => action,
        };
        match action {
            FlushAction::Finish => self.finish(),
            FlushAction::DrawOff => self
                .active
                .as_mut()
                .context("No recording is active")?
                .set_drawing(false),
        }
    }
    fn tick(&mut self) {
        let finished = match &self.worker {
            Some(Worker::Starting(rx, cancel)) => match rx.try_recv() {
                Ok(Ok(prepared)) => {
                    if !cancel.load(Ordering::Relaxed) {
                        match Active::begin(prepared, cancel) {
                            Ok(active) => self.active = Some(active),
                            Err(e) => self.error = Some(format!("{e:#}")),
                        }
                    }
                    true
                }
                Ok(Err(e)) => {
                    if !cancel.load(Ordering::Relaxed) {
                        self.error = Some(format!("{e:#}"));
                    }
                    true
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.error = Some("Recording worker stopped unexpectedly".into());
                    true
                }
                _ => false,
            },
            Some(Worker::Finishing(rx)) => match rx.try_recv() {
                Ok(Ok(path)) => {
                    self.bundle = Some(path);
                    self.error = None;
                    true
                }
                Ok(Err(e)) => {
                    self.error = Some(format!(
                        "{e:#}. Recording preserved; use scrawl reprocess with a new --out path."
                    ));
                    true
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.error =
                        Some("Processing worker stopped unexpectedly; recording preserved".into());
                    true
                }
                _ => false,
            },
            None => false,
        };
        if finished {
            self.worker = None;
        }
        if let Some(a) = self.active.as_mut() {
            if let Err(e) = a.tick() {
                let _ = write_metadata(&a.dir, &a.metadata);
                self.bundle = Some(a.dir.clone());
                self.error = Some(format!("{e:#}"));
                self.active = None;
                self.pending_flush = None;
            } else if a.started.elapsed() >= MAX_RECORDING {
                let _ = self.finish();
            }
        }
        if let Some((action, at)) = self.pending_flush {
            if at.elapsed() >= Duration::from_secs(1) {
                if let Err(e) = self.apply_flush(action) {
                    self.error = Some(format!("{e:#}"));
                }
            }
        }
    }
}

struct SocketGuard(PathBuf);
impl Drop for SocketGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

pub fn serve() -> Result<()> {
    SHUTDOWN.store(false, Ordering::Relaxed);
    let dir = runtime_dir()?;
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(dir.join("daemon.lock"))?;
    ensure!(
        unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0,
        "Scrawl service is already running"
    );
    let path = dir.join("control.sock");
    if path.exists() {
        std::fs::remove_file(&path)?;
    }
    let listener = UnixListener::bind(&path)?;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    let _socket = SocketGuard(path);
    listener.set_nonblocking(true)?;
    unsafe {
        libc::signal(libc::SIGTERM, shutdown_signal as *const () as usize);
        libc::signal(libc::SIGINT, shutdown_signal as *const () as usize);
    }
    let mut service = Service::default();
    while !SHUTDOWN.load(Ordering::Relaxed) {
        service.tick();
        match listener.accept() {
            Ok((mut stream, _)) => {
                stream.set_read_timeout(Some(Duration::from_millis(500)))?;
                stream.set_write_timeout(Some(Duration::from_secs(2)))?;
                let response = (|| -> Result<Value> {
                    let mut line = String::new();
                    BufReader::new(&mut stream)
                        .take(MAX_REQUEST + 1)
                        .read_line(&mut line)?;
                    ensure!(line.len() as u64 <= MAX_REQUEST, "Request exceeds 1 MiB");
                    let request: Request = serde_json::from_str(&line)?;
                    service.request(request)?;
                    Ok(service.status())
                })();
                let reply =
                    response.unwrap_or_else(|e| json!({"ok":false,"error":format!("{e:#}")}));
                let _ = serde_json::to_writer(&mut stream, &reply);
                let _ = stream.write_all(b"\n");
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(30))
            }
            Err(e) => return Err(e.into()),
        }
    }
    if let Some(mut a) = service.active.take() {
        let _ = a.recorder.stop();
        let _ = write_metadata(&a.dir, &a.metadata);
    }
    // Signal-based shutdown waits for an owned worker so no capture process is orphaned.
    if let Some(worker) = service.worker.take() {
        match worker {
            Worker::Starting(rx, cancel) => {
                cancel.store(true, Ordering::Relaxed);
                let _ = rx.recv();
            }
            Worker::Finishing(rx) => {
                let _ = rx.recv();
            }
        }
    }
    drop(listener);
    drop(_socket);
    drop(lock);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_malformed_and_out_of_bounds_strokes() {
        let mut stroke = Stroke {
            t0: 0,
            t1: 100,
            fw: 800,
            fh: 600,
            pts: vec![0, 0, 0, 10_000, 10_000, 100],
            on: None,
            pin: None,
            laser: false,
        };
        assert!(validate_stroke(&stroke, 100).is_ok());
        stroke.pts[3] = 10_001;
        assert!(validate_stroke(&stroke, 100).is_err());
        stroke.pts[3] = 10_000;
        stroke.pts[5] = -1;
        assert!(validate_stroke(&stroke, 100).is_err());
        stroke.pts.pop();
        assert!(validate_stroke(&stroke, 100).is_err());
    }
    #[test]
    fn recorder_guard_reaps_its_child_on_drop() {
        let child = Command::new("sleep").arg("60").spawn().unwrap();
        let pid = child.id() as i32;
        drop(Recorder { child });
        assert_eq!(unsafe { libc::kill(pid, 0) }, -1);
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::ESRCH)
        );
    }
    #[test]
    fn private_runtime_rejects_symlink_and_public_directory() {
        let root = std::env::temp_dir().join(super::super::make_bundle_id());
        std::fs::create_dir(&root).unwrap();
        assert!(private_dir(&root).is_err());
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert!(private_dir(&root).is_ok());
        let link = root.with_extension("link");
        std::os::unix::fs::symlink(&root, &link).unwrap();
        assert!(private_dir(&link).is_err());
        std::fs::remove_file(link).unwrap();
        std::fs::remove_dir(root).unwrap();
    }
    #[test]
    fn invalid_requests_do_not_change_idle_state() {
        let mut s = Service::default();
        for command in ["stop", "draw", "undo", "stroke", "cancel", "bogus"] {
            assert!(
                s.request(Request {
                    command: command.into(),
                    ..Request::default()
                })
                .is_err()
            );
            assert_eq!(s.status()["state"], "idle");
        }
    }
}

//! Portable bundle processing for recordings made by the Linux backend.

use std::fs::{self, File};
use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};

use crate::model::{
    CursorSample, InteractionEvent, RgbaFrame, ScrawlBundle, Stroke, TargetInfo, TranscriptData,
    TranscriptSegment, WindowSnapshot,
};

const MAX_CANDIDATES: usize = 384;
const CANDIDATE_EDGE: u32 = 480;
const MAX_DECODE_BYTES: u64 = 384 * 1024 * 1024;
const MAX_LOG_BYTES: u64 = 8 * 1024 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CaptureMetadata {
    pub target: TargetInfo,
    #[serde(default)]
    pub strokes: Vec<Stroke>,
    #[serde(default)]
    pub events: Vec<InteractionEvent>,
    #[serde(default)]
    pub cursor: Vec<CursorSample>,
    #[serde(default)]
    pub windows: Vec<WindowSnapshot>,
    #[serde(default)]
    pub note: Option<String>,
    #[serde(default)]
    pub ink_in_video: bool,
    /// Half-open intervals on the movie clock when the drawing layer was visible.
    /// An open interval ends at `u64::MAX`; absent in recordings made before visibility logging.
    #[serde(default)]
    pub ink_sessions: Vec<[u64; 2]>,
}

pub fn build_bundle(dir: &Path, metadata: &CaptureMetadata) -> Result<ScrawlBundle> {
    build_bundle_with_whisper(dir, metadata, WhisperConfig::configured().as_ref())
}

/// Decode requested bundle times, preserving their original timestamps in the returned frames.
/// File-backed output and a per-command deadline avoid pipe deadlocks on large desktop frames.
pub fn frames_at(
    movie: &Path,
    times: &[u64],
    offset_ms: u64,
    max_edge: u32,
) -> Result<Vec<RgbaFrame>> {
    if times.is_empty() {
        return Ok(Vec::new());
    }
    let probe = probe(movie)?;
    let (width, height) = dimensions(
        &probe,
        if max_edge == 0 {
            2560
        } else {
            max_edge.min(2560)
        },
    );
    let frame_bytes = u64::from(width) * u64::from(height) * 4;
    ensure!(
        (times.len() as u64).saturating_mul(frame_bytes) <= MAX_DECODE_BYTES,
        "Requested frames exceed the 384 MiB decode limit; request fewer frames"
    );
    let temp = TempDir::new()?;
    let mut frames = Vec::with_capacity(times.len());
    for &time in times {
        let at = time
            .saturating_sub(offset_ms)
            .min(probe.duration_ms.saturating_sub(1));
        let mut cmd = ffmpeg();
        // Keep the preceding GOP, whose timestamps become negative relative to the seek.
        // The fps filter chooses the frame visible at zero, on a millisecond time base;
        // only one output frame is produced. Accurate seek alone picks a future frame on VFR.
        cmd.args([
            "-ss",
            &seconds(at),
            "-noaccurate_seek",
            "-noautorotate",
            "-i",
        ])
        .arg(movie)
        .args([
            "-map",
            "0:v:0",
            "-an",
            "-sn",
            "-dn",
            "-vf",
            &format!("fps=1000:start_time=0:round=up,scale={width}:{height}"),
            "-frames:v",
            "1",
            "-threads",
            "1",
            "-pix_fmt",
            "rgba",
            "-f",
            "rawvideo",
            "pipe:1",
        ]);
        run(&mut cmd, &temp.path, Duration::from_secs(45), frame_bytes)?;
        let rgba = fs::read(temp.path.join("stdout"))?;
        ensure!(
            rgba.len() as u64 == frame_bytes,
            "No complete video frame at {} ms in {}",
            at,
            movie.display()
        );
        frames.push(RgbaFrame::new(time, width, height, rgba));
    }
    Ok(frames)
}

/// A deterministic visual sample that does not need a compositor, microphone or speech model.
pub fn demo(out: &Path) -> Result<ScrawlBundle> {
    use crate::model::{CaptureMode, TargetFrame};
    if let Some(parent) = out.parent().filter(|p| !p.as_os_str().is_empty()) {
        fs::create_dir_all(parent)?;
    }
    match fs::create_dir(out) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            // The CLI atomically reserves a private directory before invoking the builder.
            ensure!(
                fs::read_dir(out)?.next().is_none(),
                "Output {} is not empty; choose a new output directory",
                out.display()
            );
        }
        Err(error) => {
            return Err(error).with_context(|| format!("Could not create {}", out.display()));
        }
    }
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(out, fs::Permissions::from_mode(0o700))?;
    let dir = out.canonicalize()?;
    let metadata = CaptureMetadata {
        target: TargetInfo {
            mode: CaptureMode::Region,
            app: Some("Scrawl demo".into()),
            bundle_id: None,
            window_title: None,
            window_id: None,
            display_id: 0,
            frame: TargetFrame {
                x: 0.0,
                y: 0.0,
                width: 1280.0,
                height: 800.0,
                scale: 1.0,
            },
        },
        strokes: vec![Stroke {
            t0: 1500,
            t1: 2000,
            fw: 1280,
            fh: 800,
            on: None,
            pin: None,
            laser: false,
            pts: vec![
                3800, 4800, 0, 4500, 4600, 100, 5200, 4800, 200, 5300, 6000, 300, 4500, 6200, 400,
                3800, 4800, 500,
            ],
        }],
        events: Vec::new(),
        cursor: Vec::new(),
        windows: Vec::new(),
        note: Some("Synthetic Linux demo; no microphone or desktop was recorded.".into()),
        ink_in_video: false,
        ink_sessions: Vec::new(),
    };
    let duration_ms = 6000;
    let candidates: Vec<_> = (0..24)
        .map(|n| {
            let t = n * 250;
            crate::render::render_synthetic_demo_frame(1280, 800, t, (t / 2500) as u32)
        })
        .collect();
    finish_bundle(
        &dir,
        &metadata,
        duration_ms,
        candidates,
        TranscriptData {
            source: "demo".into(),
            ..Default::default()
        },
        None,
        false,
        None,
    )
}

fn candidate_times(duration_ms: u64, metadata: &CaptureMetadata) -> Vec<u64> {
    let end = duration_ms.saturating_sub(1);
    let mut priority: Vec<_> = metadata
        .strokes
        .iter()
        .map(|s| s.t1.min(end))
        .chain(
            metadata
                .events
                .iter()
                .map(|e| e.timestamp_ms().saturating_add(150).min(end)),
        )
        .collect();
    priority.sort_unstable();
    priority.dedup();
    // Keep room for visual transitions. All marks remain in the manifest and selection;
    // pathological input logs share nearby samples instead of growing frame memory without bound.
    if priority.len() > MAX_CANDIDATES / 2 {
        priority = (0..MAX_CANDIDATES / 2)
            .map(|i| priority[i * (priority.len() - 1) / (MAX_CANDIDATES / 2 - 1)])
            .collect();
    }
    let regular = (duration_ms.div_ceil(250) as usize).clamp(1, MAX_CANDIDATES - priority.len());
    priority.extend((0..regular).map(|i| (i as u64 * duration_ms / regular as u64).min(end)));
    priority.sort_unstable();
    priority.dedup();
    priority
}

fn whisper_transcript(json: &[u8], duration_ms: u64, model: &Path) -> Result<TranscriptData> {
    let value: serde_json::Value =
        serde_json::from_slice(json).context("Invalid whisper.cpp JSON")?;
    let raw = value
        .get("transcription")
        .and_then(|v| v.as_array())
        .context("whisper.cpp JSON has no transcription array")?;
    let mut segments = Vec::new();
    for segment in raw {
        let text = segment
            .get("text")
            .and_then(|v| v.as_str())
            .context("whisper.cpp segment has no text")?
            .trim();
        if text.is_empty() {
            continue;
        }
        // whisper-cli's JSON offsets are milliseconds. Segment times are never word times.
        let t0 = segment
            .pointer("/offsets/from")
            .and_then(|v| v.as_u64())
            .context("whisper.cpp segment has no start time")?;
        let t1 = segment
            .pointer("/offsets/to")
            .and_then(|v| v.as_u64())
            .context("whisper.cpp segment has no end time")?;
        ensure!(t1 >= t0, "whisper.cpp segment ends before it starts");
        if t0 < duration_ms {
            segments.push(TranscriptSegment {
                t0,
                t1: t1.min(duration_ms),
                text: text.to_owned(),
            });
        }
    }
    segments.sort_by_key(|s| s.t0);
    Ok(TranscriptData {
        source: "whisper.cpp".into(),
        model: model.file_name().map(|v| v.to_string_lossy().into_owned()),
        text: segments
            .iter()
            .map(|s| s.text.as_str())
            .collect::<Vec<_>>()
            .join(" "),
        segments,
        words: Vec::new(),
        estimated: true,
    })
}

struct TempDir {
    path: PathBuf,
}
impl TempDir {
    fn new() -> Result<Self> {
        Self::in_dir(&std::env::temp_dir())
    }

    fn in_dir(parent: &Path) -> Result<Self> {
        use std::os::unix::fs::DirBuilderExt;
        static SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        for _ in 0..100 {
            let n = SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let path = parent.join(format!(".scrawl-media-{}-{n}", std::process::id()));
            match std::fs::DirBuilder::new().mode(0o700).create(&path) {
                Ok(()) => return Ok(Self { path }),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(e.into()),
            }
        }
        anyhow::bail!("Could not allocate a private media work directory")
    }
}
impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

struct WhisperConfig {
    model: PathBuf,
    binary: PathBuf,
}

impl WhisperConfig {
    fn configured() -> Option<Self> {
        let config: serde_json::Value = fs::read(crate::handoff::default_config_path())
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        let model = std::env::var_os("SCRAWL_WHISPER_MODEL")
            .map(PathBuf::from)
            .or_else(|| {
                config
                    .pointer("/linux/whisperModel")
                    .and_then(|v| v.as_str())
                    .map(PathBuf::from)
            })?;
        let binary = std::env::var_os("SCRAWL_WHISPER_BIN")
            .map(PathBuf::from)
            .or_else(|| {
                config
                    .pointer("/linux/whisperBin")
                    .and_then(|v| v.as_str())
                    .map(PathBuf::from)
            })
            .unwrap_or_else(|| "whisper-cli".into());
        Some(Self { model, binary })
    }

    fn transcribe(&self, wav: &Path, duration_ms: u64, temp: &TempDir) -> Result<TranscriptData> {
        ensure!(
            self.model.is_file(),
            "Speech model {} does not exist; install a whisper.cpp model and set SCRAWL_WHISPER_MODEL",
            self.model.display()
        );
        let output = temp.path.join("speech");
        let mut cmd = Command::new(&self.binary);
        cmd.arg("-m")
            .arg(&self.model)
            .arg("-f")
            .arg(wav)
            .args(["-oj", "-of"])
            .arg(&output)
            .args(["-l", "auto", "-t", "3"]);
        run(
            &mut cmd,
            &temp.path,
            Duration::from_secs((duration_ms / 1000 * 5 + 60).clamp(60, 1800)),
            MAX_LOG_BYTES,
        )?;
        let json = output.with_extension("json");
        ensure!(
            fs::metadata(&json)
                .context("whisper.cpp did not write JSON output")?
                .len()
                <= MAX_LOG_BYTES,
            "whisper.cpp transcript is too large"
        );
        whisper_transcript(&fs::read(json)?, duration_ms, &self.model)
    }
}

fn build_bundle_with_whisper(
    dir: &Path,
    metadata: &CaptureMetadata,
    whisper: Option<&WhisperConfig>,
) -> Result<ScrawlBundle> {
    let dir = dir
        .canonicalize()
        .with_context(|| format!("Capture directory {} does not exist", dir.display()))?;
    let movie = dir.join("recording.mp4");
    let info = probe(&movie)?;
    let temp = TempDir::in_dir(&dir)?;
    let (transcript, warning) = if info.audio {
        let audio = temp.path.join("audio.wav");
        let mut cmd = ffmpeg();
        cmd.arg("-i")
            .arg(&movie)
            .args([
                "-map",
                "0:a:0",
                "-vn",
                "-af",
                "aresample=16000:async=1:first_pts=0,apad",
                "-t",
                &seconds(info.duration_ms),
                "-ac",
                "1",
                "-c:a",
                "pcm_s16le",
                "-y",
            ])
            .arg(&audio);
        run(
            &mut cmd,
            &temp.path,
            Duration::from_secs(600),
            MAX_LOG_BYTES,
        )
        .context("Could not extract microphone audio; the original recording is preserved")?;
        fs::rename(&audio, dir.join("audio.wav")).context("Could not save audio.wav")?;
        match whisper {
            Some(config) => match config.transcribe(&dir.join("audio.wav"), info.duration_ms, &temp) {
                Ok(transcript) => (transcript, None),
                Err(error) => (unavailable_transcript(), Some(format!("Transcription unavailable: {error:#}. Audio is preserved in audio.wav and recording.mp4."))),
            },
            None => (unavailable_transcript(), Some("Transcription unavailable: set SCRAWL_WHISPER_MODEL to a local whisper.cpp model. Audio is preserved in audio.wav and recording.mp4.".into())),
        }
    } else {
        (
            TranscriptData {
                source: "no_audio".into(),
                ..Default::default()
            },
            None,
        )
    };
    if let Some(warning) = &warning {
        eprintln!("scrawl: {warning}");
    }
    let times = candidate_times(info.duration_ms, metadata);
    let candidates = candidates(&movie, &info, &times, &temp)?;
    finish_bundle(
        &dir,
        metadata,
        info.duration_ms,
        candidates,
        transcript,
        Some(&movie),
        info.audio,
        warning.as_deref(),
    )
}

fn unavailable_transcript() -> TranscriptData {
    TranscriptData {
        source: "unavailable".into(),
        ..Default::default()
    }
}

#[derive(Debug)]
struct Probe {
    width: u32,
    height: u32,
    duration_ms: u64,
    audio: bool,
}

fn probe(movie: &Path) -> Result<Probe> {
    ensure!(movie.is_file(), "Recording {} is missing", movie.display());
    let temp = TempDir::new()?;
    let mut cmd = Command::new("ffprobe");
    cmd.args([
        "-v",
        "error",
        "-show_entries",
        "format=duration:stream=codec_type,width,height,duration",
        "-of",
        "json",
    ])
    .arg(movie);
    run(&mut cmd, &temp.path, Duration::from_secs(20), 1024 * 1024)?;
    let value: serde_json::Value = serde_json::from_slice(&fs::read(temp.path.join("stdout"))?)
        .context("Invalid ffprobe output")?;
    let streams = value
        .get("streams")
        .and_then(|v| v.as_array())
        .context("Recording has no media streams")?;
    let video = streams
        .iter()
        .find(|s| s.get("codec_type").and_then(|v| v.as_str()) == Some("video"))
        .context("Recording has no video stream")?;
    let number = |v: &serde_json::Value| {
        v.as_str()
            .and_then(|s| s.parse::<f64>().ok())
            .or_else(|| v.as_f64())
    };
    let duration = video
        .get("duration")
        .and_then(number)
        .or_else(|| value.pointer("/format/duration").and_then(number))
        .context("Recording has no duration")?;
    ensure!(
        duration.is_finite() && duration > 0.0 && duration <= 86_400.0,
        "Recording duration must be between zero and 24 hours"
    );
    let dimension = |key| {
        video
            .get(key)
            .and_then(|v| v.as_u64())
            .and_then(|n| u32::try_from(n).ok())
            .filter(|&n| n > 0)
    };
    Ok(Probe {
        width: dimension("width").context("Recording has no video width")?,
        height: dimension("height").context("Recording has no video height")?,
        duration_ms: (duration * 1000.0).round().max(1.0) as u64,
        audio: streams
            .iter()
            .any(|s| s.get("codec_type").and_then(|v| v.as_str()) == Some("audio")),
    })
}

fn dimensions(probe: &Probe, edge: u32) -> (u32, u32) {
    let factor = (f64::from(edge) / f64::from(probe.width.max(probe.height))).min(1.0);
    (
        (f64::from(probe.width) * factor).round().max(1.0) as u32,
        (f64::from(probe.height) * factor).round().max(1.0) as u32,
    )
}

fn seconds(ms: u64) -> String {
    format!("{}.{:03}", ms / 1000, ms % 1000)
}

fn ffmpeg() -> Command {
    let mut command = Command::new("ffmpeg");
    command.args([
        "-hide_banner",
        "-loglevel",
        "error",
        "-nostdin",
        "-nostats",
        "-threads",
        "2",
        "-filter_threads",
        "1",
    ]);
    command
}

fn candidates(
    movie: &Path,
    probe: &Probe,
    times: &[u64],
    temp: &TempDir,
) -> Result<Vec<RgbaFrame>> {
    let (width, height) = dimensions(probe, CANDIDATE_EDGE);
    let frame_bytes = u64::from(width) * u64::from(height) * 4;
    // Select the first real frame at/after each requested time, retaining its actual PTS.
    // One sequential decoder is much faster than hundreds of separate movie seeks.
    let select = std::iter::once("eq(n,0)".to_owned())
        .chain(
            times
                .iter()
                .filter(|&&t| t > 0)
                .map(|&t| format!("gte(t,{0})*lt(prev_selected_t,{0})", seconds(t))),
        )
        .collect::<Vec<_>>()
        .join("+");
    let filter = format!("select='{select}',scale={width}:{height},showinfo");
    let mut cmd = ffmpeg();
    cmd.args(["-loglevel", "info", "-noautorotate", "-i"])
        .arg(movie)
        .args([
            "-map",
            "0:v:0",
            "-an",
            "-sn",
            "-dn",
            "-vf",
            &filter,
            "-fps_mode",
            "passthrough",
            "-frames:v",
            &times.len().to_string(),
            "-threads",
            "1",
            "-pix_fmt",
            "rgba",
            "-f",
            "rawvideo",
            "pipe:1",
        ]);
    run(
        &mut cmd,
        &temp.path,
        Duration::from_secs((probe.duration_ms / 1000 + 60).clamp(60, 900)),
        frame_bytes * times.len() as u64,
    )?;
    let timestamps: Vec<_> = fs::read_to_string(temp.path.join("stderr"))?
        .lines()
        .filter_map(|line| {
            line.split_once("pts_time:")
                .and_then(|(_, tail)| tail.split_whitespace().next())
                .and_then(|s| s.parse::<f64>().ok())
        })
        .filter(|t| t.is_finite() && *t >= 0.0)
        .map(|t| (t * 1000.0).round() as u64)
        .collect();
    let mut input = BufReader::new(File::open(temp.path.join("stdout"))?);
    let bytes = input.get_ref().metadata()?.len();
    ensure!(
        bytes > 0 && bytes % frame_bytes == 0,
        "FFmpeg could not decode complete candidate frames"
    );
    let count = (bytes / frame_bytes) as usize;
    ensure!(
        count <= MAX_CANDIDATES && count == timestamps.len(),
        "FFmpeg frame timestamps did not match the decoded frames"
    );
    let mut frames = Vec::with_capacity(count);
    for t in timestamps {
        let mut rgba = vec![0; frame_bytes as usize];
        input.read_exact(&mut rgba)?;
        frames.push(RgbaFrame::new(t, width, height, rgba));
    }
    Ok(frames)
}

#[allow(clippy::too_many_arguments)]
fn finish_bundle(
    dir: &Path,
    metadata: &CaptureMetadata,
    duration_ms: u64,
    candidates: Vec<RgbaFrame>,
    transcript: TranscriptData,
    movie: Option<&Path>,
    audio: bool,
    warning: Option<&str>,
) -> Result<ScrawlBundle> {
    let mut marks = crate::sketch::marks_of(&metadata.strokes);
    let mut selected = crate::keyframes::select_keyframes(
        &candidates,
        &metadata.strokes,
        &mut marks,
        &metadata.events,
        &transcript,
        crate::model::keyframe_budget(duration_ms),
    );
    ensure!(
        !selected.is_empty(),
        "No keyframes could be selected from the recording"
    );
    if let Some(movie) = movie {
        let times: Vec<_> = selected.iter().map(|(k, _)| k.t).collect();
        let decoded = frames_at(movie, &times, 0, 2560).context(
            "Could not decode full-size keyframes; recording is preserved for reprocessing",
        )?;
        for ((_, frame), full) in selected.iter_mut().zip(decoded) {
            *frame = full;
        }
    }
    let baseline = candidates.first().cloned();
    drop(candidates);
    let settings = crate::autocrop::Settings::configured();
    let (mut selected, mut cropped) = if metadata.ink_in_video {
        // Marks on one screen can share a later keyframe even if Draw was closed in between.
        // Restore those cleared strokes, retaining their original indices for Mark::strokes.
        // Keep full framing: cropping cannot remove baked-in annotations independently.
        let mut frames: Vec<_> = selected
            .into_iter()
            .map(|(keyframe, mut frame)| {
                let mut strokes = metadata.strokes.clone();
                for (index, stroke) in strokes.iter_mut().enumerate() {
                    let assigned = marks.iter().any(|mark| {
                        mark.strokes.contains(&index)
                            && (mark.frame.as_ref() == Some(&keyframe.file)
                                || keyframe.mark_id.as_ref() == Some(&mark.id))
                    });
                    let baked = metadata.ink_sessions.iter().any(|&[start, end]| {
                        start <= keyframe.t
                            && keyframe.t < end
                            && start <= stroke.t0
                            && stroke.t0 <= keyframe.t
                    });
                    if metadata.ink_sessions.is_empty() || baked || !assigned {
                        stroke.pts.clear();
                    }
                }
                crate::render::annotate_keyframe_marks_in_place(
                    &mut frame,
                    &keyframe,
                    &strokes,
                    &marks,
                    &metadata.events,
                );
                (keyframe, frame)
            })
            .collect();
        for (keyframe, frame) in &mut frames {
            frame.badge_top_right |= metadata.strokes.iter().any(|s| {
                s.t0 <= keyframe.t
                    && s.normalized_points()
                        .iter()
                        .any(|p| p[0] < 0.30 && p[1] < 0.18)
            });
        }
        (frames, None)
    } else {
        crate::autocrop::annotate_and_crop(
            selected,
            baseline.as_ref(),
            settings.as_ref(),
            &metadata.target,
            duration_ms,
            &metadata.strokes,
            &marks,
            &metadata.events,
            &metadata.windows,
            &metadata.cursor,
        )
    };
    let bundle = ScrawlBundle {
        version: 1,
        id: dir
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned(),
        created_at_ms: SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis() as u64,
        duration_ms,
        target: metadata.target.clone(),
        transcript,
        note: metadata.note.clone(),
        strokes: metadata.strokes.clone(),
        marks,
        events: metadata.events.clone(),
        keyframes: selected.iter().map(|(k, _)| k.clone()).collect(),
        contact_sheet: "contact-sheet.png".into(),
        video: movie.map(|_| "recording.mp4".into()),
        video_offset_ms: 0,
        audio: audio.then(|| "audio.wav".into()),
        source: Some("linux".into()),
        device: None,
        context: Default::default(),
        context_changes: Vec::new(),
        screens: Vec::new(),
        windows: metadata.windows.clone(),
        cursor: metadata.cursor.clone(),
        autocrop: cropped.as_ref().map(|(info, _)| info.clone()),
        destination: None,
    };
    let sheet = crate::render::render_contact_sheet(&bundle, &selected);
    if let Some((info, full)) = &mut cropped {
        let full_sheet = crate::render::render_contact_sheet(&bundle, full);
        crate::render::stamp_keyframe_badges(full);
        crate::autocrop::write_full_frames(dir, info, full, &full_sheet)?;
    }
    crate::render::stamp_keyframe_badges(&mut selected);
    fs::create_dir_all(dir.join("frames"))?;
    for (keyframe, frame) in selected {
        crate::render::save_png(&frame, &dir.join(keyframe.file))?;
    }
    crate::render::save_png(&sheet, &dir.join(&bundle.contact_sheet))?;
    let mut summary = crate::sketch::summary_markdown(&bundle, dir);
    if let Some(warning) = warning {
        summary.push_str(&format!("\n{warning}\n"));
    }
    if metadata.ink_in_video && metadata.ink_sessions.is_empty() && !metadata.strokes.is_empty() {
        let warning = "Drawing visibility history is unavailable for this older capture. Existing video ink is preserved; grouped strokes from cleared drawing sessions may be missing.";
        eprintln!("scrawl: {warning}");
        summary.push_str(&format!("\n{warning}\n"));
    }
    summary.push_str(&format!(
        "\nContact sheet: {}\n",
        dir.join(&bundle.contact_sheet).display()
    ));
    for keyframe in &bundle.keyframes {
        summary.push_str(&format!(
            "#{}: {}\n",
            keyframe.index,
            dir.join(&keyframe.file).display()
        ));
    }
    if let Some(video) = &bundle.video {
        summary.push_str(&format!("Video: {}\n", dir.join(video).display()));
    }
    summary.push_str(&format!("Bundle: {}\n", dir.display()));
    fs::write(dir.join("summary.md"), summary)?;
    // The manifest is the completion marker: write it only after every visual artifact succeeds.
    let manifest = dir.join(".scrawl.json.tmp");
    fs::write(&manifest, serde_json::to_vec_pretty(&bundle)?)?;
    fs::rename(manifest, dir.join("scrawl.json"))?;
    Ok(bundle)
}

/// Subprocess output goes to private files, never unread pipes. Limits apply while it runs,
/// and timeout/error paths kill and reap the child before returning.
fn run(cmd: &mut Command, dir: &Path, timeout: Duration, max_output: u64) -> Result<()> {
    let stdout = dir.join("stdout");
    let stderr = dir.join("stderr");
    let program = cmd.get_program().to_string_lossy().into_owned();
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(File::create(&stdout)?)
        .stderr(File::create(&stderr)?)
        .spawn()
        .with_context(|| {
            format!("Could not start {program}; install it and ensure it is on PATH")
        })?;
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {}
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(error).context(format!("Could not wait for {program}"));
            }
        }
        let timed_out = started.elapsed() >= timeout;
        let too_large = fs::metadata(&stdout).is_ok_and(|m| m.len() > max_output)
            || fs::metadata(&stderr).is_ok_and(|m| m.len() > MAX_LOG_BYTES);
        if timed_out || too_large {
            let _ = child.kill();
            let _ = child.wait();
            if timed_out {
                bail!(
                    "{program} timed out after {:.1} seconds",
                    timeout.as_secs_f64()
                );
            }
            bail!("{program} exceeded its media output limit");
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    ensure!(
        fs::metadata(&stdout)?.len() <= max_output && fs::metadata(&stderr)?.len() <= MAX_LOG_BYTES,
        "{program} exceeded its media output limit"
    );
    if !status.success() {
        let mut message = String::new();
        File::open(stderr)?
            .take(8192)
            .read_to_string(&mut message)?;
        bail!("{program} failed ({status}): {}", message.trim());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{CaptureMode, TargetFrame};
    use std::process::Command;

    fn fixture(with_audio: bool) -> TempDir {
        let temp = TempDir::new().unwrap();
        let mut cmd = Command::new("ffmpeg");
        cmd.args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-nostdin",
            "-f",
            "lavfi",
            "-i",
            "color=c=blue:s=640x360:r=10:d=2",
        ]);
        if with_audio {
            // The source audio begins 0.5 s after the movie, exercising asset-timeline padding.
            cmd.args([
                "-itsoffset",
                "0.5",
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=440:sample_rate=48000:duration=1.5",
            ]);
        }
        cmd.args([
            "-c:v", "libx264", "-threads", "1", "-pix_fmt", "yuv420p", "-c:a", "aac", "-y",
        ])
        .arg(temp.path.join("recording.mp4"));
        assert!(
            cmd.status()
                .expect("FFmpeg is required for Linux media tests")
                .success()
        );
        temp
    }

    fn metadata() -> CaptureMetadata {
        CaptureMetadata {
            target: TargetInfo {
                mode: CaptureMode::Region,
                app: None,
                bundle_id: None,
                window_title: None,
                window_id: None,
                display_id: 0,
                frame: TargetFrame {
                    x: 0.0,
                    y: 0.0,
                    width: 640.0,
                    height: 360.0,
                    scale: 1.0,
                },
            },
            strokes: Vec::new(),
            events: Vec::new(),
            cursor: Vec::new(),
            windows: Vec::new(),
            note: None,
            ink_in_video: false,
            ink_sessions: Vec::new(),
        }
    }

    #[test]
    fn candidate_sampling_keeps_marks_and_bounds_long_movies() {
        let mut meta = metadata();
        meta.strokes.push(Stroke {
            t0: 121_000,
            t1: 121_250,
            fw: 640,
            fh: 360,
            on: None,
            pin: None,
            laser: false,
            pts: vec![1000, 1000, 0, 2000, 2000, 250],
        });
        let times = candidate_times(6 * 60 * 60 * 1000, &meta);
        assert_eq!(times.first(), Some(&0));
        assert!(
            times.contains(&121_250),
            "a brief drawing must survive sparse sampling"
        );
        assert!(times.len() <= 384);
        assert!(times.windows(2).all(|pair| pair[0] < pair[1]));
        assert!(times.last().copied().unwrap() < 6 * 60 * 60 * 1000);
    }

    #[test]
    fn whisper_keeps_segment_times_without_inventing_word_times() {
        let json = br#"{"transcription":[
            {"offsets":{"from":450,"to":1350},"text":" Now look here."},
            {"offsets":{"from":1500,"to":2500},"text":" Another sentence."}
        ]}"#;
        let transcript = whisper_transcript(json, 2000, Path::new("/models/base.bin")).unwrap();
        assert_eq!(transcript.text, "Now look here. Another sentence.");
        assert_eq!(
            (transcript.segments[0].t0, transcript.segments[0].t1),
            (450, 1350)
        );
        assert_eq!(transcript.segments[1].t1, 2000);
        assert!(transcript.words.is_empty());
        assert!(
            transcript.estimated,
            "segment timing must not enable measured spoken cues"
        );
        assert_eq!(transcript.source, "whisper.cpp");
    }

    #[test]
    fn whisper_rejects_missing_timing_and_accepts_real_silence() {
        assert!(
            whisper_transcript(
                br#"{"transcription":[{"text":"hello"}]}"#,
                1000,
                Path::new("base.bin")
            )
            .is_err()
        );
        let silent =
            whisper_transcript(br#"{"transcription":[]}"#, 1000, Path::new("base.bin")).unwrap();
        assert!(silent.text.is_empty());
        assert!(silent.segments.is_empty());
    }

    #[test]
    fn ffmpeg_static_capture_stays_compact_and_preserves_delayed_audio() {
        let temp = fixture(true);
        let bundle = build_bundle_with_whisper(&temp.path, &metadata(), None).unwrap();
        assert_eq!(
            bundle.keyframes.len(),
            1,
            "static video should not gain pause frames"
        );
        assert_eq!(bundle.audio.as_deref(), Some("audio.wav"));
        assert_eq!(bundle.transcript.source, "unavailable");
        assert!(bundle.transcript.text.is_empty());
        let mut wav = hound::WavReader::open(temp.path.join("audio.wav")).unwrap();
        let rate = wav.spec().sample_rate as usize;
        let samples: Vec<i16> = wav.samples::<i16>().map(Result::unwrap).collect();
        assert!(
            samples.len() >= rate * 19 / 10,
            "audio must retain its movie timeline"
        );
        assert!(
            samples[..rate / 4].iter().all(|s| s.abs() < 10),
            "audio must begin with the original silence"
        );
        assert!(samples[rate..].iter().any(|s| s.abs() > 100));
        let summary = std::fs::read_to_string(temp.path.join("summary.md")).unwrap();
        assert!(summary.contains("Transcription unavailable"));
        assert!(summary.contains(&format!(
            "Contact sheet: {}",
            temp.path.join("contact-sheet.png").display()
        )));
        assert!(temp.path.join(&bundle.keyframes[0].file).is_file());
        assert!(temp.path.join("scrawl.json").is_file());
    }

    #[test]
    fn ffmpeg_frames_use_bundle_clock_and_limit_resolution() {
        let temp = fixture(false);
        let frames = frames_at(&temp.path.join("recording.mp4"), &[250, 1250], 250, 160).unwrap();
        assert_eq!(frames.len(), 2);
        assert_eq!((frames[0].t_ms, frames[1].t_ms), (250, 1250));
        assert_eq!((frames[0].width, frames[0].height), (160, 90));
        assert!(
            frames[0]
                .rgba
                .chunks_exact(4)
                .all(|p| p[2] > 200 && p[0] < 30)
        );
    }

    #[test]
    fn zero_frame_edge_preserves_native_resolution() {
        let temp = fixture(false);
        let frames = frames_at(&temp.path.join("recording.mp4"), &[1000], 0, 0).unwrap();
        assert_eq!((frames[0].width, frames[0].height), (640, 360));
    }

    #[test]
    fn sparse_video_returns_the_visible_frame_instead_of_the_next_frame() {
        let temp = TempDir::new().unwrap();
        let movie = temp.path.join("recording.mp4");
        assert!(
            Command::new("ffmpeg")
                .args([
                    "-v",
                    "error",
                    "-f",
                    "lavfi",
                    "-i",
                    "testsrc2=s=64x36:r=1:d=3",
                    "-c:v",
                    "libx264",
                    "-threads",
                    "1"
                ])
                .arg(&movie)
                .status()
                .unwrap()
                .success()
        );
        let frames = frames_at(&movie, &[0, 500, 1000, 1500, 2000, 2999], 0, 64).unwrap();
        assert!(
            frames[0].rgba == frames[1].rgba,
            "a still frame persists until the next timestamp"
        );
        assert!(frames[2].rgba == frames[3].rgba);
        assert!(
            frames[4].rgba == frames[5].rgba,
            "the final frame persists until the movie ends"
        );
        assert!(frames[0].rgba != frames[2].rgba);
    }

    #[test]
    fn large_subprocess_output_is_drained_and_capped() {
        let temp = TempDir::new().unwrap();
        let command = || {
            let mut cmd = Command::new("head");
            cmd.args(["-c", "1048576", "/dev/zero"]);
            cmd
        };
        run(
            &mut command(),
            &temp.path,
            Duration::from_secs(2),
            2 * 1024 * 1024,
        )
        .unwrap();
        assert_eq!(
            fs::metadata(temp.path.join("stdout")).unwrap().len(),
            1048576
        );
        assert!(
            run(&mut command(), &temp.path, Duration::from_secs(2), 1024)
                .unwrap_err()
                .to_string()
                .contains("output limit")
        );
    }

    #[test]
    fn captured_ink_is_not_painted_again() {
        let temp = fixture(false);
        let mut meta = metadata();
        meta.ink_in_video = true;
        meta.strokes.push(Stroke {
            t0: 250,
            t1: 500,
            fw: 640,
            fh: 360,
            on: None,
            pin: None,
            laser: false,
            pts: vec![2000, 6000, 0, 8000, 6000, 250],
        });
        let bundle = build_bundle_with_whisper(&temp.path, &meta, None).unwrap();
        assert_eq!(bundle.marks.len(), 1);
        let frame = crate::render::decode_png(std::io::BufReader::new(
            std::fs::File::open(temp.path.join(&bundle.keyframes[0].file)).unwrap(),
        ))
        .unwrap();
        assert!(
            frame
                .rgba
                .chunks_exact(4)
                .skip(640 * 180)
                .all(|p| p[2] > 200 && p[0] < 30),
            "the lower half should remain the movie's blue, without metadata ink"
        );
    }

    #[test]
    fn merged_marks_restore_cleared_sessions_without_repainting_visible_ink() {
        let temp = TempDir::new().unwrap();
        let movie = temp.path.join("recording.mp4");
        assert!(Command::new("ffmpeg").args([
            "-v", "error", "-f", "lavfi", "-i", "color=white:s=640x360:r=10:d=5",
            "-vf", "drawbox=x=50:y=148:w=30:h=4:color=red:t=fill:enable='between(t,0.8,1.499)',drawbox=x=450:y=148:w=30:h=4:color=red:t=fill:enable='between(t,2.8,3.999)'",
            "-c:v", "libx264", "-threads", "1",
        ]).arg(&movie).status().unwrap().success());
        let mut meta = metadata();
        meta.ink_in_video = true;
        for (t0, x0, x1) in [(800, 781, 1250), (2800, 7031, 7500)] {
            meta.strokes.push(Stroke {
                t0,
                t1: t0 + 200,
                fw: 640,
                fh: 360,
                on: None,
                pin: None,
                laser: false,
                pts: vec![x0, 4167, 0, x1, 4167, 200],
            });
        }
        // Exercise the persisted metadata contract, including compatibility with earlier files.
        let mut value = serde_json::to_value(meta).unwrap();
        value["inkSessions"] = serde_json::json!([[500, 1500], [2500, 4000]]);
        let meta: CaptureMetadata = serde_json::from_value(value).unwrap();
        let bundle = build_bundle_with_whisper(&temp.path, &meta, None).unwrap();
        assert_eq!(bundle.marks.len(), 2);
        assert_eq!(
            bundle.marks[0].frame, bundle.marks[1].frame,
            "same-screen marks should still share one keyframe"
        );
        let frame = crate::render::decode_png(BufReader::new(
            File::open(temp.path.join(bundle.marks[0].frame.as_ref().unwrap())).unwrap(),
        ))
        .unwrap();
        // PNG loading has no timestamp: obtain the selected frame's time from the manifest.
        let selected = bundle
            .keyframes
            .iter()
            .find(|k| Some(&k.file) == bundle.marks[0].frame.as_ref())
            .unwrap();
        let raw = frames_at(&movie, &[selected.t], 0, 640).unwrap();
        let red = |p: &[u8]| p[0] > 180 && p[1] < 120 && p[2] < 120;
        let baked_center = (150 * 640 + 465) * 4;
        assert!(
            red(&raw[0].rgba[baked_center..baked_center + 4]),
            "the selected movie frame must contain the second session's ink"
        );
        let restored = (140..160)
            .flat_map(|y| (45..90).map(move |x| (y * 640 + x) * 4))
            .filter(|&i| red(&frame.rgba[i..i + 4]))
            .count();
        assert!(
            restored > 40,
            "the first session's assigned underline must be restored"
        );
        for y in 100..250 {
            let row = (y * 640 + 320) * 4..(y + 1) * 640 * 4;
            assert!(
                frame.rgba[row.clone()] == raw[0].rgba[row],
                "the already captured second stroke must remain pixel-identical"
            );
        }
    }

    #[test]
    fn process_timeout_reaps_the_worker() {
        let temp = TempDir::new().unwrap();
        let pid_file = temp.path.join("pid");
        let mut cmd = Command::new("sh");
        cmd.args(["-c", "echo $$ > \"$1\"; exec sleep 30", "scrawl-test"])
            .arg(&pid_file);
        let result = run(
            &mut cmd,
            &temp.path,
            std::time::Duration::from_millis(150),
            1024,
        );
        assert!(result.unwrap_err().to_string().contains("timed out"));
        let pid = std::fs::read_to_string(pid_file).unwrap();
        assert!(!Path::new("/proc").join(pid.trim()).exists());
    }

    #[test]
    fn demo_refuses_existing_output_and_writes_visual_artifacts() {
        let temp = TempDir::new().unwrap();
        let out = temp.path.join("demo.scrawl");
        // The CLI reserves a private output directory before passing it to the pipeline.
        fs::create_dir(&out).unwrap();
        let bundle = demo(&out).unwrap();
        assert!(!bundle.strokes.is_empty());
        assert!(out.join("contact-sheet.png").is_file());
        assert!(demo(&out).is_err());
    }
}

//! Deterministic layered export primitives.
//!
//! The frame schedule is rational and shared with the reference compositor;
//! the optional FFmpeg bridge feeds those composed RGBA frames to a pinned
//! executable and publishes only a completed temporary output.

use crate::composition::frame::{FrameLimits, RgbaFrame};
use crate::composition::{self, CompositionError, MediaSource, RenderOptions};
use crate::engine::api::SourceId;
use crate::engine::audio::{
    AudioInterval as EngineAudioInterval, AudioMixConfig, AudioMixer, ClippingPolicy,
};
use crate::engine::decode::{DecodeRequest, SoftwareDecoder};
use crate::media::Binaries;
use crate::project::{AssetId, AssetMetadata, FrameRate, Project, RationalError, Time, TimeRange};
use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::{Cursor, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::{SystemTime, UNIX_EPOCH};

/// One exact output-frame timestamp.  `index` is zero-based within the
/// selected export range, while `time` is an absolute project time.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ScheduledFrame {
    pub index: u64,
    pub time: Time,
}

/// A half-open, constant-cadence frame schedule.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FrameSchedule {
    pub range: TimeRange,
    pub frame_rate: FrameRate,
    count: u64,
}

impl FrameSchedule {
    pub fn new(range: TimeRange, frame_rate: FrameRate) -> Result<Self, ScheduleError> {
        let frame_duration = frame_rate.frame_duration()?;
        let duration = range.duration().map_err(ScheduleError::Time)?;
        let frame_count = duration.checked_div(frame_duration)?.ceil_i128();
        let count = u64::try_from(frame_count).map_err(|_| ScheduleError::Overflow)?;
        if count == 0 {
            return Err(ScheduleError::Empty);
        }
        Ok(Self {
            range,
            frame_rate,
            count,
        })
    }

    pub const fn len(self) -> u64 {
        self.count
    }

    pub const fn is_empty(self) -> bool {
        self.count == 0
    }

    pub fn frame_at(self, index: u64) -> Result<Option<ScheduledFrame>, ScheduleError> {
        if index >= self.count {
            return Ok(None);
        }
        let offset = self.frame_rate.frame_start(index)?;
        let time = self.range.start.checked_add(offset)?;
        if time >= self.range.end {
            return Ok(None);
        }
        Ok(Some(ScheduledFrame { index, time }))
    }

    pub fn iter(self) -> impl Iterator<Item = ScheduledFrame> {
        (0..self.count).filter_map(move |index| self.frame_at(index).ok().flatten())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ScheduleError {
    Empty,
    Overflow,
    Range(crate::project::TimeRangeError),
    Time(RationalError),
}

impl std::fmt::Display for ScheduleError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Empty => formatter.write_str("export range has no output frames"),
            Self::Overflow => formatter.write_str("export frame count overflowed"),
            Self::Range(error) => error.fmt(formatter),
            Self::Time(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for ScheduleError {}

impl From<RationalError> for ScheduleError {
    fn from(error: RationalError) -> Self {
        Self::Time(error)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RenderReport {
    pub frames: u64,
}

/// Render exact scheduled frames to a caller-owned sink.  The sink is called
/// in order and may stream frames to an encoder without retaining the whole
/// export in memory.
pub fn render_range<M, F>(
    project: &Project,
    range: TimeRange,
    media: &M,
    options: RenderOptions,
    mut consume: F,
) -> Result<RenderReport, ExportError>
where
    M: MediaSource,
    F: FnMut(ScheduledFrame, &RgbaFrame) -> Result<(), ExportError>,
{
    project.validate()?;
    let schedule = FrameSchedule::new(range, project.frame_rate).map_err(ExportError::Schedule)?;
    let mut frames: u64 = 0;
    for scheduled in schedule.iter() {
        let frame = composition::render_with_options(project, scheduled.time, media, options)?;
        consume(scheduled, &frame)?;
        frames = frames.saturating_add(1);
    }
    Ok(RenderReport { frames })
}

/// The formats supported by the initial composition encoder.  The legacy job
/// remains responsible for audio-only and packet-copy formats.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CompositionFormat {
    Mp4,
    Mkv,
    Gif,
    Wav,
}

impl CompositionFormat {
    pub fn from_extension(path: &Path) -> Option<Self> {
        match path.extension()?.to_str()?.to_ascii_lowercase().as_str() {
            "mp4" => Some(Self::Mp4),
            "mkv" => Some(Self::Mkv),
            "gif" => Some(Self::Gif),
            "wav" => Some(Self::Wav),
            _ => None,
        }
    }

    const fn muxer(self) -> &'static str {
        match self {
            Self::Mp4 => "mp4",
            Self::Mkv => "matroska",
            Self::Gif => "gif",
            Self::Wav => "wav",
        }
    }
}

#[derive(Clone, Debug)]
pub struct CompositionExportRequest {
    pub project: Project,
    pub binaries: Binaries,
    pub output: PathBuf,
    pub range: Option<TimeRange>,
    pub format: CompositionFormat,
    pub quality: u8,
    pub render_options: RenderOptions,
}

impl CompositionExportRequest {
    pub fn validate(&self) -> Result<TimeRange, ExportError> {
        self.project.validate()?;
        self.binaries.validate().map_err(ExportError::Tool)?;
        let range = self
            .range
            .or_else(|| self.project.output_range())
            .ok_or(ExportError::Schedule(ScheduleError::Empty))?;
        if self.output.as_os_str().is_empty() {
            return Err(ExportError::Invalid("export output is empty".to_owned()));
        }
        Ok(range)
    }
}

#[derive(Debug)]
pub enum ExportError {
    Project(crate::project::ProjectError),
    Composition(CompositionError),
    Schedule(ScheduleError),
    Tool(anyhow::Error),
    Io(std::io::Error),
    Invalid(String),
    MissingSource(PathBuf),
    SourceChanged(PathBuf),
    DestinationExists(PathBuf),
    Cancelled,
    Encoder(String),
}

impl std::fmt::Display for ExportError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Project(error) => error.fmt(formatter),
            Self::Composition(error) => error.fmt(formatter),
            Self::Schedule(error) => error.fmt(formatter),
            Self::Tool(error) => error.fmt(formatter),
            Self::Io(error) => error.fmt(formatter),
            Self::Invalid(error) => formatter.write_str(error),
            Self::MissingSource(path) => {
                write!(
                    formatter,
                    "export source is missing or not a regular file: {}",
                    path.display()
                )
            }
            Self::SourceChanged(path) => {
                write!(
                    formatter,
                    "export source changed while rendering: {}",
                    path.display()
                )
            }
            Self::DestinationExists(path) => {
                write!(formatter, "refusing to overwrite {}", path.display())
            }
            Self::Cancelled => formatter.write_str("composition export cancelled"),
            Self::Encoder(error) => formatter.write_str(error),
        }
    }
}

impl std::error::Error for ExportError {}

impl From<crate::project::ProjectError> for ExportError {
    fn from(error: crate::project::ProjectError) -> Self {
        Self::Project(error)
    }
}

impl From<CompositionError> for ExportError {
    fn from(error: CompositionError) -> Self {
        Self::Composition(error)
    }
}

impl From<std::io::Error> for ExportError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

/// Cooperative cancellation shared by synchronous and threaded export APIs.
#[derive(Clone, Default)]
pub struct ExportControl {
    cancelled: Arc<AtomicBool>,
}

impl ExportControl {
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum CompositionExportEvent {
    Progress { completed: u64, total: u64 },
    Completed(PathBuf),
    Cancelled,
    Failed(String),
}

pub struct CompositionExportJob {
    pub events: mpsc::Receiver<CompositionExportEvent>,
    control: ExportControl,
}

impl CompositionExportJob {
    pub fn cancel(&self) {
        self.control.cancel();
    }
}

/// Spawn a bounded-worker composition export.  The input project is cloned
/// as an immutable snapshot before the worker starts.
pub fn spawn_composition_export(
    request: CompositionExportRequest,
) -> Result<CompositionExportJob, ExportError> {
    request.validate()?;
    let control = ExportControl::default();
    let worker_control = control.clone();
    let (sender, events) = mpsc::channel();
    thread::Builder::new()
        .name("slicer-composition-export".to_owned())
        .spawn(move || {
            let result = export_project(&request, &worker_control, |completed, total| {
                let _ = sender.send(CompositionExportEvent::Progress { completed, total });
            });
            let event = match result {
                Ok(report) => CompositionExportEvent::Completed(report.output),
                Err(ExportError::Cancelled) => CompositionExportEvent::Cancelled,
                Err(error) => CompositionExportEvent::Failed(error.to_string()),
            };
            let _ = sender.send(event);
        })
        .map_err(|error| ExportError::Io(std::io::Error::other(error.to_string())))?;
    Ok(CompositionExportJob { events, control })
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExportReport {
    pub output: PathBuf,
    pub frames: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct SourceFingerprint {
    path: PathBuf,
    bytes: u64,
    modified: Option<SystemTime>,
}

fn capture_source_fingerprints(
    project: &Project,
) -> Result<BTreeMap<AssetId, SourceFingerprint>, ExportError> {
    project
        .assets
        .iter()
        .map(|(asset_id, asset)| {
            let metadata = fs::metadata(&asset.path)
                .map_err(|_| ExportError::MissingSource(asset.path.clone()))?;
            if !metadata.is_file() {
                return Err(ExportError::MissingSource(asset.path.clone()));
            }
            Ok((
                *asset_id,
                SourceFingerprint {
                    path: asset.path.clone(),
                    bytes: metadata.len(),
                    modified: metadata.modified().ok(),
                },
            ))
        })
        .collect()
}

fn ensure_source_fingerprints(
    fingerprints: &BTreeMap<AssetId, SourceFingerprint>,
) -> Result<(), ExportError> {
    for fingerprint in fingerprints.values() {
        let metadata = fs::metadata(&fingerprint.path)
            .map_err(|_| ExportError::SourceChanged(fingerprint.path.clone()))?;
        if !metadata.is_file()
            || metadata.len() != fingerprint.bytes
            || metadata.modified().ok() != fingerprint.modified
        {
            return Err(ExportError::SourceChanged(fingerprint.path.clone()));
        }
    }
    Ok(())
}

/// Run a composition export on the calling thread.  This is useful for CLI
/// workflows and is also the worker body behind [`spawn_composition_export`].
pub fn export_project<F>(
    request: &CompositionExportRequest,
    control: &ExportControl,
    mut progress: F,
) -> Result<ExportReport, ExportError>
where
    F: FnMut(u64, u64),
{
    let range = request.validate()?;
    let source_fingerprints = capture_source_fingerprints(&request.project)?;
    let temp = reserve_output(&request.output)?;
    let result = if request.format == CompositionFormat::Wav {
        let result = export_audio_to_temp(request, control, range, &temp, &source_fingerprints);
        if result.is_ok() {
            progress(1, 1);
        }
        result.map(|_| 1)
    } else {
        let schedule =
            FrameSchedule::new(range, request.project.frame_rate).map_err(ExportError::Schedule)?;
        export_to_temp(
            request,
            control,
            schedule,
            &temp,
            &source_fingerprints,
            &mut progress,
        )
    };
    match result {
        Ok(frames) => {
            if let Err(error) = ensure_source_fingerprints(&source_fingerprints) {
                let _ = fs::remove_file(&temp);
                return Err(error);
            }
            // Publish with a same-directory hard link. Unlike a final
            // `rename`, this remains no-replace if another process creates
            // the destination between the preflight check and publication.
            // The temporary is removed only after the link succeeds.
            match fs::hard_link(&temp, &request.output) {
                Ok(()) => {
                    let _ = fs::remove_file(&temp);
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    let _ = fs::remove_file(&temp);
                    return Err(ExportError::DestinationExists(request.output.clone()));
                }
                Err(error) => {
                    let _ = fs::remove_file(&temp);
                    return Err(ExportError::Io(error));
                }
            }
            Ok(ExportReport {
                output: request.output.clone(),
                frames,
            })
        }
        Err(error) => {
            let _ = fs::remove_file(&temp);
            Err(error)
        }
    }
}

fn export_to_temp<F>(
    request: &CompositionExportRequest,
    control: &ExportControl,
    schedule: FrameSchedule,
    temp: &Path,
    source_fingerprints: &BTreeMap<AssetId, SourceFingerprint>,
    progress: &mut F,
) -> Result<u64, ExportError>
where
    F: FnMut(u64, u64),
{
    let media = FfmpegMediaSource::new(&request.project, request.binaries.clone())?;
    let mut command = Command::new(&request.binaries.ffmpeg);
    let fps = request.project.frame_rate;
    command
        .args(["-hide_banner", "-loglevel", "error", "-nostdin", "-y"])
        // image2pipe is used instead of the rawvideo muxer because the
        // packaged FFmpeg build intentionally omits that muxer.  Each PNG is
        // a self-delimiting frame, so the pipe can carry the whole schedule.
        .args(["-f", "image2pipe", "-vcodec", "png"])
        .args([
            "-framerate",
            &format!("{}/{}", fps.numerator, fps.denominator),
        ])
        .args(["-i", "pipe:0"])
        .args(["-frames:v", &schedule.len().to_string()])
        .args(["-an", "-f", request.format.muxer()]);
    match request.format {
        CompositionFormat::Mp4 | CompositionFormat::Mkv => {
            command.args([
                "-c:v",
                "mpeg4",
                "-q:v",
                &quality_quantizer(request.quality),
                "-pix_fmt",
                "yuv420p",
            ]);
        }
        CompositionFormat::Gif => {
            command.args(["-vf", "format=rgb24"]);
        }
        CompositionFormat::Wav => unreachable!("audio export is handled before video encoding"),
    }
    command
        .arg(temp)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    let mut child = command.spawn().map_err(ExportError::Io)?;
    let stderr = match child.stderr.take() {
        Some(stderr) => stderr,
        None => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(ExportError::Invalid(
                "FFmpeg stderr was not piped".to_owned(),
            ));
        }
    };
    let mut stderr_reader = Some(thread::spawn(move || read_bounded(stderr, 64 * 1024)));
    let mut stdin = match child.stdin.take() {
        Some(stdin) => stdin,
        None => {
            abort_encoder(&mut child, &mut stderr_reader);
            return Err(ExportError::Invalid(
                "FFmpeg stdin was not piped".to_owned(),
            ));
        }
    };
    let mut completed: u64 = 0;
    for scheduled in schedule.iter() {
        if control.is_cancelled() {
            abort_encoder(&mut child, &mut stderr_reader);
            return Err(ExportError::Cancelled);
        }
        if let Err(error) = ensure_source_fingerprints(source_fingerprints) {
            abort_encoder(&mut child, &mut stderr_reader);
            return Err(error);
        }
        let frame = match composition::render_with_options(
            &request.project,
            scheduled.time,
            &media,
            request.render_options,
        ) {
            Ok(frame) => frame,
            Err(error) => {
                abort_encoder(&mut child, &mut stderr_reader);
                return Err(error.into());
            }
        };
        let png = match encode_png(&frame) {
            Ok(png) => png,
            Err(error) => {
                abort_encoder(&mut child, &mut stderr_reader);
                return Err(error);
            }
        };
        if let Err(error) = stdin.write_all(&png) {
            abort_encoder(&mut child, &mut stderr_reader);
            return Err(ExportError::Io(error));
        }
        completed = completed.saturating_add(1);
        progress(completed, schedule.len());
    }
    drop(stdin);
    let status = match child.wait() {
        Ok(status) => status,
        Err(error) => {
            abort_encoder(&mut child, &mut stderr_reader);
            return Err(ExportError::Io(error));
        }
    };
    let diagnostics = stderr_reader
        .take()
        .expect("encoder diagnostics reader is present before join")
        .join()
        .map_err(|_| ExportError::Invalid("FFmpeg diagnostics thread panicked".to_owned()))??;
    if control.is_cancelled() {
        return Err(ExportError::Cancelled);
    }
    if !status.success() {
        let detail = String::from_utf8_lossy(&diagnostics).trim().to_owned();
        return Err(ExportError::Encoder(if detail.is_empty() {
            format!("FFmpeg exited with status {status}")
        } else {
            detail
        }));
    }
    Ok(completed)
}

fn abort_encoder(
    child: &mut std::process::Child,
    stderr_reader: &mut Option<thread::JoinHandle<std::io::Result<Vec<u8>>>>,
) {
    let _ = child.kill();
    let _ = child.wait();
    if let Some(reader) = stderr_reader.take() {
        let _ = reader.join();
    }
}

fn export_audio_to_temp(
    request: &CompositionExportRequest,
    control: &ExportControl,
    range: TimeRange,
    temp: &Path,
    source_fingerprints: &BTreeMap<AssetId, SourceFingerprint>,
) -> Result<(), ExportError> {
    if control.is_cancelled() {
        return Err(ExportError::Cancelled);
    }
    let intervals = request.project.audio_intervals(range)?;
    let mut decoded = Vec::new();
    for interval in intervals.into_iter().filter(|interval| !interval.muted) {
        if control.is_cancelled() {
            return Err(ExportError::Cancelled);
        }
        ensure_source_fingerprints(source_fingerprints)?;
        let asset = request.project.asset(interval.asset_id).ok_or_else(|| {
            ExportError::Invalid(format!("missing audio asset {}", interval.asset_id))
        })?;
        let source_duration = interval
            .source_range
            .duration()
            .map_err(|error| ExportError::Invalid(error.to_string()))?;
        let (sample_rate, channels, samples) = decode_audio_samples(
            &request.binaries,
            &asset.path,
            interval.source_range.start,
            source_duration,
            control,
        )?;
        let source_id = SourceId::new(interval.clip_id.value());
        let decoded_interval = EngineAudioInterval::new(
            interval.clip_id,
            interval.track_id,
            interval.asset_id,
            source_id,
            interval.project_range,
            interval.source_range,
            sample_rate,
            channels,
            samples,
            interval.gain,
            false,
        )
        .map_err(|error| {
            ExportError::Invalid(format!("decoded audio interval is invalid: {error}"))
        })?;
        decoded.push(decoded_interval);
    }

    let config = AudioMixConfig::new(48_000, 2, ClippingPolicy::HardClip)
        .map_err(|error| ExportError::Invalid(format!("invalid export audio layout: {error}")))?;
    let mixed = AudioMixer::new(config)
        .mix_range(&decoded, range)
        .map_err(|error| ExportError::Invalid(format!("audio mix failed: {error}")))?;
    if control.is_cancelled() {
        return Err(ExportError::Cancelled);
    }
    let bytes = encode_wav_s16le(&mixed.samples, mixed.sample_rate, mixed.channels)?;
    fs::write(temp, bytes)?;
    Ok(())
}

fn decode_audio_samples(
    binaries: &Binaries,
    path: &Path,
    source_start: Time,
    source_duration: Time,
    control: &ExportControl,
) -> Result<(u32, u16, Vec<f32>), ExportError> {
    let start_text = source_start.to_f64().max(0.0).to_string();
    let duration_text = source_duration.to_f64().max(0.0).to_string();
    let mut command = Command::new(&binaries.ffmpeg);
    command
        .args(["-hide_banner", "-loglevel", "error", "-nostdin", "-y"])
        .args(["-ss", &start_text, "-i"])
        .arg(path)
        .args([
            "-t",
            &duration_text,
            "-map",
            "0:a:0",
            "-vn",
            "-ar",
            "48000",
            "-ac",
            "2",
            "-c:a",
            "pcm_f32le",
            "-f",
            "wav",
            "pipe:1",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn().map_err(ExportError::Io)?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| ExportError::Invalid("FFmpeg stdout was not piped".to_owned()))?;
    let stdout_reader = thread::spawn(move || read_bounded(stdout, 256 * 1024 * 1024));
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| ExportError::Invalid("FFmpeg stderr was not piped".to_owned()))?;
    let stderr_reader = thread::spawn(move || read_bounded(stderr, 64 * 1024));
    let status = loop {
        if control.is_cancelled() {
            let _ = child.kill();
            let _ = child.wait();
            let _ = stdout_reader.join();
            let _ = stderr_reader.join();
            return Err(ExportError::Cancelled);
        }
        match child.try_wait().map_err(ExportError::Io)? {
            Some(status) => break status,
            None => thread::sleep(std::time::Duration::from_millis(5)),
        }
    };
    let bytes = stdout_reader
        .join()
        .map_err(|_| ExportError::Invalid("audio reader thread panicked".to_owned()))??;
    let diagnostics = stderr_reader
        .join()
        .map_err(|_| ExportError::Invalid("FFmpeg diagnostics thread panicked".to_owned()))??;
    if !status.success() {
        let detail = String::from_utf8_lossy(&diagnostics).trim().to_owned();
        return Err(ExportError::Encoder(if detail.is_empty() {
            format!("FFmpeg exited with status {status}")
        } else {
            detail
        }));
    }
    parse_wav_f32(&bytes)
}

fn parse_wav_f32(bytes: &[u8]) -> Result<(u32, u16, Vec<f32>), ExportError> {
    if bytes.len() < 12 || &bytes[..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return Err(ExportError::Encoder(
            "audio decoder returned an invalid WAV".to_owned(),
        ));
    }
    let mut offset = 12_usize;
    let mut format = None;
    let mut data = None;
    while offset.saturating_add(8) <= bytes.len() {
        let id = &bytes[offset..offset + 4];
        let payload = offset + 8;
        let encoded_size = u32::from_le_bytes(bytes[offset + 4..offset + 8].try_into().unwrap());
        // WAV written to a non-seekable pipe cannot backpatch RIFF/data
        // lengths, so FFmpeg uses the legal streaming sentinel 0xffffffff.
        let size = if encoded_size == u32::MAX && id == b"data" {
            bytes.len().saturating_sub(payload)
        } else {
            encoded_size as usize
        };
        let end = payload
            .checked_add(size)
            .ok_or_else(|| ExportError::Encoder("WAV chunk size overflowed".to_owned()))?;
        if end > bytes.len() {
            return Err(ExportError::Encoder(
                "WAV chunk exceeds decoder output".to_owned(),
            ));
        }
        match id {
            b"fmt " if size >= 16 => {
                let audio_format =
                    u16::from_le_bytes(bytes[payload..payload + 2].try_into().unwrap());
                let channels =
                    u16::from_le_bytes(bytes[payload + 2..payload + 4].try_into().unwrap());
                let sample_rate =
                    u32::from_le_bytes(bytes[payload + 4..payload + 8].try_into().unwrap());
                let bits =
                    u16::from_le_bytes(bytes[payload + 14..payload + 16].try_into().unwrap());
                format = Some((audio_format, channels, sample_rate, bits));
            }
            b"data" => data = Some(&bytes[payload..end]),
            _ => {}
        }
        offset = end
            .checked_add(size & 1)
            .ok_or_else(|| ExportError::Encoder("WAV chunk offset overflowed".to_owned()))?;
    }
    let (audio_format, channels, sample_rate, bits) =
        format.ok_or_else(|| ExportError::Encoder("WAV has no format chunk".to_owned()))?;
    if audio_format != 3 || bits != 32 || channels == 0 || sample_rate == 0 {
        return Err(ExportError::Encoder(
            "audio decoder did not return 32-bit float PCM".to_owned(),
        ));
    }
    let data = data.ok_or_else(|| ExportError::Encoder("WAV has no data chunk".to_owned()))?;
    if data.len() % 4 != 0 || data.len() / 4 % usize::from(channels) != 0 {
        return Err(ExportError::Encoder(
            "WAV sample data has an invalid layout".to_owned(),
        ));
    }
    let mut samples = Vec::with_capacity(data.len() / 4);
    for sample in data.chunks(4) {
        let sample = <[u8; 4]>::try_from(sample)
            .map_err(|_| ExportError::Encoder("WAV sample data is truncated".to_owned()))?;
        samples.push(f32::from_le_bytes(sample));
    }
    Ok((sample_rate, channels, samples))
}

fn encode_wav_s16le(
    samples: &[f32],
    sample_rate: u32,
    channels: u16,
) -> Result<Vec<u8>, ExportError> {
    if sample_rate == 0 || channels == 0 || !samples.len().is_multiple_of(usize::from(channels)) {
        return Err(ExportError::Encoder(
            "cannot encode invalid PCM layout".to_owned(),
        ));
    }
    let data_len = samples
        .len()
        .checked_mul(2)
        .ok_or_else(|| ExportError::Encoder("WAV sample data is too large".to_owned()))?;
    let data_len_u32 = u32::try_from(data_len)
        .map_err(|_| ExportError::Encoder("WAV sample data exceeds the RIFF limit".to_owned()))?;
    let riff_len = 36_u32
        .checked_add(data_len_u32)
        .ok_or_else(|| ExportError::Encoder("WAV file exceeds the RIFF limit".to_owned()))?;
    let mut bytes = Vec::with_capacity(44 + data_len);
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&riff_len.to_le_bytes());
    bytes.extend_from_slice(b"WAVEfmt ");
    bytes.extend_from_slice(&16_u32.to_le_bytes());
    bytes.extend_from_slice(&1_u16.to_le_bytes());
    bytes.extend_from_slice(&channels.to_le_bytes());
    bytes.extend_from_slice(&sample_rate.to_le_bytes());
    let byte_rate = sample_rate
        .checked_mul(u32::from(channels))
        .and_then(|value| value.checked_mul(2))
        .ok_or_else(|| ExportError::Encoder("WAV byte rate overflowed".to_owned()))?;
    bytes.extend_from_slice(&byte_rate.to_le_bytes());
    let block_align = channels
        .checked_mul(2)
        .ok_or_else(|| ExportError::Encoder("WAV block alignment overflowed".to_owned()))?;
    bytes.extend_from_slice(&block_align.to_le_bytes());
    bytes.extend_from_slice(&16_u16.to_le_bytes());
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&data_len_u32.to_le_bytes());
    for &sample in samples {
        let sample = if sample.is_finite() { sample } else { 0.0 };
        let sample = sample.clamp(-1.0, 1.0);
        let pcm = if sample < 0.0 {
            (sample * 32_768.0).round() as i16
        } else {
            (sample * 32_767.0).round() as i16
        };
        bytes.extend_from_slice(&pcm.to_le_bytes());
    }
    Ok(bytes)
}

fn encode_png(frame: &RgbaFrame) -> Result<Vec<u8>, ExportError> {
    let mut bytes = Vec::new();
    {
        let mut encoder = png::Encoder::new(Cursor::new(&mut bytes), frame.width(), frame.height());
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder
            .write_header()
            .map_err(|error| ExportError::Encoder(format!("PNG header encode failed: {error}")))?;
        writer
            .write_image_data(frame.pixels())
            .map_err(|error| ExportError::Encoder(format!("PNG frame encode failed: {error}")))?;
    }
    Ok(bytes)
}

fn quality_quantizer(quality: u8) -> String {
    let quality = quality.clamp(1, 100);
    // MPEG-4's qscale is inverse quality.  Keep the mapping bounded and
    // deterministic; the UI's 50..100 range therefore maps to qscale 5..1.
    let quantizer = 1 + u32::from(100 - quality) * 4 / 99;
    quantizer.to_string()
}

fn reserve_output(output: &Path) -> Result<PathBuf, ExportError> {
    let parent = output
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;
    if fs::symlink_metadata(output).is_ok() {
        return Err(ExportError::DestinationExists(output.to_owned()));
    }
    let name = output
        .file_name()
        .ok_or_else(|| ExportError::Invalid("export output has no filename".to_owned()))?
        .to_string_lossy();
    for attempt in 0..100_u32 {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let temp = parent.join(format!(".{name}.slicer-{stamp}-{attempt}.tmp"));
        match OpenOptions::new().write(true).create_new(true).open(&temp) {
            Ok(_) => return Ok(temp),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(ExportError::Io(error)),
        }
    }
    Err(ExportError::Invalid(
        "could not reserve a temporary export path".to_owned(),
    ))
}

fn read_bounded<R: Read>(mut reader: R, limit: usize) -> std::io::Result<Vec<u8>> {
    let mut output = Vec::with_capacity(limit.min(4096));
    let mut buffer = [0_u8; 8192];
    loop {
        let count = reader.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        if output.len().saturating_add(count) > limit {
            return Err(std::io::Error::other(
                "FFmpeg diagnostics exceeded the limit",
            ));
        }
        output.extend_from_slice(&buffer[..count]);
    }
    Ok(output)
}

/// FFmpeg-backed source provider for project assets.  The small mutex cache
/// avoids decoding the same asset/time twice when two clips reference it.
pub struct FfmpegMediaSource {
    decoder: SoftwareDecoder,
    assets: BTreeMap<AssetId, PathBuf>,
    cache: Mutex<BTreeMap<(AssetId, Time), RgbaFrame>>,
    cancel: AtomicBool,
}

impl FfmpegMediaSource {
    pub fn new(project: &Project, binaries: Binaries) -> Result<Self, ExportError> {
        let decoder = SoftwareDecoder::new(binaries).map_err(ExportError::Tool)?;
        let assets = project
            .assets
            .iter()
            .filter_map(|(id, asset)| match asset.metadata {
                AssetMetadata::Video(_) | AssetMetadata::Image(_) => {
                    Some((*id, asset.path.clone()))
                }
                AssetMetadata::Audio(_) => None,
            })
            .collect();
        Ok(Self {
            decoder,
            assets,
            cache: Mutex::new(BTreeMap::new()),
            cancel: AtomicBool::new(false),
        })
    }

    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Release);
    }
}

impl MediaSource for FfmpegMediaSource {
    fn frame(
        &self,
        asset_id: AssetId,
        source_time: Time,
    ) -> Result<Option<RgbaFrame>, CompositionError> {
        if self.cancel.load(Ordering::Acquire) {
            return Err(CompositionError::InvalidGeometry(
                "media decode cancelled".to_owned(),
            ));
        }
        if let Some(frame) = self
            .cache
            .lock()
            .ok()
            .and_then(|cache| cache.get(&(asset_id, source_time)).cloned())
        {
            return Ok(Some(frame));
        }
        let Some(path) = self.assets.get(&asset_id) else {
            return Err(CompositionError::MissingMedia(asset_id));
        };
        let request = DecodeRequest {
            path: path.clone(),
            timestamp: source_time.to_f64().max(0.0),
            width: None,
            height: None,
            generation: 0,
        };
        let lease = self
            .decoder
            .decode(&request, &self.cancel)
            .map_err(|error| {
                CompositionError::InvalidGeometry(format!("decode asset {asset_id}: {error:#}"))
            })?;
        let frame = RgbaFrame::from_rgba8(
            lease.width,
            lease.height,
            lease.pixels().to_vec(),
            FrameLimits::default(),
        )?;
        if let Ok(mut cache) = self.cache.lock() {
            cache.insert((asset_id, source_time), frame.clone());
        }
        Ok(Some(frame))
    }
}

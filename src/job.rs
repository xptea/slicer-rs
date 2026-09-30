//! Background FFmpeg jobs.
//!
//! A job owns a same-directory temporary file and only publishes it after
//! FFmpeg exits successfully.  Publishing uses a hard link, which gives us a
//! no-replace commit on the filesystems supported by Rust's standard library:
//! a file that appeared at the destination meanwhile causes the commit to
//! fail instead of overwriting it.

use crate::media::Binaries;
use anyhow::{Context, Result, anyhow, bail};
use serde_json::Value;
use std::ffi::OsString;
use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const MAX_STDERR_BYTES: usize = 64 * 1024;

/// Trim mode used by an export.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrimMode {
    /// Copy encoded packets.  The cut may move to a nearby keyframe.
    Fast,
    /// Decode and encode the selected interval for exact boundaries.
    Exact,
}

/// Container/codec target for an export.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OutputFormat {
    Mp4,
    Mkv,
    Webm,
    Mp3,
    Wav,
    Gif,
}

/// Pixel rectangle to retain from a video frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CropRect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

impl OutputFormat {
    /// The FFmpeg muxer name used for this format.
    pub fn muxer(self) -> &'static str {
        match self {
            Self::Mp4 => "mp4",
            Self::Mkv => "matroska",
            Self::Webm => "webm",
            Self::Mp3 => "mp3",
            Self::Wav => "wav",
            Self::Gif => "gif",
        }
    }

    /// A conventional filename extension for the format.
    pub fn extension(self) -> &'static str {
        match self {
            Self::Mp4 => "mp4",
            Self::Mkv => "mkv",
            Self::Webm => "webm",
            Self::Mp3 => "mp3",
            Self::Wav => "wav",
            Self::Gif => "gif",
        }
    }
}

/// User-selected input, interval, and output settings.
#[derive(Clone, Debug)]
pub struct ExportRequest {
    pub input: PathBuf,
    pub output: PathBuf,
    pub start: f64,
    pub end: f64,
    pub mode: TrimMode,
    pub format: OutputFormat,
    /// Optional video crop in source pixels. A crop always uses the exact
    /// decode/re-encode path, even when the requested trim mode is Fast.
    pub crop: Option<CropRect>,
    /// Quality from 0 (lowest) through 100 (highest). Values above 100 are
    /// clamped so callers receiving an untrusted UI value cannot make an
    /// invalid FFmpeg command.
    pub quality: u8,
    /// Whether the export should contain no source audio. GIF exports are
    /// silent by definition; this flag also controls video and WAV exports.
    pub mute_audio: bool,
}

/// Events emitted by a background export.
#[derive(Clone, Debug, PartialEq)]
pub enum JobEvent {
    /// A value in the inclusive range 0.0 through 1.0.
    Progress(f64),
    Completed(PathBuf),
    Cancelled,
    Failed(String),
}

/// Estimate from measured encoding progress, never from an assumed encode speed.
pub fn estimated_remaining(progress: f64, elapsed: Duration) -> Option<Duration> {
    if !progress.is_finite() || !(0.005..1.0).contains(&progress) || elapsed.as_secs_f64() < 0.5 {
        return None;
    }
    Duration::try_from_secs_f64(elapsed.as_secs_f64() * (1.0 - progress) / progress).ok()
}

/// Handle for a running export.
pub struct JobHandle {
    /// Receive progress and terminal events. Exactly one terminal event is
    /// emitted for a job unless the receiver is dropped first.
    pub events: mpsc::Receiver<JobEvent>,
    cancel_requested: Arc<AtomicBool>,
    child: Arc<Mutex<Option<Child>>>,
}

impl JobHandle {
    /// Validate an export, reserve its temporary output, and start FFmpeg on a
    /// worker thread. The original input is never passed as an output path.
    pub fn spawn(binaries: Binaries, request: ExportRequest) -> Result<Self> {
        binaries.validate()?;
        validate_request(&request)?;

        if request.crop.is_some() {
            if matches!(request.format, OutputFormat::Mp3 | OutputFormat::Wav) {
                bail!("video crop is unavailable for audio-only exports");
            }
            if request.format == OutputFormat::Webm {
                bail!(
                    "cropped WebM export is unavailable in the bundled FFmpeg build; choose MP4 or MKV"
                );
            }
        }

        if request.mode == TrimMode::Exact && request.format == OutputFormat::Webm {
            bail!(
                "exact WebM export is unavailable in the bundled FFmpeg build; choose Fast mode or another format"
            );
        }
        if request.format == OutputFormat::Mp3 {
            bail!(
                "MP3 export is unavailable in the bundled FFmpeg build because it has no MP3 encoder"
            );
        }

        let prepared = PreparedRequest::new(request)?;
        let (sender, events) = mpsc::channel();
        let cancel_requested = Arc::new(AtomicBool::new(false));
        let child = Arc::new(Mutex::new(None));

        let worker_cancel = Arc::clone(&cancel_requested);
        let worker_child = Arc::clone(&child);
        let worker_binaries = binaries;
        let temp_for_spawn = prepared.temp.clone();
        let worker = thread::Builder::new()
            .name("slicer-ffmpeg".to_owned())
            .spawn(move || {
                run_export(
                    worker_binaries,
                    prepared,
                    sender,
                    worker_cancel,
                    worker_child,
                )
            });

        if let Err(error) = worker {
            // The temporary file was reserved before starting the worker, so
            // clean it up if the OS refuses to create the thread.
            let _ = fs::remove_file(&temp_for_spawn);
            return Err(error).context("failed to start FFmpeg worker thread");
        }

        Ok(Self {
            events,
            cancel_requested,
            child,
        })
    }

    /// Request cancellation. The worker kills FFmpeg and removes its
    /// incomplete temporary output before sending `Cancelled`.
    pub fn cancel(&self) {
        self.cancel_requested.store(true, Ordering::SeqCst);
        if let Ok(mut guard) = self.child.lock()
            && let Some(child) = guard.as_mut()
        {
            let _ = child.kill();
        }
    }
}

impl Drop for JobHandle {
    fn drop(&mut self) {
        // A dropped handle represents a closed/cancelled UI job.  Killing the
        // process here also prevents an orphaned worker from publishing an
        // output after the caller has stopped observing its events.
        self.cancel();
    }
}

struct PreparedRequest {
    source_bitrates: Option<SourceBitrates>,
    request: ExportRequest,
    input: PathBuf,
    output: PathBuf,
    temp: PathBuf,
}

impl PreparedRequest {
    fn new(request: ExportRequest) -> Result<Self> {
        let input = fs::canonicalize(&request.input).with_context(|| {
            format!("unable to resolve export input {}", request.input.display())
        })?;
        let input_metadata = fs::metadata(&input)
            .with_context(|| format!("unable to read export input {}", input.display()))?;
        if !input_metadata.is_file() {
            bail!("export input is not a regular file: {}", input.display());
        }

        let output_name = request.output.file_name().ok_or_else(|| {
            anyhow!(
                "export output has no filename: {}",
                request.output.display()
            )
        })?;
        if output_name.is_empty() || output_name == "." || output_name == ".." {
            bail!(
                "export output has an invalid filename: {}",
                request.output.display()
            );
        }

        let requested_parent = request
            .output
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        let output_parent = fs::canonicalize(requested_parent).with_context(|| {
            format!(
                "unable to resolve export output directory {}",
                requested_parent.display()
            )
        })?;
        let output = output_parent.join(output_name);

        // symlink_metadata catches dangling symlinks as well as regular files.
        match fs::symlink_metadata(&output) {
            Ok(_) => bail!(
                "refusing to overwrite existing export output {}",
                output.display()
            ),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error).with_context(|| {
                    format!("unable to inspect export output {}", output.display())
                });
            }
        }

        if paths_refer_to_same_file(&input, &output) {
            bail!("export output must differ from the input file");
        }

        let temp = reserve_temp(&output_parent, output_name)?;
        Ok(Self {
            source_bitrates: None,
            request,
            input,
            output,
            temp,
        })
    }
}

fn validate_request(request: &ExportRequest) -> Result<()> {
    if !request.start.is_finite() || !request.end.is_finite() {
        bail!("export start and end must be finite timestamps");
    }
    if request.start < 0.0 {
        bail!("export start must be non-negative");
    }
    if request.end <= request.start {
        bail!("export end must be greater than export start");
    }
    if request.input.as_os_str().is_empty() {
        bail!("export input path is empty");
    }
    if request.output.as_os_str().is_empty() {
        bail!("export output path is empty");
    }
    if let Some(crop) = request.crop {
        validate_crop_shape_for_format(crop, request.format)?;
    }
    Ok(())
}

fn validate_crop_shape_for_format(crop: CropRect, format: OutputFormat) -> Result<()> {
    if format == OutputFormat::Gif {
        if crop.width == 0 || crop.height == 0 {
            bail!("crop width and height must be positive");
        }
        // GIF frames are palette-based RGB images and do not have the even
        // dimension requirement imposed by YUV video encoders.
        return Ok(());
    }
    validate_crop_shape(crop)
}

fn validate_crop_shape(crop: CropRect) -> Result<()> {
    if crop.width == 0 || crop.height == 0 {
        bail!("crop width and height must be positive");
    }
    if !crop.width.is_multiple_of(2) || !crop.height.is_multiple_of(2) {
        bail!("crop width and height must be even for YUV video");
    }
    Ok(())
}

#[cfg(test)]
fn validate_crop_bounds(crop: CropRect, source_width: u32, source_height: u32) -> Result<()> {
    validate_crop_shape(crop)?;
    validate_crop_bounds_only(crop, source_width, source_height)
}

fn validate_crop_bounds_for_format(
    crop: CropRect,
    source_width: u32,
    source_height: u32,
    format: OutputFormat,
) -> Result<()> {
    validate_crop_shape_for_format(crop, format)?;
    validate_crop_bounds_only(crop, source_width, source_height)
}

fn validate_crop_bounds_only(crop: CropRect, source_width: u32, source_height: u32) -> Result<()> {
    let right = crop
        .x
        .checked_add(crop.width)
        .ok_or_else(|| anyhow!("crop extends beyond the source width"))?;
    let bottom = crop
        .y
        .checked_add(crop.height)
        .ok_or_else(|| anyhow!("crop extends beyond the source height"))?;
    if right > source_width || bottom > source_height {
        bail!(
            "crop rectangle {}x{}+{}+{} exceeds source dimensions {}x{}",
            crop.width,
            crop.height,
            crop.x,
            crop.y,
            source_width,
            source_height
        );
    }
    Ok(())
}

#[derive(Clone, Copy)]
struct SourceBitrates {
    video: u64,
    audio: u64,
}

impl Default for SourceBitrates {
    fn default() -> Self {
        Self {
            video: 2_000_000,
            audio: 128_000,
        }
    }
}

fn probe_source_bitrates(binaries: &Binaries, input: &Path) -> Option<SourceBitrates> {
    let output = Command::new(&binaries.ffprobe)
        .args([
            "-v",
            "error",
            "-show_entries",
            "stream=codec_type,bit_rate:format=duration,size,bit_rate",
            "-of",
            "json",
            "--",
        ])
        .arg(input)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let document: Value = serde_json::from_slice(&output.stdout).ok()?;
    source_bitrates(&document)
}

fn source_bitrates(document: &Value) -> Option<SourceBitrates> {
    let positive = |value: Option<&Value>| probe_f64(value).filter(|n| n.is_finite() && *n > 0.0);
    let streams = document.get("streams")?.as_array()?;
    let video = streams.iter().find(|s| s["codec_type"] == "video")?;
    let audio_rates: Vec<_> = streams
        .iter()
        .filter(|s| s["codec_type"] == "audio")
        .filter_map(|s| positive(s.get("bit_rate")))
        .collect();
    let format = &document["format"];
    let total_rate = positive(format.get("bit_rate"))
        .or_else(|| Some(positive(format.get("size"))? * 8.0 / positive(format.get("duration"))?));
    let video_rate = positive(video.get("bit_rate")).or_else(|| {
        // Matroska often omits stream bitrates. Use the file's average budget,
        // reserving known audio rates (or 128 kb/s per unknown audio track).
        let audio_count = streams
            .iter()
            .filter(|s| s["codec_type"] == "audio")
            .count();
        let audio_total =
            audio_rates.iter().sum::<f64>() + (audio_count - audio_rates.len()) as f64 * 128_000.0;
        Some((total_rate? - audio_total).max(total_rate? * 0.25))
    })?;
    Some(SourceBitrates {
        video: (video_rate as u64).clamp(32_000, 200_000_000),
        audio: (audio_rates.first().copied().unwrap_or(128_000.0) as u64).clamp(32_000, 192_000),
    })
}

const MAX_CROP_PROBE_DIAGNOSTIC_BYTES: usize = 16 * 1024;

fn validate_crop_for_input(binaries: &Binaries, prepared: &PreparedRequest) -> Result<()> {
    let Some(crop) = prepared.request.crop else {
        return Ok(());
    };
    let output = Command::new(&binaries.ffprobe)
        .args([
            "-v",
            "error",
            "-select_streams",
            "v:0",
            "-show_streams",
            "-of",
            "json",
            "--",
        ])
        .arg(&prepared.input)
        .output()
        .with_context(|| {
            format!(
                "failed to launch ffprobe for crop dimensions of {}",
                prepared.input.display()
            )
        })?;
    if !output.status.success() {
        let detail = bounded_text(&output.stderr, MAX_CROP_PROBE_DIAGNOSTIC_BYTES);
        if detail.is_empty() {
            bail!(
                "ffprobe could not read crop dimensions for {} (status {})",
                prepared.input.display(),
                output.status
            );
        }
        bail!(
            "ffprobe could not read crop dimensions for {} (status {}): {}",
            prepared.input.display(),
            output.status,
            detail
        );
    }

    let document: Value = serde_json::from_slice(&output.stdout).with_context(|| {
        format!(
            "ffprobe returned invalid crop metadata for {}",
            prepared.input.display()
        )
    })?;
    let stream = document
        .get("streams")
        .and_then(Value::as_array)
        .and_then(|streams| streams.first())
        .ok_or_else(|| anyhow!("video crop requires a video stream"))?;

    if let Some(rotation) = crop_rotation(stream)? {
        let normalized = rotation.rem_euclid(360.0);
        if normalized > 0.5 && normalized < 359.5 {
            bail!(
                "cropping video with rotation metadata ({rotation:.2} degrees) is unsupported; rotate or normalize the source first"
            );
        }
    }

    let width = probe_u32(stream.get("width"))
        .ok_or_else(|| anyhow!("video crop requires a known source width"))?;
    let height = probe_u32(stream.get("height"))
        .ok_or_else(|| anyhow!("video crop requires a known source height"))?;
    validate_crop_bounds_for_format(crop, width, height, prepared.request.format)
}

fn probe_u32(value: Option<&Value>) -> Option<u32> {
    match value? {
        Value::Number(number) => number.as_u64().and_then(|value| u32::try_from(value).ok()),
        Value::String(value) => value.parse::<u32>().ok(),
        _ => None,
    }
}

fn probe_f64(value: Option<&Value>) -> Option<f64> {
    match value? {
        Value::Number(number) => number.as_f64(),
        Value::String(value) => value.parse::<f64>().ok(),
        _ => None,
    }
}

fn crop_rotation(stream: &Value) -> Result<Option<f64>> {
    let tag_rotation = stream
        .get("tags")
        .and_then(Value::as_object)
        .and_then(|tags| tags.get("rotate"));
    let mut rotation =
        match tag_rotation {
            Some(value) => Some(probe_f64(Some(value)).ok_or_else(|| {
                anyhow!("crop cannot safely interpret the source rotation metadata")
            })?),
            None => None,
        };
    let mut unresolved_display_matrix = false;
    if let Some(side_data) = stream.get("side_data_list").and_then(Value::as_array) {
        for entry in side_data {
            if let Some(value) = entry.get("rotation") {
                rotation = Some(probe_f64(Some(value)).ok_or_else(|| {
                    anyhow!("crop cannot safely interpret the source rotation metadata")
                })?);
            } else if entry
                .get("side_data_type")
                .and_then(Value::as_str)
                .is_some_and(|kind| kind.eq_ignore_ascii_case("Display Matrix"))
                || entry.get("displaymatrix").is_some()
            {
                unresolved_display_matrix = true;
            }
        }
    }
    if unresolved_display_matrix && rotation.is_none() {
        bail!(
            "cropping video with an unrecognized display matrix is unsupported; rotate or normalize the source first"
        );
    }
    Ok(rotation)
}

fn reserve_temp(parent: &Path, output_name: &std::ffi::OsStr) -> Result<PathBuf> {
    static COUNTER: AtomicU64 = AtomicU64::new(0);

    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    let pid = std::process::id();
    let output_name = output_name.to_string_lossy();

    for _ in 0..128 {
        let counter = COUNTER.fetch_add(1, Ordering::Relaxed);
        let filename = format!(".{output_name}.slicer-{pid}-{stamp}-{counter}.part");
        let path = parent.join(filename);
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(file) => {
                drop(file);
                return Ok(path);
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(error).with_context(|| {
                    format!("unable to reserve temporary export {}", path.display())
                });
            }
        }
    }

    bail!(
        "unable to reserve a unique temporary export beside {}",
        parent.display()
    )
}

fn paths_refer_to_same_file(input: &Path, output: &Path) -> bool {
    if input == output {
        return true;
    }

    #[cfg(windows)]
    {
        input
            .to_string_lossy()
            .eq_ignore_ascii_case(&output.to_string_lossy())
    }
    #[cfg(not(windows))]
    {
        false
    }
}

/// The subset of FFmpeg's runtime capabilities used by GIF export.  The
/// encoder and muxer are required; palette filters improve color quality but
/// are optional so a compatible FFmpeg can still produce a valid GIF.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct GifCapabilities {
    encoder: bool,
    muxer: bool,
    fps_filter: bool,
    split_filter: bool,
    palettegen_filter: bool,
    paletteuse_filter: bool,
}

impl GifCapabilities {
    fn use_palette(self) -> bool {
        self.split_filter && self.palettegen_filter && self.paletteuse_filter
    }

    fn ensure_usable(self) -> Result<()> {
        if !self.encoder || !self.muxer {
            bail!(
                "GIF export is unavailable in the bundled FFmpeg build; rebuild the bundle with the GIF encoder and muxer"
            );
        }
        Ok(())
    }
}

fn probe_gif_capabilities(binaries: &Binaries) -> Result<GifCapabilities> {
    let encoders = ffmpeg_listing(binaries, "-encoders")?;
    let muxers = ffmpeg_listing(binaries, "-muxers")?;
    let filters = ffmpeg_listing(binaries, "-filters")?;
    Ok(GifCapabilities {
        encoder: listing_has_name(&encoders, "gif"),
        muxer: listing_has_name(&muxers, "gif"),
        fps_filter: listing_has_name(&filters, "fps"),
        split_filter: listing_has_name(&filters, "split"),
        palettegen_filter: listing_has_name(&filters, "palettegen"),
        paletteuse_filter: listing_has_name(&filters, "paletteuse"),
    })
}

fn ensure_filter_available(binaries: &Binaries, filter: &str) -> Result<()> {
    let filters = ffmpeg_listing(binaries, "-filters")?;
    if !listing_has_name(&filters, filter) {
        bail!(
            "muted WAV export requires the bundled FFmpeg {} filter; rebuild the bundle with --enable-filter={filter}",
            filter
        );
    }
    Ok(())
}

fn ffmpeg_listing(binaries: &Binaries, listing: &str) -> Result<String> {
    let output = Command::new(&binaries.ffmpeg)
        .args(["-hide_banner", listing])
        .output()
        .with_context(|| format!("failed to query bundled FFmpeg {listing}"))?;
    if !output.status.success() {
        let detail = bounded_text(&output.stderr, MAX_CROP_PROBE_DIAGNOSTIC_BYTES);
        if detail.is_empty() {
            bail!(
                "bundled FFmpeg capability query {listing} failed with status {}",
                output.status
            );
        }
        bail!(
            "bundled FFmpeg capability query {listing} failed with status {}: {detail}",
            output.status
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn listing_has_name(listing: &str, wanted: &str) -> bool {
    listing
        .lines()
        .any(|line| line.split_whitespace().any(|token| token == wanted))
}

fn run_export(
    binaries: Binaries,
    mut prepared: PreparedRequest,
    sender: mpsc::Sender<JobEvent>,
    cancel_requested: Arc<AtomicBool>,
    child_slot: Arc<Mutex<Option<Child>>>,
) {
    if cancel_requested.load(Ordering::SeqCst) {
        cleanup_temp(&prepared.temp);
        send_event(&sender, JobEvent::Cancelled);
        return;
    }

    // Probe the actual executable on the worker rather than trusting the
    // build profile.  Capability queries can launch a process and must not
    // block the UI thread while a job is being queued.
    let gif_capabilities = if prepared.request.format == OutputFormat::Gif {
        let capabilities = match probe_gif_capabilities(&binaries) {
            Ok(capabilities) => capabilities,
            Err(error) => {
                cleanup_temp(&prepared.temp);
                send_event(&sender, JobEvent::Failed(format!("{error:#}")));
                return;
            }
        };
        if let Err(error) = capabilities.ensure_usable() {
            cleanup_temp(&prepared.temp);
            send_event(&sender, JobEvent::Failed(format!("{error:#}")));
            return;
        }
        Some(capabilities)
    } else {
        None
    };
    if prepared.request.format == OutputFormat::Wav
        && prepared.request.mute_audio
        && let Err(error) = ensure_filter_available(&binaries, "anullsrc")
    {
        cleanup_temp(&prepared.temp);
        send_event(&sender, JobEvent::Failed(format!("{error:#}")));
        return;
    }

    if prepared.request.crop.is_some() {
        if let Err(error) = validate_crop_for_input(&binaries, &prepared) {
            cleanup_temp(&prepared.temp);
            send_event(&sender, JobEvent::Failed(format!("{error:#}")));
            return;
        }
        // A crop probe can take long enough for the user to cancel. Check
        // again before constructing or launching FFmpeg.
        if cancel_requested.load(Ordering::SeqCst) {
            cleanup_temp(&prepared.temp);
            send_event(&sender, JobEvent::Cancelled);
            return;
        }
    }

    if matches!(
        prepared.request.format,
        OutputFormat::Mp4 | OutputFormat::Mkv
    ) && (prepared.request.mode == TrimMode::Exact || prepared.request.crop.is_some())
    {
        // Keep metadata probing on this worker, with no UI-thread process work.
        prepared.source_bitrates = probe_source_bitrates(&binaries, &prepared.input);
    }
    if cancel_requested.load(Ordering::SeqCst) {
        cleanup_temp(&prepared.temp);
        send_event(&sender, JobEvent::Cancelled);
        return;
    }

    let args = match build_args_with_gif_capabilities(&prepared, gif_capabilities) {
        Ok(args) => args,
        Err(error) => {
            cleanup_temp(&prepared.temp);
            send_event(&sender, JobEvent::Failed(error.to_string()));
            return;
        }
    };

    let mut command = Command::new(&binaries.ffmpeg);
    command
        .args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut process = match command.spawn() {
        Ok(process) => process,
        Err(error) => {
            cleanup_temp(&prepared.temp);
            send_event(
                &sender,
                JobEvent::Failed(format!("failed to launch ffmpeg: {error}")),
            );
            return;
        }
    };

    let stdout = process.stdout.take();
    let stderr = process.stderr.take();
    let stderr_reader =
        stderr.map(|stderr| thread::spawn(move || read_bounded(stderr, MAX_STDERR_BYTES)));

    // Publish the child before reading progress so cancel() can kill it even
    // when FFmpeg is blocked opening a damaged input.
    if let Ok(mut guard) = child_slot.lock() {
        *guard = Some(process);
        // cancel() may have set the flag while spawn() was in flight. Check it
        // while holding the same mutex used by cancel() so that this race
        // cannot leave a newly spawned FFmpeg process running.
        if cancel_requested.load(Ordering::SeqCst)
            && let Some(child) = guard.as_mut()
        {
            let _ = child.kill();
        }
    } else {
        let _ = process.kill();
        let _ = process.wait();
        cleanup_temp(&prepared.temp);
        send_event(
            &sender,
            JobEvent::Failed("FFmpeg process state was poisoned".to_owned()),
        );
        return;
    }

    send_event(&sender, JobEvent::Progress(0.0));
    let mut progress_error = None;
    if let Some(stdout) = stdout {
        let lines = BufReader::new(stdout).lines();
        let total = prepared.request.end - prepared.request.start;
        let mut last_progress = 0.0;
        for line in lines {
            match line {
                Ok(line) => {
                    if let Some(seconds) = progress_seconds(&line) {
                        // Reserve 100% for successful publication of the final file.
                        let progress = (seconds / total).clamp(0.0, 0.99);
                        if progress >= 1.0 || progress - last_progress >= 0.005 {
                            last_progress = progress;
                            send_event(&sender, JobEvent::Progress(progress));
                        }
                    }
                }
                Err(error) => {
                    progress_error = Some(format!("failed to read ffmpeg progress: {error}"));
                    if let Ok(mut guard) = child_slot.lock()
                        && let Some(child) = guard.as_mut()
                    {
                        let _ = child.kill();
                    }
                    break;
                }
            }
        }
    }

    let status = match child_slot.lock() {
        Ok(mut guard) => guard.take().map(|mut child| child.wait()),
        Err(_) => Some(Err(std::io::Error::other(
            "FFmpeg process state was poisoned",
        ))),
    };
    let stderr = stderr_reader
        .and_then(|reader| reader.join().ok())
        .unwrap_or_default();

    if cancel_requested.load(Ordering::SeqCst) {
        cleanup_temp(&prepared.temp);
        send_event(&sender, JobEvent::Cancelled);
        return;
    }

    if let Some(error) = progress_error {
        cleanup_temp(&prepared.temp);
        send_event(&sender, JobEvent::Failed(error));
        return;
    }

    let status = match status {
        Some(Ok(status)) => status,
        Some(Err(error)) => {
            cleanup_temp(&prepared.temp);
            send_event(
                &sender,
                JobEvent::Failed(format!("failed waiting for ffmpeg: {error}")),
            );
            return;
        }
        None => {
            cleanup_temp(&prepared.temp);
            send_event(
                &sender,
                JobEvent::Failed("FFmpeg process state was lost".to_owned()),
            );
            return;
        }
    };

    if !status.success() {
        cleanup_temp(&prepared.temp);
        let detail = bounded_text(&stderr, MAX_STDERR_BYTES);
        let message = if detail.is_empty() {
            format!("ffmpeg exited with status {status}")
        } else {
            format!("ffmpeg exited with status {status}: {detail}")
        };
        send_event(&sender, JobEvent::Failed(message));
        return;
    }

    if cancel_requested.load(Ordering::SeqCst) {
        cleanup_temp(&prepared.temp);
        send_event(&sender, JobEvent::Cancelled);
        return;
    }

    match fs::metadata(&prepared.temp) {
        Ok(metadata) if metadata.is_file() && metadata.len() > 0 => {}
        Ok(_) => {
            cleanup_temp(&prepared.temp);
            send_event(
                &sender,
                JobEvent::Failed("ffmpeg produced an empty output".to_owned()),
            );
            return;
        }
        Err(error) => {
            cleanup_temp(&prepared.temp);
            send_event(
                &sender,
                JobEvent::Failed(format!("ffmpeg output is missing: {error}")),
            );
            return;
        }
    }

    // hard_link is an atomic create-without-replace operation when source and
    // destination are in the same directory. It avoids rename() overwriting a
    // destination created by another process after our initial preflight.
    if let Err(error) = fs::hard_link(&prepared.temp, &prepared.output) {
        cleanup_temp(&prepared.temp);
        let detail = if error.kind() == std::io::ErrorKind::AlreadyExists {
            format!(
                "destination {} appeared while the export was running",
                prepared.output.display()
            )
        } else {
            format!(
                "the destination filesystem may not support same-directory hard-link commits: {error}"
            )
        };
        send_event(
            &sender,
            JobEvent::Failed(format!(
                "unable to publish export {} safely without overwriting it: {detail}",
                prepared.output.display()
            )),
        );
        return;
    }
    cleanup_temp(&prepared.temp);
    send_event(&sender, JobEvent::Progress(1.0));
    send_event(&sender, JobEvent::Completed(prepared.output));
}

#[cfg(test)]
fn build_args(prepared: &PreparedRequest) -> Result<Vec<OsString>> {
    build_args_with_gif_capabilities(prepared, None)
}

fn build_args_with_gif_capabilities(
    prepared: &PreparedRequest,
    gif_capabilities: Option<GifCapabilities>,
) -> Result<Vec<OsString>> {
    let request = &prepared.request;
    let is_gif = request.format == OutputFormat::Gif;
    let silent_wav = request.format == OutputFormat::Wav && request.mute_audio;
    // GIF is always decoded and encoded so its frame boundaries are accurate,
    // even when the caller selected Fast mode. A crop has the same property.
    let reencode = request.mode == TrimMode::Exact || request.crop.is_some() || is_gif;
    let mut args = Vec::with_capacity(32);
    args.extend(
        ["-hide_banner", "-nostdin", "-loglevel", "error", "-nostats"]
            .into_iter()
            .map(OsString::from),
    );
    // The bundled GIF palette pipeline supports the same progress protocol as
    // video exports. A short reporting interval keeps small cuts responsive.
    args.extend(
        ["-stats_period", "0.2", "-progress", "pipe:1"]
            .into_iter()
            .map(OsString::from),
    );

    if silent_wav {
        // There may be no source audio at all, so synthesize a deterministic
        // stereo stream and trim it to the requested duration. The source
        // path was still canonicalized and validated by PreparedRequest.
        args.extend([
            OsString::from("-f"),
            OsString::from("lavfi"),
            OsString::from("-i"),
            OsString::from("anullsrc=r=48000:cl=stereo"),
        ]);
    } else {
        if !reencode {
            args.push(OsString::from("-ss"));
            args.push(OsString::from(format_seconds(request.start)));
        }
        args.push(OsString::from("-i"));
        args.push(prepared.input.clone().into_os_string());
        // Palette-based GIF graphs do not accept an output-side seek in the
        // bundled FFmpeg build. Their trim filter below receives the absolute
        // source interval instead, keeping the first/last selected frames
        // exact without producing an empty GIF.
        if reencode && !is_gif {
            args.push(OsString::from("-ss"));
            args.push(OsString::from(format_seconds(request.start)));
        }
    }
    args.push(OsString::from("-t"));
    args.push(OsString::from(format_seconds(request.end - request.start)));

    match request.format {
        OutputFormat::Mp3 | OutputFormat::Wav => {
            // MP3 and WAV support a single audio stream. MP3 is rejected in
            // spawn() for the LGPL bundle used by the app; keeping this map
            // here makes the command correct for a future build with an MP3
            // encoder.
            args.extend([
                OsString::from("-map"),
                OsString::from(if silent_wav { "0:a:0" } else { "0:a:0?" }),
            ]);
            match request.format {
                OutputFormat::Mp3 => {
                    args.extend([
                        OsString::from("-c:a"),
                        OsString::from("libmp3lame"),
                        OsString::from("-q:a"),
                        OsString::from(audio_quality(request.quality).to_string()),
                    ]);
                }
                OutputFormat::Wav => {
                    args.extend([OsString::from("-c:a"), OsString::from("pcm_s16le")]);
                }
                _ => unreachable!(),
            }
        }
        OutputFormat::Gif => {
            let capabilities = gif_capabilities.unwrap_or_default();
            let mut filters = Vec::with_capacity(4);
            filters.push(format!(
                "trim=start={}:end={}",
                format_seconds(request.start),
                format_seconds(request.end)
            ));
            filters.push("setpts=PTS-STARTPTS".to_owned());
            filters.extend(gif_video_filters(
                request.crop,
                capabilities.fps_filter,
                request.quality,
            ));
            if capabilities.use_palette() {
                // palettegen retains frames until it receives EOF. Applying
                // the selected interval before splitting prevents a long
                // recording from being queued in the palette branch.
                let input = format!("[0:v:0]{}", filters.join(","));
                args.extend([
                    OsString::from("-filter_complex"),
                    OsString::from(format!(
                        "{input},split=2[gifbase][gifpalette];[gifpalette]palettegen=max_colors=256:stats_mode=diff[palette];[gifbase][palette]paletteuse=dither=sierra2_4a[gifout]"
                    )),
                    OsString::from("-map"),
                    OsString::from("[gifout]"),
                ]);
            } else {
                if !filters.is_empty() {
                    args.extend([OsString::from("-vf"), OsString::from(filters.join(","))]);
                }
                args.extend([OsString::from("-map"), OsString::from("0:v:0")]);
            }
            args.extend([
                OsString::from("-an"),
                OsString::from("-c:v"),
                OsString::from("gif"),
                OsString::from("-loop"),
                OsString::from("0"),
            ]);
        }
        OutputFormat::Mp4 | OutputFormat::Mkv | OutputFormat::Webm => {
            // Map video and every audio track, while dropping attachments and
            // data streams that most simple output containers cannot carry.
            args.extend([OsString::from("-map"), OsString::from("0:v?")]);
            if !request.mute_audio {
                args.extend([OsString::from("-map"), OsString::from("0:a?")]);
            } else {
                // Keep the mute policy explicit even though the video-only
                // map already excludes audio streams.
                args.push(OsString::from("-an"));
            }
            if let Some(crop) = request.crop {
                args.extend([
                    OsString::from("-vf"),
                    OsString::from(format!(
                        "crop={}:{}:{}:{}",
                        crop.width, crop.height, crop.x, crop.y
                    )),
                ]);
            }
            match (reencode, request.format) {
                (false, _) => {
                    args.extend([OsString::from("-c"), OsString::from("copy")]);
                }
                (true, OutputFormat::Mp4 | OutputFormat::Mkv) => {
                    args.extend([
                        // Preserve variable frame-rate timestamps during the
                        // decode/re-encode path. A 60 kHz time base is fine
                        // for video encoders and gives sub-frame
                        // precision without overflowing its timestamp range.
                        OsString::from("-fps_mode"),
                        OsString::from("passthrough"),
                        OsString::from("-enc_time_base:v"),
                        OsString::from("1:60000"),
                    ]);
                    // Use the same software H.264 encoder and rate policy on
                    // Linux, Windows, and macOS. Preserve 8-bit 4:2:0 playback compatibility.
                    let source = prepared.source_bitrates.unwrap_or_default();
                    let bitrate = source.video * u64::from(request.quality.clamp(50, 100)) / 100;
                    args.extend(
                        [
                            "-c:v",
                            "libopenh264",
                            "-pix_fmt",
                            "yuv420p",
                            "-profile:v",
                            "high",
                            "-rc_mode",
                            "bitrate",
                        ]
                        .into_iter()
                        .map(OsString::from),
                    );
                    args.extend([
                        OsString::from("-b:v"),
                        OsString::from(bitrate.to_string()),
                        OsString::from("-maxrate:v"),
                        OsString::from((bitrate + bitrate / 5).to_string()),
                    ]);
                    if !request.mute_audio {
                        args.extend([
                            OsString::from("-c:a"),
                            OsString::from("aac"),
                            OsString::from("-b:a"),
                            OsString::from(source.audio.to_string()),
                        ]);
                    }
                }
                (true, OutputFormat::Webm) => {
                    return Err(anyhow!(
                        "exact WebM export requires a WebM video/audio encoder that is not bundled"
                    ));
                }
                _ => unreachable!(),
            }
        }
    }

    if request.format == OutputFormat::Mp4 {
        args.extend([OsString::from("-movflags"), OsString::from("+faststart")]);
    }
    args.extend([
        OsString::from("-avoid_negative_ts"),
        OsString::from("make_zero"),
        OsString::from("-f"),
        OsString::from(request.format.muxer()),
        OsString::from("-y"),
        prepared.temp.clone().into_os_string(),
    ]);
    Ok(args)
}

fn gif_video_filters(crop: Option<CropRect>, fps_filter: bool, quality: u8) -> Vec<String> {
    let mut filters = Vec::with_capacity(2);
    if let Some(crop) = crop {
        filters.push(format!(
            "crop={}:{}:{}:{}",
            crop.width, crop.height, crop.x, crop.y
        ));
    }
    if fps_filter {
        filters.push(format!("fps={}", gif_frame_rate(quality)));
    }
    filters
}

fn gif_frame_rate(quality: u8) -> u8 {
    let quality = quality.clamp(50, 100);
    10 + (((u16::from(quality) - 50) * 20) / 50) as u8
}

fn format_seconds(seconds: f64) -> String {
    // Decimal seconds are accepted by FFmpeg and avoid locale-dependent
    // formatting. Nine fractional digits retain sub-microsecond UI values
    // while keeping command lines compact.
    format!("{seconds:.9}")
}

fn progress_seconds(line: &str) -> Option<f64> {
    let (key, value) = line.split_once('=')?;
    match key {
        "out_time_ms" | "out_time_us" => value.trim().parse::<f64>().ok().map(|value| {
            // FFmpeg's `out_time_ms` historically contains microseconds even
            // though the key says milliseconds. Both keys are treated as
            // microseconds, matching the documented -progress output.
            value / 1_000_000.0
        }),
        "out_time" => parse_timestamp(value.trim()),
        _ => None,
    }
}

fn parse_timestamp(value: &str) -> Option<f64> {
    let mut parts = value.split(':');
    let hours = parts.next()?.parse::<f64>().ok()?;
    let minutes = parts.next()?.parse::<f64>().ok()?;
    let seconds = parts.next()?.parse::<f64>().ok()?;
    if parts.next().is_some() {
        return None;
    }
    Some(hours * 3600.0 + minutes * 60.0 + seconds)
}

fn audio_quality(quality: u8) -> u8 {
    let quality = u16::from(quality.min(100));
    // libmp3lame's q:a scale ranges from 0 (best) to 9 (worst).
    (9 - ((quality * 9) / 100)) as u8
}

fn read_bounded(mut reader: impl Read, limit: usize) -> Vec<u8> {
    let mut output = Vec::with_capacity(limit.min(8192));
    let mut buffer = [0_u8; 8192];
    loop {
        match reader.read(&mut buffer) {
            Ok(0) => break,
            Ok(size) => {
                let remaining = limit.saturating_sub(output.len());
                if remaining > 0 {
                    output.extend_from_slice(&buffer[..size.min(remaining)]);
                }
            }
            Err(_) => break,
        }
    }
    output
}

fn bounded_text(bytes: &[u8], limit: usize) -> String {
    let end = bytes.len().min(limit);
    String::from_utf8_lossy(&bytes[..end]).trim().to_owned()
}

fn cleanup_temp(path: &Path) {
    let _ = fs::remove_file(path);
}

fn send_event(sender: &mpsc::Sender<JobEvent>, event: JobEvent) {
    let _ = sender.send(event);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remaining_time_uses_measured_progress_and_avoids_invalid_estimates() {
        assert_eq!(
            estimated_remaining(0.25, Duration::from_secs(10)),
            Some(Duration::from_secs(30))
        );
        assert_eq!(
            estimated_remaining(0.5, Duration::from_secs(10)),
            Some(Duration::from_secs(10))
        );
        for progress in [0.0, -1.0, 0.001, 1.0, f64::NAN, f64::INFINITY] {
            assert_eq!(estimated_remaining(progress, Duration::from_secs(10)), None);
        }
        assert_eq!(estimated_remaining(0.1, Duration::from_millis(100)), None);
    }

    #[test]
    fn parses_progress_formats() {
        assert_eq!(progress_seconds("out_time_ms=2500000"), Some(2.5));
        assert_eq!(progress_seconds("out_time_us=1250000"), Some(1.25));
        assert_eq!(progress_seconds("out_time=00:01:02.500000"), Some(62.5));
        assert_eq!(progress_seconds("progress=end"), None);
    }

    #[test]
    fn source_budget_separates_video_from_high_bitrate_audio() {
        let document = serde_json::json!({"streams": [
            {"codec_type": "video", "bit_rate": "284977"},
            {"codec_type": "audio", "bit_rate": "512000"}
        ], "format": {"bit_rate": "800487"}});
        let budget = source_bitrates(&document).unwrap();
        assert_eq!(budget.video, 284977);
        assert_eq!(budget.audio, 192000);
        let document = serde_json::json!({"streams": [
            {"codec_type": "video"}, {"codec_type": "audio", "bit_rate": "128000"}
        ], "format": {"size": "1000000", "duration": "10"}});
        assert_eq!(source_bitrates(&document).unwrap().video, 672000);
    }

    #[test]
    fn output_formats_have_stable_muxers_and_extensions() {
        assert_eq!(OutputFormat::Mp4.muxer(), "mp4");
        assert_eq!(OutputFormat::Mkv.muxer(), "matroska");
        assert_eq!(OutputFormat::Webm.extension(), "webm");
        assert_eq!(OutputFormat::Mp3.extension(), "mp3");
        assert_eq!(OutputFormat::Wav.muxer(), "wav");
        assert_eq!(OutputFormat::Gif.muxer(), "gif");
        assert_eq!(OutputFormat::Gif.extension(), "gif");
    }

    #[test]
    fn rejects_invalid_time_ranges() {
        let request = ExportRequest {
            input: PathBuf::from("input.mp4"),
            output: PathBuf::from("output.mp4"),
            start: 2.0,
            end: 2.0,
            mode: TrimMode::Fast,
            format: OutputFormat::Mp4,
            crop: None,
            quality: 80,
            mute_audio: false,
        };
        assert!(validate_request(&request).is_err());
    }

    #[test]
    fn rejects_invalid_crop_shapes_and_bounds() {
        assert!(
            validate_crop_shape(CropRect {
                x: 0,
                y: 0,
                width: 0,
                height: 20,
            })
            .is_err()
        );
        assert!(
            validate_crop_shape(CropRect {
                x: 0,
                y: 0,
                width: 21,
                height: 20,
            })
            .is_err()
        );
        assert!(
            validate_crop_bounds(
                CropRect {
                    x: 140,
                    y: 60,
                    width: 40,
                    height: 40,
                },
                160,
                90,
            )
            .is_err()
        );
        assert!(
            validate_crop_bounds(
                CropRect {
                    x: 20,
                    y: 10,
                    width: 80,
                    height: 40,
                },
                160,
                90,
            )
            .is_ok()
        );
    }

    #[test]
    fn rejects_unrecognized_rotation_metadata_for_crop() {
        let rotated: Value = serde_json::json!({
            "tags": {"rotate": "90"},
            "width": 1920,
            "height": 1080,
        });
        assert_eq!(crop_rotation(&rotated).unwrap(), Some(90.0));

        let matrix: Value = serde_json::json!({
            "side_data_list": [{
                "side_data_type": "Display Matrix",
                "displaymatrix": "unparsed"
            }]
        });
        assert!(crop_rotation(&matrix).is_err());
    }

    #[test]
    fn crop_forces_exact_video_encoding_and_emits_filter() {
        let prepared = PreparedRequest {
            source_bitrates: None,
            request: ExportRequest {
                input: PathBuf::from("input.mp4"),
                output: PathBuf::from("output.mp4"),
                start: 0.25,
                end: 1.75,
                mode: TrimMode::Fast,
                format: OutputFormat::Mp4,
                crop: Some(CropRect {
                    x: 20,
                    y: 10,
                    width: 80,
                    height: 40,
                }),
                quality: 80,
                mute_audio: false,
            },
            input: PathBuf::from("input.mp4"),
            output: PathBuf::from("output.mp4"),
            temp: PathBuf::from("output.part"),
        };
        let args = build_args(&prepared).expect("valid crop command");
        let args = args
            .into_iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert!(
            args.windows(2)
                .any(|window| { window == ["-vf".to_owned(), "crop=80:40:20:10".to_owned()] })
        );
        assert!(
            !args
                .windows(2)
                .any(|window| { window == ["-c".to_owned(), "copy".to_owned()] })
        );
        assert!(
            args.windows(2)
                .any(|window| { window == ["-c:v".to_owned(), "libopenh264".to_owned(),] })
        );
        let input_index = args.iter().position(|arg| arg == "-i").unwrap();
        let seek_index = args.iter().position(|arg| arg == "-ss").unwrap();
        assert!(
            seek_index > input_index,
            "crop must use exact input seeking"
        );
    }

    fn argument_strings(args: Vec<OsString>) -> Vec<String> {
        args.into_iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect()
    }

    fn prepared_request(format: OutputFormat) -> PreparedRequest {
        PreparedRequest {
            source_bitrates: None,
            request: ExportRequest {
                input: PathBuf::from("input.mp4"),
                output: PathBuf::from("output").with_extension(format.extension()),
                start: 2.25,
                end: 3.75,
                mode: TrimMode::Exact,
                format,
                crop: None,
                quality: 75,
                mute_audio: false,
            },
            input: PathBuf::from("input.mp4"),
            output: PathBuf::from("output").with_extension(format.extension()),
            temp: PathBuf::from("output.part"),
        }
    }

    #[test]
    fn gif_palette_graph_trims_before_palettegen_and_crops_selected_frames() {
        let mut prepared = prepared_request(OutputFormat::Gif);
        prepared.request.crop = Some(CropRect {
            x: 20,
            y: 10,
            width: 80,
            height: 40,
        });
        let args = argument_strings(
            build_args_with_gif_capabilities(
                &prepared,
                Some(GifCapabilities {
                    encoder: true,
                    muxer: true,
                    fps_filter: true,
                    split_filter: true,
                    palettegen_filter: true,
                    paletteuse_filter: true,
                }),
            )
            .expect("valid GIF command"),
        );
        let graph = args
            .iter()
            .position(|arg| arg == "-filter_complex")
            .and_then(|index| args.get(index + 1))
            .expect("palette graph");
        let trim = graph
            .find("trim=start=2.250000000:end=3.750000000")
            .expect("trim filter");
        let palette = graph.find("palettegen").expect("palettegen filter");
        assert!(trim < palette, "trim must precede palettegen: {graph}");
        assert!(graph.contains("setpts=PTS-STARTPTS,crop=80:40:20:10,fps=20,split=2"));
        assert!(!args.contains(&"-ss".to_owned()));
        assert!(args.contains(&"-t".to_owned()));
        assert!(args.contains(&"-progress".to_owned()));
    }

    #[test]
    fn muted_video_omits_audio_mapping_and_codec() {
        let mut prepared = prepared_request(OutputFormat::Mp4);
        prepared.request.mute_audio = true;
        let args = argument_strings(build_args(&prepared).expect("valid muted command"));
        assert!(args.contains(&"-an".to_owned()));
        assert!(!args.contains(&"0:a?".to_owned()));
        assert!(!args.contains(&"-c:a".to_owned()));
        assert!(!args.contains(&"aac".to_owned()));
    }

    #[test]
    fn muted_wav_uses_synthesized_silence() {
        let mut prepared = prepared_request(OutputFormat::Wav);
        prepared.request.mute_audio = true;
        let args = argument_strings(build_args(&prepared).expect("valid muted WAV command"));
        assert!(
            args.windows(2)
                .any(|window| { window == ["-f".to_owned(), "lavfi".to_owned()] })
        );
        assert!(args.contains(&"anullsrc=r=48000:cl=stereo".to_owned()));
        assert!(args.contains(&"0:a:0".to_owned()));
        assert!(args.contains(&"pcm_s16le".to_owned()));
        assert!(!args.contains(&prepared.input.to_string_lossy().into_owned()));
    }
}

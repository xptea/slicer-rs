//! Media discovery and inspection.
//!
//! The application deliberately does not search `PATH` for FFmpeg.  A
//! packaged build carries both programs next to the application and resolves
//! them from that installation directory.  `SLICER_FFMPEG_DIR` is available as
//! an explicit development/test override.

use anyhow::{Context, Result, anyhow, bail};
use serde::Deserialize;
use serde_json::Value;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// The FFmpeg programs used by the application.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Binaries {
    pub ffmpeg: PathBuf,
    pub ffprobe: PathBuf,
}

impl Binaries {
    /// Resolve the bundled FFmpeg programs.
    ///
    /// Resolution first honors `SLICER_FFMPEG_DIR`.  This is intentionally an
    /// explicit override rather than a `PATH` lookup, which keeps packaged
    /// builds reproducible and prevents accidentally using a different
    /// system FFmpeg.  Without the override, the two layouts used by the
    /// installer are checked relative to the executable:
    ///
    /// * `../lib/slicer/bin`
    /// * `resources/bin`
    pub fn resolve() -> Result<Self> {
        if let Some(dir) = env::var_os("SLICER_FFMPEG_DIR") {
            let dir = PathBuf::from(dir);
            return Self::from_dir(&dir).with_context(|| {
                format!(
                    "SLICER_FFMPEG_DIR does not contain usable ffmpeg and ffprobe binaries: {}",
                    dir.display()
                )
            });
        }

        let executable = env::current_exe().context("unable to determine application path")?;
        let executable_dir = executable
            .parent()
            .ok_or_else(|| anyhow!("application path has no parent directory"))?;

        let candidates = [
            executable_dir.join("../lib/slicer/bin"),
            executable_dir.join("resources/bin"),
        ];

        let mut checked = Vec::with_capacity(candidates.len());
        for dir in candidates {
            checked.push(dir.clone());
            if let Ok(binaries) = Self::from_dir(&dir) {
                return Ok(binaries);
            }
        }

        bail!(
            "bundled ffmpeg and ffprobe were not found; checked {}",
            checked
                .iter()
                .map(|path| path.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        )
    }

    /// Build a binary pair from a directory.
    ///
    /// This constructor is useful to tests and to packaging code that already
    /// knows the bundle directory.  It does not consult `PATH`.
    pub fn from_dir(dir: impl AsRef<Path>) -> Result<Self> {
        let dir = dir.as_ref();
        let ffmpeg = dir.join(program_name("ffmpeg"));
        let ffprobe = dir.join(program_name("ffprobe"));
        let binaries = Self { ffmpeg, ffprobe };
        binaries.validate()?;
        Ok(binaries)
    }

    /// Check that both configured paths point to executable regular files.
    pub fn validate(&self) -> Result<()> {
        validate_program(&self.ffmpeg, "ffmpeg")?;
        validate_program(&self.ffprobe, "ffprobe")?;
        Ok(())
    }
}

/// Resolve bundled FFmpeg programs.  This free function mirrors
/// [`Binaries::resolve`] for callers that prefer a module-level API.
pub fn resolve() -> Result<Binaries> {
    Binaries::resolve()
}

/// Metadata returned by [`inspect`].
#[derive(Clone, Debug, Deserialize, serde::Serialize, PartialEq)]
pub struct MediaInfo {
    pub duration: f64,
    pub size: u64,
    pub streams: Vec<MediaStream>,
}

/// A single audio, video, or other stream in a media file.
#[derive(Clone, Debug, Deserialize, serde::Serialize, PartialEq, Eq)]
pub struct MediaStream {
    pub index: u32,
    pub kind: String,
    pub codec: String,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub channels: Option<u32>,
}

/// A reduced rational reported by FFmpeg (for example `30000/1001`).
/// Keeping the numerator and denominator intact avoids turning source timing
/// into a binary floating-point approximation at the media boundary.
#[derive(Clone, Copy, Debug, Deserialize, serde::Serialize, PartialEq, Eq)]
pub struct MediaRational {
    pub numerator: i64,
    pub denominator: i64,
}

impl MediaRational {
    pub fn new(numerator: i64, denominator: i64) -> Option<Self> {
        if denominator == 0 {
            return None;
        }
        let sign = if denominator < 0 { -1 } else { 1 };
        let numerator = numerator.checked_mul(sign)?;
        let denominator = denominator.checked_mul(sign)?;
        let divisor = gcd(numerator.unsigned_abs(), denominator.unsigned_abs());
        let divisor = i64::try_from(divisor).ok().filter(|value| *value > 0)?;
        Some(Self {
            numerator: numerator / divisor,
            denominator: denominator / divisor,
        })
    }

    pub fn as_f64(self) -> f64 {
        self.numerator as f64 / self.denominator as f64
    }
}

/// Stream metadata needed by source-time mapping and color/orientation-aware
/// composition.  `MediaInfo` remains the compact compatibility API used by
/// the existing home screen and legacy player.
#[derive(Clone, Debug, Deserialize, serde::Serialize, PartialEq)]
pub struct DetailedMediaStream {
    pub base: MediaStream,
    pub time_base: Option<MediaRational>,
    pub average_frame_rate: Option<MediaRational>,
    pub nominal_frame_rate: Option<MediaRational>,
    pub start_time: Option<f64>,
    pub duration: Option<f64>,
    pub frames: Option<u64>,
    pub sample_rate: Option<u32>,
    pub pixel_aspect_ratio: Option<MediaRational>,
    pub color_range: Option<String>,
    pub color_space: Option<String>,
    pub pixel_format: Option<String>,
    pub has_b_frames: Option<u32>,
    pub rotation: Option<f64>,
    pub has_alpha: bool,
}

/// Full probe result for the project/engine boundary.
#[derive(Clone, Debug, Deserialize, serde::Serialize, PartialEq)]
pub struct DetailedMediaInfo {
    pub duration: f64,
    pub size: u64,
    pub streams: Vec<DetailedMediaStream>,
}

/// Inspect a media file with the bundled `ffprobe` and parse its JSON output.
pub fn inspect(binaries: &Binaries, input: &Path) -> Result<MediaInfo> {
    binaries.validate()?;
    let metadata = fs::metadata(input)
        .with_context(|| format!("unable to read media file {}", input.display()))?;
    if !metadata.is_file() {
        bail!("media input is not a regular file: {}", input.display());
    }

    let raw = probe_document(binaries, input)?;

    let duration = document_duration(&raw);
    if !duration.is_finite() || duration < 0.0 {
        bail!(
            "ffprobe returned an invalid duration for {}",
            input.display()
        );
    }

    let size = document_size(&raw, metadata.len());

    let streams = raw
        .streams
        .unwrap_or_default()
        .into_iter()
        .enumerate()
        .map(|(position, stream)| MediaStream {
            index: stream.index.unwrap_or(position as u32),
            kind: stream.codec_type.unwrap_or_else(|| "unknown".to_owned()),
            codec: stream.codec_name.unwrap_or_else(|| "unknown".to_owned()),
            width: stream.width.as_ref().and_then(value_as_u32),
            height: stream.height.as_ref().and_then(value_as_u32),
            channels: stream.channels.as_ref().and_then(value_as_u32),
        })
        .collect();

    Ok(MediaInfo {
        duration,
        size,
        streams,
    })
}

/// Inspect a media file while preserving stream timing and presentation
/// metadata.  This calls the same explicit bundled `ffprobe` as [`inspect`]
/// and never discovers a system executable through `PATH`.
pub fn inspect_detailed(binaries: &Binaries, input: &Path) -> Result<DetailedMediaInfo> {
    binaries.validate()?;
    let metadata = fs::metadata(input)
        .with_context(|| format!("unable to read media file {}", input.display()))?;
    if !metadata.is_file() {
        bail!("media input is not a regular file: {}", input.display());
    }
    let raw = probe_document(binaries, input)?;
    let duration = document_duration(&raw);
    if !duration.is_finite() || duration < 0.0 {
        bail!(
            "ffprobe returned an invalid duration for {}",
            input.display()
        );
    }
    let size = document_size(&raw, metadata.len());
    let streams = raw
        .streams
        .unwrap_or_default()
        .into_iter()
        .enumerate()
        .map(|(position, stream)| {
            let base = base_stream(&stream, position);
            let pixel_format = stream.pixel_format.clone();
            let rotation = stream_rotation(&stream);
            DetailedMediaStream {
                base,
                time_base: stream.time_base.as_deref().and_then(parse_rational),
                average_frame_rate: stream
                    .average_frame_rate
                    .as_deref()
                    .and_then(parse_rational),
                nominal_frame_rate: stream
                    .nominal_frame_rate
                    .as_deref()
                    .and_then(parse_rational),
                start_time: stream
                    .start_time
                    .as_ref()
                    .and_then(|value| value_as_f64(Some(value))),
                duration: stream
                    .duration
                    .as_ref()
                    .and_then(|value| value_as_f64(Some(value))),
                frames: stream
                    .frames
                    .as_ref()
                    .and_then(|value| value_as_u64(Some(value))),
                sample_rate: stream.sample_rate.as_ref().and_then(value_as_u32),
                pixel_aspect_ratio: stream
                    .pixel_aspect_ratio
                    .as_deref()
                    .and_then(parse_rational),
                color_range: stream.color_range,
                color_space: stream.color_space,
                pixel_format,
                has_b_frames: stream.has_b_frames.as_ref().and_then(value_as_u32),
                rotation,
                has_alpha: stream
                    .pixel_format
                    .as_deref()
                    .is_some_and(pixel_format_has_alpha),
            }
        })
        .collect();
    Ok(DetailedMediaInfo {
        duration,
        size,
        streams,
    })
}

/// Inspect a source and construct the project asset metadata used by the
/// layered session. This is kept at the media boundary so UI import workers
/// and headless callers make the same classification and timing decisions.
pub fn inspect_project_asset(binaries: &Binaries, input: &Path) -> Result<crate::project::Asset> {
    let info = inspect_detailed(binaries, input)?;
    let video = info
        .streams
        .iter()
        .find(|stream| stream.base.kind == "video");
    let audio = info
        .streams
        .iter()
        .find(|stream| stream.base.kind == "audio");

    if is_still_image_path(input) {
        let stream = video.context("image input has no video stream")?;
        let width = stream.base.width.context("image width is unknown")?;
        let height = stream.base.height.context("image height is unknown")?;
        let mut asset =
            crate::project::Asset::image(crate::project::AssetId::fresh(), input, width, height)?;
        if let crate::project::AssetMetadata::Image(metadata) = &mut asset.metadata {
            metadata.orientation = orientation_from_rotation(stream.rotation);
            metadata.pixel_aspect = project_rational(stream.pixel_aspect_ratio)
                .filter(|value| *value > crate::project::Time::ZERO)
                .unwrap_or(crate::project::Rational::ONE);
        }
        return Ok(asset);
    }

    if let Some(stream) = video {
        let width = stream.base.width.context("video width is unknown")?;
        let height = stream.base.height.context("video height is unknown")?;
        let duration = positive_duration(info.duration)
            .or_else(|| stream.duration.and_then(positive_duration))
            .context("video duration is unknown")?;
        let mut asset = crate::project::Asset::video(
            crate::project::AssetId::fresh(),
            input,
            crate::project::Rational::from_seconds(duration)?,
            width,
            height,
        )?;
        if let crate::project::AssetMetadata::Video(metadata) = &mut asset.metadata {
            metadata.time_base = project_rational(stream.time_base)
                .filter(|value| *value > crate::project::Time::ZERO)
                .unwrap_or(crate::project::Rational::ONE);
            metadata.frame_rate = stream
                .average_frame_rate
                .or(stream.nominal_frame_rate)
                .and_then(project_frame_rate);
            metadata.orientation = orientation_from_rotation(stream.rotation);
            metadata.pixel_aspect = project_rational(stream.pixel_aspect_ratio)
                .filter(|value| *value > crate::project::Time::ZERO)
                .unwrap_or(crate::project::Rational::ONE);
            metadata.has_audio = audio.is_some();
        }
        return Ok(asset);
    }

    if let Some(stream) = audio {
        let duration = positive_duration(info.duration)
            .or_else(|| stream.duration.and_then(positive_duration))
            .context("audio duration is unknown")?;
        let sample_rate = stream.sample_rate.unwrap_or(48_000);
        let channels = stream.base.channels.unwrap_or(2);
        let channels = u16::try_from(channels).context("audio channel count is too large")?;
        return Ok(crate::project::Asset::audio(
            crate::project::AssetId::fresh(),
            input,
            crate::project::Rational::from_seconds(duration)?,
            sample_rate,
            channels,
        )?);
    }

    bail!("input has no supported video, image, or audio stream")
}

fn positive_duration(value: f64) -> Option<f64> {
    value
        .is_finite()
        .then_some(value)
        .filter(|value| *value > 0.0)
}

fn is_still_image_path(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            matches!(
                extension.to_ascii_lowercase().as_str(),
                "bmp" | "jpeg" | "jpg" | "png" | "tif" | "tiff" | "webp"
            )
        })
}

fn project_rational(value: Option<MediaRational>) -> Option<crate::project::Rational> {
    let value = value?;
    let denominator = u32::try_from(value.denominator).ok()?;
    crate::project::Rational::new(value.numerator, denominator).ok()
}

fn project_frame_rate(value: MediaRational) -> Option<crate::project::FrameRate> {
    let numerator = u32::try_from(value.numerator).ok()?;
    let denominator = u32::try_from(value.denominator).ok()?;
    crate::project::FrameRate::new(numerator, denominator).ok()
}

fn orientation_from_rotation(rotation: Option<f64>) -> crate::project::Orientation {
    let Some(rotation) = rotation.filter(|rotation| rotation.is_finite()) else {
        return crate::project::Orientation::Normal;
    };
    let normalized = (rotation.round() as i64).rem_euclid(360);
    match normalized {
        90 => crate::project::Orientation::Rotate90,
        180 => crate::project::Orientation::Rotate180,
        270 => crate::project::Orientation::Rotate270,
        _ => crate::project::Orientation::Normal,
    }
}

fn probe_document(binaries: &Binaries, input: &Path) -> Result<ProbeDocument> {
    // Keep the input as a direct argument. In particular, do not construct a
    // shell command: paths may contain spaces, Unicode, or shell metacharacters.
    let output = Command::new(&binaries.ffprobe)
        .args([
            "-v",
            "error",
            "-show_streams",
            "-show_format",
            "-print_format",
            "json",
            "--",
        ])
        .arg(input)
        .output()
        .with_context(|| format!("failed to launch ffprobe for {}", input.display()))?;
    if !output.status.success() {
        let stderr = bounded_text(&output.stderr, MAX_DIAGNOSTIC_BYTES);
        if stderr.is_empty() {
            bail!(
                "ffprobe failed for {} with status {}",
                input.display(),
                output.status
            );
        }
        bail!(
            "ffprobe failed for {} with status {}: {}",
            input.display(),
            output.status,
            stderr
        );
    }
    serde_json::from_slice(&output.stdout)
        .with_context(|| format!("ffprobe returned invalid JSON for {}", input.display()))
}

fn document_duration(raw: &ProbeDocument) -> f64 {
    raw.format
        .as_ref()
        .and_then(|format| value_as_f64(format.duration.as_ref()))
        .unwrap_or(0.0)
}

fn document_size(raw: &ProbeDocument, fallback: u64) -> u64 {
    raw.format
        .as_ref()
        .and_then(|format| value_as_u64(format.size.as_ref()))
        .unwrap_or(fallback)
}

fn base_stream(stream: &ProbeStream, position: usize) -> MediaStream {
    MediaStream {
        index: stream.index.unwrap_or(position as u32),
        kind: stream
            .codec_type
            .clone()
            .unwrap_or_else(|| "unknown".to_owned()),
        codec: stream
            .codec_name
            .clone()
            .unwrap_or_else(|| "unknown".to_owned()),
        width: stream.width.as_ref().and_then(value_as_u32),
        height: stream.height.as_ref().and_then(value_as_u32),
        channels: stream.channels.as_ref().and_then(value_as_u32),
    }
}

fn parse_rational(value: &str) -> Option<MediaRational> {
    let (numerator, denominator) = value.split_once('/')?;
    let numerator = numerator.trim().parse().ok()?;
    let denominator = denominator.trim().parse().ok()?;
    MediaRational::new(numerator, denominator)
}

fn stream_rotation(stream: &ProbeStream) -> Option<f64> {
    let tag_rotation = stream
        .tags
        .as_ref()
        .and_then(|tags| tags.get("rotate"))
        .and_then(|value| value_as_f64(Some(value)));
    let side_rotation = stream.side_data.as_ref().and_then(|side_data| {
        side_data.iter().find_map(|entry| {
            entry
                .get("rotation")
                .and_then(|value| value_as_f64(Some(value)))
                .or_else(|| {
                    entry.get("displaymatrix").and_then(|value| {
                        value.as_str().and_then(|matrix| {
                            matrix.lines().find_map(|line| line.trim().parse().ok())
                        })
                    })
                })
        })
    });
    side_rotation.or(tag_rotation)
}

fn pixel_format_has_alpha(format: &str) -> bool {
    let format = format.to_ascii_lowercase();
    format.contains("rgba")
        || format.contains("argb")
        || format.contains("abgr")
        || format.contains("yuva")
        || format.ends_with('a')
}

fn gcd(mut left: u64, mut right: u64) -> u64 {
    while right != 0 {
        let remainder = left % right;
        left = right;
        right = remainder;
    }
    left
}

const MAX_DIAGNOSTIC_BYTES: usize = 16 * 1024;

fn program_name(name: &str) -> &'static str {
    #[cfg(windows)]
    {
        match name {
            "ffmpeg" => "ffmpeg.exe",
            "ffprobe" => "ffprobe.exe",
            _ => unreachable!("unknown bundled program"),
        }
    }
    #[cfg(not(windows))]
    {
        match name {
            "ffmpeg" => "ffmpeg",
            "ffprobe" => "ffprobe",
            _ => unreachable!("unknown bundled program"),
        }
    }
}

fn validate_program(path: &Path, name: &str) -> Result<()> {
    let metadata = fs::metadata(path)
        .with_context(|| format!("{} binary does not exist: {}", name, path.display()))?;
    if !metadata.is_file() {
        bail!("{} binary is not a regular file: {}", name, path.display());
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o111 == 0 {
            bail!("{} binary is not executable: {}", name, path.display());
        }
    }

    Ok(())
}

fn bounded_text(bytes: &[u8], limit: usize) -> String {
    let end = bytes.len().min(limit);
    String::from_utf8_lossy(&bytes[..end]).trim().to_owned()
}

fn value_as_f64(value: Option<&Value>) -> Option<f64> {
    match value? {
        Value::Number(number) => number.as_f64(),
        Value::String(value) => value.parse::<f64>().ok(),
        _ => None,
    }
}

fn value_as_u64(value: Option<&Value>) -> Option<u64> {
    match value? {
        Value::Number(number) => number.as_u64().or_else(|| {
            number
                .as_f64()
                .filter(|value| *value >= 0.0)
                .map(|v| v as u64)
        }),
        Value::String(value) => value.parse::<u64>().ok(),
        _ => None,
    }
}

fn value_as_u32(value: &Value) -> Option<u32> {
    value_as_u64(Some(value)).and_then(|value| u32::try_from(value).ok())
}

#[derive(Debug, Deserialize)]
struct ProbeDocument {
    format: Option<ProbeFormat>,
    streams: Option<Vec<ProbeStream>>,
}

#[derive(Debug, Deserialize)]
struct ProbeFormat {
    duration: Option<Value>,
    size: Option<Value>,
}

#[derive(Debug, Deserialize)]
struct ProbeStream {
    #[serde(default)]
    index: Option<u32>,
    #[serde(default)]
    codec_type: Option<String>,
    #[serde(default)]
    codec_name: Option<String>,
    #[serde(default)]
    width: Option<Value>,
    #[serde(default)]
    height: Option<Value>,
    #[serde(default)]
    channels: Option<Value>,
    #[serde(default)]
    time_base: Option<String>,
    #[serde(default, alias = "avg_frame_rate")]
    average_frame_rate: Option<String>,
    #[serde(default, alias = "r_frame_rate")]
    nominal_frame_rate: Option<String>,
    #[serde(default)]
    start_time: Option<Value>,
    #[serde(default)]
    duration: Option<Value>,
    #[serde(default, alias = "nb_frames")]
    frames: Option<Value>,
    #[serde(default)]
    sample_rate: Option<Value>,
    #[serde(default, alias = "sample_aspect_ratio")]
    pixel_aspect_ratio: Option<String>,
    #[serde(default)]
    color_range: Option<String>,
    #[serde(default, alias = "color_space")]
    color_space: Option<String>,
    #[serde(default, alias = "pix_fmt")]
    pixel_format: Option<String>,
    #[serde(default)]
    has_b_frames: Option<Value>,
    #[serde(default)]
    tags: Option<std::collections::HashMap<String, Value>>,
    #[serde(default, alias = "side_data_list")]
    side_data: Option<Vec<std::collections::HashMap<String, Value>>>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_ffprobe_string_numbers_and_optional_fields() {
        let json = br#"{
            "format": {"duration": "12.5", "size": "42"},
            "streams": [
                {"index": 0, "codec_type": "video", "codec_name": "h264", "width": 1920, "height": "1080"},
                {"index": 1, "codec_type": "audio", "codec_name": "aac", "channels": 2}
            ]
        }"#;
        let raw: ProbeDocument = serde_json::from_slice(json).expect("valid probe fixture");
        let info = MediaInfo {
            duration: raw
                .format
                .as_ref()
                .and_then(|format| value_as_f64(format.duration.as_ref()))
                .unwrap(),
            size: raw
                .format
                .as_ref()
                .and_then(|format| value_as_u64(format.size.as_ref()))
                .unwrap(),
            streams: raw
                .streams
                .unwrap()
                .into_iter()
                .map(|stream| MediaStream {
                    index: stream.index.unwrap(),
                    kind: stream.codec_type.unwrap(),
                    codec: stream.codec_name.unwrap(),
                    width: stream.width.as_ref().and_then(value_as_u32),
                    height: stream.height.as_ref().and_then(value_as_u32),
                    channels: stream.channels.as_ref().and_then(value_as_u32),
                })
                .collect(),
        };
        assert_eq!(info.duration, 12.5);
        assert_eq!(info.size, 42);
        assert_eq!(info.streams[0].width, Some(1920));
        assert_eq!(info.streams[0].height, Some(1080));
        assert_eq!(info.streams[1].channels, Some(2));
    }

    #[test]
    fn from_dir_requires_both_programs() {
        let result = Binaries::from_dir("/definitely/missing");
        assert!(result.is_err());
    }
}

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

/// Inspect a media file with the bundled `ffprobe` and parse its JSON output.
pub fn inspect(binaries: &Binaries, input: &Path) -> Result<MediaInfo> {
    binaries.validate()?;
    let metadata = fs::metadata(input)
        .with_context(|| format!("unable to read media file {}", input.display()))?;
    if !metadata.is_file() {
        bail!("media input is not a regular file: {}", input.display());
    }

    // Keep the input as a direct argument.  In particular, do not construct a
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

    let raw: ProbeDocument = serde_json::from_slice(&output.stdout)
        .with_context(|| format!("ffprobe returned invalid JSON for {}", input.display()))?;

    let duration = raw
        .format
        .as_ref()
        .and_then(|format| value_as_f64(format.duration.as_ref()))
        .unwrap_or(0.0);
    if !duration.is_finite() || duration < 0.0 {
        bail!(
            "ffprobe returned an invalid duration for {}",
            input.display()
        );
    }

    let size = raw
        .format
        .as_ref()
        .and_then(|format| value_as_u64(format.size.as_ref()))
        .unwrap_or(metadata.len());

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
    index: Option<u32>,
    codec_type: Option<String>,
    codec_name: Option<String>,
    width: Option<Value>,
    height: Option<Value>,
    channels: Option<Value>,
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

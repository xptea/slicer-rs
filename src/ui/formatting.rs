//! Shared timestamp, file size, and output-label formatting.

use crate::job;
use std::path::{Path, PathBuf};

pub(super) fn default_output_path(input: &Path, format: job::OutputFormat) -> PathBuf {
    let stem = input
        .file_stem()
        .and_then(|name| name.to_str())
        .unwrap_or("clip");
    let parent = input.parent().unwrap_or_else(|| Path::new("."));
    parent.join(format!("{stem}-clip.{}", format.extension()))
}

pub(super) fn format_label(format: job::OutputFormat) -> &'static str {
    match format {
        job::OutputFormat::Mp4 => "MP4",
        job::OutputFormat::Mkv => "MKV",
        job::OutputFormat::Webm => "WebM",
        job::OutputFormat::Mp3 => "MP3",
        job::OutputFormat::Wav => "WAV",
        job::OutputFormat::Gif => "GIF",
    }
}

pub(super) fn parse_timestamp(value: &str) -> Option<f64> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    if value.starts_with('-') {
        return None;
    }
    let parts: Vec<_> = value.split(':').collect();
    if parts.iter().any(|part| part.trim_start().starts_with('-')) {
        return None;
    }
    if parts.iter().any(|part| {
        part.parse::<f64>()
            .map_or(true, |value| !value.is_finite() || value < 0.0)
    }) {
        return None;
    }
    let seconds = match parts.as_slice() {
        [seconds] => seconds.parse::<f64>().ok()?,
        [minutes, seconds] => minutes.parse::<f64>().ok()? * 60.0 + seconds.parse::<f64>().ok()?,
        [hours, minutes, seconds] => {
            hours.parse::<f64>().ok()? * 3600.0
                + minutes.parse::<f64>().ok()? * 60.0
                + seconds.parse::<f64>().ok()?
        }
        _ => return None,
    };
    seconds
        .is_finite()
        .then_some(seconds)
        .filter(|seconds| *seconds >= 0.0)
}

pub(super) fn format_timestamp(seconds: f64) -> String {
    let total_millis = (seconds.max(0.0) * 1000.0).round() as u64;
    let millis = total_millis % 1000;
    let total = total_millis / 1000;
    let hours = total / 3600;
    let minutes = (total / 60) % 60;
    let seconds = total % 60;
    if hours > 0 {
        if millis == 0 {
            format!("{hours:02}:{minutes:02}:{seconds:02}")
        } else {
            format!("{hours:02}:{minutes:02}:{seconds:02}.{millis:03}")
        }
    } else if millis == 0 {
        format!("{minutes}:{seconds:02}")
    } else {
        format!("{minutes}:{seconds:02}.{millis:03}")
    }
}

#[cfg(test)]
mod tests {
    use super::{format_timestamp, parse_timestamp};

    #[test]
    fn timestamps_keep_subseconds() {
        assert_eq!(format_timestamp(3.6), "0:03.600");
        assert_eq!(parse_timestamp("0:03.600"), Some(3.6));
    }

    #[test]
    fn timestamps_reject_negative_values() {
        assert_eq!(parse_timestamp("-0:01"), None);
        assert_eq!(parse_timestamp("-1"), None);
        assert_eq!(parse_timestamp("1:-1"), None);
    }
}

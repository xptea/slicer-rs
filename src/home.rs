//! Local library settings and a small, deterministic home-page listing.
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    time::SystemTime,
};

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Settings {
    pub library_directory: Option<PathBuf>,
    /// Defaults used by the editor's one-click Export action.
    ///
    /// The serde default keeps settings written by older Slicer versions
    /// valid: files without this field receive the same defaults as a fresh
    /// install.
    #[serde(default)]
    pub export_defaults: ExportDefaults,
}

/// The export preferences persisted in [`Settings`].
///
/// This is deliberately separate from `job::OutputFormat`. The job layer is
/// also used by the command-line interface and does not own the on-disk
/// settings format, while this enum gives serde a stable representation.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ExportFormat {
    #[default]
    Mp4,
    Mkv,
    Webm,
    Mp3,
    Wav,
    Gif,
}

impl ExportFormat {
    /// The filename extension associated with the saved preference.
    pub const fn extension(self) -> &'static str {
        match self {
            Self::Mp4 => "mp4",
            Self::Mkv => "mkv",
            Self::Webm => "webm",
            Self::Mp3 => "mp3",
            Self::Wav => "wav",
            Self::Gif => "gif",
        }
    }

    /// The label used by the settings UI.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Mp4 => "MP4",
            Self::Mkv => "MKV",
            Self::Webm => "WebM",
            Self::Mp3 => "MP3",
            Self::Wav => "WAV",
            Self::Gif => "GIF",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExportDefaults {
    #[serde(default)]
    pub format: ExportFormat,
    /// Video quality from 50 (smaller files) through 100 (best quality).
    #[serde(default = "default_export_quality")]
    pub quality: u8,
    /// Folder for exported clips. `None` means next to the source video.
    #[serde(default)]
    pub output_directory: Option<PathBuf>,
    /// Copy a successfully exported clip to the system clipboard.
    #[serde(default = "default_copy_to_clipboard")]
    pub copy_to_clipboard: bool,
}

impl Default for ExportDefaults {
    fn default() -> Self {
        Self {
            format: ExportFormat::default(),
            quality: default_export_quality(),
            output_directory: None,
            copy_to_clipboard: true,
        }
    }
}

impl ExportDefaults {
    /// Keep values loaded from hand-edited settings inside the UI's range.
    pub fn normalized(mut self) -> Self {
        self.quality = self.quality.clamp(50, 100);
        // Exact WebM and MP3 are unavailable in the bundled FFmpeg build, so
        // hand-edited settings cannot leave the one-click action unusable.
        if matches!(self.format, ExportFormat::Webm | ExportFormat::Mp3) {
            self.format = ExportFormat::Mp4;
        }
        self
    }
}

const fn default_export_quality() -> u8 {
    100
}

const fn default_copy_to_clipboard() -> bool {
    true
}
impl Settings {
    pub fn load() -> Result<Self> {
        Self::load_at(&settings_path()?)
    }
    fn load_at(path: &Path) -> Result<Self> {
        match fs::read(path) {
            Ok(bytes) => {
                let mut settings: Self = serde_json::from_slice(&bytes)
                    .with_context(|| format!("Cannot read settings at {}", path.display()))?;
                settings.export_defaults = settings.export_defaults.normalized();
                Ok(settings)
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(error) => Err(error).context("Cannot load Slicer settings"),
        }
    }
    pub fn save(&self) -> Result<()> {
        self.save_at(&settings_path()?)
    }
    fn save_at(&self, path: &Path) -> Result<()> {
        let parent = path.parent().context("Settings directory is missing")?;
        fs::create_dir_all(parent).context("Cannot create settings directory")?;
        let pending = parent.join(format!("settings-{}.tmp", std::process::id()));
        let bytes = serde_json::to_vec_pretty(self)?;
        let result = (|| -> Result<()> {
            let mut file = fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .open(&pending)?;
            file.write_all(&bytes)?;
            file.sync_all()?;
            #[cfg(target_os = "windows")]
            if path.exists() {
                fs::remove_file(&path)?;
            }
            fs::rename(&pending, path)?;
            Ok(())
        })();
        if result.is_err() {
            let _ = fs::remove_file(pending);
        }
        result.context("Cannot save Slicer settings")
    }
}
fn settings_path() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("SLICER_CONFIG_DIR") {
        return Ok(PathBuf::from(path).join("settings.json"));
    }
    #[cfg(target_os = "windows")]
    if let Some(path) = std::env::var_os("APPDATA") {
        return Ok(PathBuf::from(path).join("Slicer/settings.json"));
    }
    #[cfg(target_os = "macos")]
    if let Some(path) = std::env::var_os("HOME") {
        return Ok(PathBuf::from(path).join("Library/Application Support/Slicer/settings.json"));
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        if let Some(path) =
            std::env::var_os("XDG_CONFIG_HOME").filter(|p| Path::new(p).is_absolute())
        {
            return Ok(PathBuf::from(path).join("slicer/settings.json"));
        }
        if let Some(path) = std::env::var_os("HOME") {
            return Ok(PathBuf::from(path).join(".config/slicer/settings.json"));
        }
    }
    bail!("Cannot determine settings location; set SLICER_CONFIG_DIR")
}
#[derive(Clone, Debug)]
pub struct RecentVideo {
    pub path: PathBuf,
    pub name: String,
    pub modified: SystemTime,
    pub size: u64,
}
/// Non-recursive listing, newest modification first; path breaks timestamp ties.
/// Broken entries are skipped so a file deleted during refresh does not hide the library.
pub fn recent_videos(directory: &Path) -> Result<Vec<RecentVideo>> {
    let mut videos = Vec::new();
    for entry in fs::read_dir(directory)
        .with_context(|| format!("Cannot open video folder {}", directory.display()))?
    {
        let Ok(entry) = entry else { continue };
        let path = entry.path();
        if !is_video_path(&path) {
            continue;
        }
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        if !metadata.is_file() {
            continue;
        }
        videos.push(RecentVideo {
            name: entry.file_name().to_string_lossy().into_owned(),
            path,
            modified: metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH),
            size: metadata.len(),
        });
    }
    videos.sort_by(|a, b| {
        b.modified
            .cmp(&a.modified)
            .then_with(|| a.path.cmp(&b.path))
    });
    videos.truncate(3);
    Ok(videos)
}
fn is_video_path(path: &Path) -> bool {
    path.extension()
        .and_then(|x| x.to_str())
        .is_some_and(|extension| {
            matches!(
                extension.to_ascii_lowercase().as_str(),
                "mp4"
                    | "mkv"
                    | "mov"
                    | "webm"
                    | "avi"
                    | "m4v"
                    | "mpg"
                    | "mpeg"
                    | "mts"
                    | "m2ts"
                    | "ts"
                    | "ogv"
                    | "wmv"
                    | "flv"
                    | "3gp"
            )
        })
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_three_newest_video_files_are_returned() {
        let dir = tempfile::tempdir().unwrap();
        for (i, name) in [
            "old.mp4",
            "second.MOV",
            "café 日本.mkv",
            "latest.webm",
            "ignored.txt",
        ]
        .iter()
        .enumerate()
        {
            let path = dir.path().join(name);
            let file = fs::File::create(path).unwrap();
            file.set_modified(
                SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(i as u64 + 1),
            )
            .unwrap();
        }
        fs::create_dir(dir.path().join("directory.mp4")).unwrap();
        let videos = recent_videos(dir.path()).unwrap();
        assert_eq!(
            videos.iter().map(|x| x.name.as_str()).collect::<Vec<_>>(),
            ["latest.webm", "café 日本.mkv", "second.MOV"]
        );
    }
    #[test]
    fn missing_library_is_an_actionable_error() {
        let dir = tempfile::tempdir().unwrap();
        assert!(
            recent_videos(&dir.path().join("missing"))
                .unwrap_err()
                .to_string()
                .contains("Cannot open video folder")
        );
    }
    #[test]
    fn settings_round_trip_preserves_unicode() {
        let settings = Settings {
            library_directory: Some(PathBuf::from("/videos/café 日本")),
            ..Settings::default()
        };
        let decoded: Settings =
            serde_json::from_slice(&serde_json::to_vec(&settings).unwrap()).unwrap();
        assert_eq!(decoded.library_directory, settings.library_directory);
    }

    #[test]
    fn older_settings_receive_safe_export_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        fs::write(&path, br#"{"library_directory":null}"#).unwrap();
        let settings = Settings::load_at(&path).unwrap();
        assert_eq!(settings.export_defaults.format, ExportFormat::Mp4);
        assert_eq!(settings.export_defaults.quality, 100);
        assert!(settings.export_defaults.copy_to_clipboard);
        assert!(settings.export_defaults.output_directory.is_none());
    }

    #[test]
    fn export_defaults_normalize_hand_edited_values() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        fs::write(
            &path,
            br#"{
                "library_directory": null,
                "export_defaults": {"format":"mp3", "quality": 4}
            }"#,
        )
        .unwrap();
        let settings = Settings::load_at(&path).unwrap();
        assert_eq!(settings.export_defaults.format, ExportFormat::Mp4);
        assert_eq!(settings.export_defaults.quality, 50);
        assert!(settings.export_defaults.copy_to_clipboard);
    }
    #[test]
    fn settings_persist_and_replace_previous_folder_atomically() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config/settings.json");
        assert!(
            Settings::load_at(&path)
                .unwrap()
                .library_directory
                .is_none()
        );
        let mut settings = Settings {
            library_directory: Some(dir.path().join("first")),
            ..Settings::default()
        };
        settings.save_at(&path).unwrap();
        assert_eq!(
            Settings::load_at(&path).unwrap().library_directory,
            settings.library_directory
        );
        settings.library_directory = Some(dir.path().join("café 日本"));
        settings.save_at(&path).unwrap();
        assert_eq!(
            Settings::load_at(&path).unwrap().library_directory,
            settings.library_directory
        );
        assert_eq!(fs::read_dir(path.parent().unwrap()).unwrap().count(), 1);
        fs::write(&path, b"not valid JSON").unwrap();
        assert!(Settings::load_at(&path).is_err());
    }
}

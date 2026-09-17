//! Portable, atomic project persistence and autosave recovery.

use super::assets::AssetId;
use super::{CURRENT_SCHEMA_VERSION, Project, ProjectError};
use serde_json::Value;
use std::error::Error;
use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

const MAX_PROJECT_BYTES: u64 = 64 * 1024 * 1024;

/// An asset that could not be found after resolving a project-relative path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MissingAsset {
    pub id: AssetId,
    pub path: PathBuf,
    pub hint: Option<PathBuf>,
}

/// Result of loading a project.  Missing media is recoverable and does not
/// prevent the edit graph from opening or being relinked.
#[derive(Clone, Debug, PartialEq)]
pub struct LoadReport {
    pub project: Project,
    pub missing_assets: Vec<MissingAsset>,
    pub recovered: bool,
    pub source_path: PathBuf,
}

impl LoadReport {
    pub fn is_complete(&self) -> bool {
        self.missing_assets.is_empty()
    }

    pub fn missing(&self) -> &[MissingAsset] {
        &self.missing_assets
    }
}

/// Result of an atomic save.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SaveReport {
    pub path: PathBuf,
    pub bytes: u64,
}

/// Storage errors are explicit so a caller can offer recover/relink/cancel
/// instead of silently discarding the current composition.
#[derive(Debug)]
pub enum ProjectStorageError {
    Io(std::io::Error),
    Json(serde_json::Error),
    Project(ProjectError),
    InvalidPath(String),
    TooLarge(u64),
    UnsupportedSchema(u32),
}

impl fmt::Display for ProjectStorageError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => error.fmt(formatter),
            Self::Json(error) => error.fmt(formatter),
            Self::Project(error) => error.fmt(formatter),
            Self::InvalidPath(error) => formatter.write_str(error),
            Self::TooLarge(bytes) => write!(formatter, "project file is too large ({bytes} bytes)"),
            Self::UnsupportedSchema(version) => {
                write!(formatter, "unsupported project schema {version}")
            }
        }
    }
}

impl Error for ProjectStorageError {}

impl From<std::io::Error> for ProjectStorageError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<serde_json::Error> for ProjectStorageError {
    fn from(error: serde_json::Error) -> Self {
        Self::Json(error)
    }
}

impl From<ProjectError> for ProjectStorageError {
    fn from(error: ProjectError) -> Self {
        Self::Project(error)
    }
}

/// Save a project using a same-directory temporary and an atomic rename.
/// Media files are never copied or modified by this operation.
pub fn save_atomic(
    project: &Project,
    path: impl AsRef<Path>,
) -> Result<SaveReport, ProjectStorageError> {
    save_impl(project, path.as_ref())
}

pub fn save_project(
    project: &Project,
    path: impl AsRef<Path>,
) -> Result<SaveReport, ProjectStorageError> {
    save_atomic(project, path)
}

/// Save the autosave copy beside the project using a distinct recovery name.
pub fn save_recovery(
    project: &Project,
    path: impl AsRef<Path>,
) -> Result<SaveReport, ProjectStorageError> {
    let recovery = recovery_path(path.as_ref());
    save_impl(project, &recovery)
}

pub fn recovery_path(path: &Path) -> PathBuf {
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "project".to_owned());
    path.with_file_name(format!(".{name}.recovery.json"))
}

/// Load the authoritative project file and resolve all relative asset paths
/// against its containing directory.
pub fn load(path: impl AsRef<Path>) -> Result<LoadReport, ProjectStorageError> {
    load_project(path)
}

pub fn load_project(path: impl AsRef<Path>) -> Result<LoadReport, ProjectStorageError> {
    load_impl(path.as_ref(), false)
}

/// Load a recovery file while marking the report as recovered for UI prompts.
pub fn load_recovery(path: impl AsRef<Path>) -> Result<LoadReport, ProjectStorageError> {
    load_impl(path.as_ref(), true)
}

fn save_impl(project: &Project, path: &Path) -> Result<SaveReport, ProjectStorageError> {
    project.validate()?;
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;
    let portable = portable_project(project, parent);
    let bytes = serde_json::to_vec_pretty(&portable)?;
    let bytes_len =
        u64::try_from(bytes.len()).map_err(|_| ProjectStorageError::TooLarge(u64::MAX))?;
    if bytes_len > MAX_PROJECT_BYTES {
        return Err(ProjectStorageError::TooLarge(bytes_len));
    }
    let temp = temporary_path(path);
    let result = (|| -> Result<(), ProjectStorageError> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        fs::rename(&temp, path)?;
        // Best effort directory sync is intentionally omitted on platforms
        // where opening directories is not supported by std::fs.
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result?;
    Ok(SaveReport {
        path: path.to_owned(),
        bytes: bytes_len,
    })
}

fn load_impl(path: &Path, recovered: bool) -> Result<LoadReport, ProjectStorageError> {
    let metadata = fs::metadata(path)?;
    if !metadata.is_file() {
        return Err(ProjectStorageError::InvalidPath(format!(
            "project path is not a regular file: {}",
            path.display()
        )));
    }
    if metadata.len() > MAX_PROJECT_BYTES {
        return Err(ProjectStorageError::TooLarge(metadata.len()));
    }
    let bytes = fs::read(path)?;
    let mut value: Value = serde_json::from_slice(&bytes)?;
    migrate_value(&mut value)?;
    let mut project: Project = serde_json::from_value(value)?;
    project.validate()?;
    let base = path.parent().unwrap_or_else(|| Path::new("."));
    let mut missing_assets = Vec::new();
    let asset_ids = project.assets.iter().map(|(id, _)| *id).collect::<Vec<_>>();
    for id in asset_ids {
        let asset = project.assets.get(id).expect("asset exists");
        let stored = asset.path.clone();
        let resolved = if stored.is_absolute() {
            stored.clone()
        } else {
            base.join(&stored)
        };
        let hint = asset.path_hint.clone();
        let selected = if resolved.exists() {
            resolved
        } else if let Some(hint) = hint.as_ref().filter(|hint| hint.exists()) {
            hint.clone()
        } else {
            missing_assets.push(MissingAsset {
                id,
                path: resolved.clone(),
                hint: hint.clone(),
            });
            resolved
        };
        if let Some(asset) = project.assets.get_mut(id) {
            asset.path = selected;
        }
    }
    Ok(LoadReport {
        project,
        missing_assets,
        recovered,
        source_path: path.to_owned(),
    })
}

fn migrate_value(value: &mut Value) -> Result<(), ProjectStorageError> {
    let object = value.as_object_mut().ok_or_else(|| {
        ProjectStorageError::InvalidPath("project document must be a JSON object".to_owned())
    })?;
    let version = object
        .get("schema_version")
        .and_then(Value::as_u64)
        .ok_or_else(|| {
            ProjectStorageError::InvalidPath("project schema_version is missing".to_owned())
        })?;
    let version =
        u32::try_from(version).map_err(|_| ProjectStorageError::UnsupportedSchema(u32::MAX))?;
    if version != CURRENT_SCHEMA_VERSION {
        return Err(ProjectStorageError::UnsupportedSchema(version));
    }
    Ok(())
}

fn portable_project(project: &Project, base: &Path) -> Project {
    let mut portable = project.clone();
    let asset_ids = portable
        .assets
        .iter()
        .map(|(id, _)| *id)
        .collect::<Vec<_>>();
    for asset_id in asset_ids {
        let path = portable
            .assets
            .get(asset_id)
            .expect("asset exists")
            .path
            .clone();
        if path.is_absolute()
            && let Ok(relative) = path.strip_prefix(base).map(Path::to_owned)
        {
            let asset = portable.assets.get_mut(asset_id).expect("asset exists");
            asset.path_hint = Some(path);
            asset.path = relative;
        }
    }
    portable
}

fn temporary_path(path: &Path) -> PathBuf {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    let pid = std::process::id();
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    path.with_file_name(format!(".{name}.slicer-{pid}-{stamp}.tmp"))
}

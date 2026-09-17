//! Model-authoritative editor session used by desktop UI and headless tests.
//!
//! A session owns the mutable command history and the current project path;
//! views receive snapshots and never edit singleton media fields directly.

use crate::project::{
    Asset, AssetId, AssetKind, Clip, ClipId, CommandError, EditCommand, LoadReport, Project,
    ProjectHistory, ProjectStorageError, Time, TimeRange, Track, TrackId, load_project,
    recovery_path, save_atomic, save_recovery,
};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Debug)]
pub enum SessionError {
    Command(CommandError),
    Storage(ProjectStorageError),
    NoProjectPath,
    Invalid(String),
}

impl std::fmt::Display for SessionError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Command(error) => error.fmt(formatter),
            Self::Storage(error) => error.fmt(formatter),
            Self::NoProjectPath => formatter.write_str("the session has no project path"),
            Self::Invalid(error) => formatter.write_str(error),
        }
    }
}

impl std::error::Error for SessionError {}

impl From<CommandError> for SessionError {
    fn from(error: CommandError) -> Self {
        Self::Command(error)
    }
}

impl From<ProjectStorageError> for SessionError {
    fn from(error: ProjectStorageError) -> Self {
        Self::Storage(error)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CloseDecision {
    Close,
    PromptToSave,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ImportedMedia {
    pub asset_id: AssetId,
    pub track_id: TrackId,
    pub clip_id: ClipId,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AssetWorkToken {
    pub project_id: crate::project::ProjectId,
    pub asset_id: AssetId,
    pub generation: u64,
}

/// The one mutable owner of a project's edit graph for an editor window.
#[derive(Debug)]
pub struct ProjectSession {
    history: ProjectHistory,
    project_path: Option<PathBuf>,
    clean_revision: u64,
    recovered: bool,
    work_generations: BTreeMap<AssetId, u64>,
}

impl ProjectSession {
    pub fn new(project: Project) -> Result<Self, SessionError> {
        let clean_revision = project.revision;
        Ok(Self {
            history: ProjectHistory::try_new(project).map_err(SessionError::Command)?,
            project_path: None,
            clean_revision,
            recovered: false,
            work_generations: BTreeMap::new(),
        })
    }

    pub fn empty() -> Result<Self, SessionError> {
        Self::new(Project::new_empty())
    }

    pub fn open(path: impl AsRef<Path>) -> Result<(Self, LoadReport), SessionError> {
        let report = load_project(path)?;
        let mut session = Self::new(report.project.clone())?;
        session.project_path = Some(report.source_path.clone());
        session.clean_revision = report.project.revision;
        Ok((session, report))
    }

    /// Return whether a distinct recovery file exists and is newer than the
    /// saved project. A missing saved project is treated as recoverable when
    /// its recovery file is present.
    pub fn recovery_is_newer(path: impl AsRef<Path>) -> bool {
        let path = path.as_ref();
        let recovery = recovery_path(path);
        let Ok(recovery_modified) = std::fs::metadata(recovery).and_then(|meta| meta.modified())
        else {
            return false;
        };
        let Ok(project_modified) = std::fs::metadata(path).and_then(|meta| meta.modified()) else {
            return true;
        };
        recovery_modified > project_modified
    }

    /// Open the autosave snapshot while preserving the intended project path.
    /// The caller can present a save/discard choice; a recovered session is
    /// always dirty until it is explicitly saved.
    pub fn open_recovery(path: impl AsRef<Path>) -> Result<(Self, LoadReport), SessionError> {
        let project_path = path.as_ref().to_owned();
        let report = crate::project::load_recovery(recovery_path(&project_path))?;
        let mut session = Self::new(report.project.clone())?;
        session.project_path = Some(project_path);
        session.clean_revision = report.project.revision;
        session.recovered = true;
        Ok((session, report))
    }

    pub fn project(&self) -> &Project {
        self.history.project()
    }

    pub fn snapshot(&self) -> crate::project::ProjectSnapshot {
        self.project().snapshot()
    }

    pub fn project_path(&self) -> Option<&Path> {
        self.project_path.as_deref()
    }

    pub fn is_dirty(&self) -> bool {
        self.recovered || self.project().revision != self.clean_revision
    }

    pub fn close_decision(&self) -> CloseDecision {
        if self.is_dirty() {
            CloseDecision::PromptToSave
        } else {
            CloseDecision::Close
        }
    }

    pub fn can_undo(&self) -> bool {
        self.history.can_undo()
    }

    pub fn can_redo(&self) -> bool {
        self.history.can_redo()
    }

    pub fn execute(&mut self, command: EditCommand) -> Result<(), SessionError> {
        self.history.execute(command)?;
        Ok(())
    }

    pub fn execute_batch<I>(&mut self, commands: I) -> Result<(), SessionError>
    where
        I: IntoIterator<Item = EditCommand>,
    {
        self.history.execute_batch(commands)?;
        Ok(())
    }

    pub fn undo(&mut self) -> Result<bool, SessionError> {
        Ok(self.history.undo()?)
    }

    pub fn redo(&mut self) -> Result<bool, SessionError> {
        Ok(self.history.redo()?)
    }

    /// Import one inspected asset and create one independent timeline
    /// instance.  Reusing the same file later creates a distinct clip ID.
    pub fn import_asset(
        &mut self,
        mut asset: Asset,
        timeline_start: Time,
    ) -> Result<ImportedMedia, SessionError> {
        if timeline_start < Time::ZERO {
            return Err(SessionError::Invalid(
                "media cannot be placed before project time zero".to_owned(),
            ));
        }
        // Deserializing a project does not advance the process-local fresh-ID
        // counter. A CLI invocation that opens the project and then imports a
        // file can therefore generate IDs already present on disk. Allocate
        // from the persisted graph instead of trusting process-local counters.
        if asset.id.is_zero() || self.project().assets.contains(asset.id) {
            asset.id = self.next_asset_id()?;
        }
        let asset_id = asset.id;
        let track_id = self.next_track_id()?;
        let clip_id = self.next_clip_id()?;
        let clip = match (&asset.kind, asset.duration()) {
            (AssetKind::Video | AssetKind::Audio, Some(duration)) => {
                let range = TimeRange::from_start_duration(timeline_start, duration)
                    .map_err(|error| SessionError::Invalid(error.to_string()))?;
                if asset.kind == AssetKind::Video {
                    Clip::video(
                        clip_id,
                        range,
                        asset_id,
                        TimeRange::new(Time::ZERO, duration)
                            .map_err(|error| SessionError::Invalid(error.to_string()))?,
                    )
                } else {
                    Clip::audio(
                        clip_id,
                        range,
                        asset_id,
                        TimeRange::new(Time::ZERO, duration)
                            .map_err(|error| SessionError::Invalid(error.to_string()))?,
                    )
                }
            }
            (AssetKind::Image, None) => {
                let duration = Time::from_integer(5);
                let range = TimeRange::from_start_duration(timeline_start, duration)
                    .map_err(|error| SessionError::Invalid(error.to_string()))?;
                Clip::image(clip_id, range, asset_id)
            }
            _ => {
                return Err(SessionError::Invalid(
                    "asset metadata does not match its media kind".to_owned(),
                ));
            }
        };
        let track = Track::new(track_id, asset.kind.kind_name(), self.next_track_order());
        self.execute_batch([
            EditCommand::AddAsset { asset },
            EditCommand::AddTrack { track },
            EditCommand::AddClip { track_id, clip },
        ])?;
        Ok(ImportedMedia {
            asset_id,
            track_id,
            clip_id,
        })
    }

    pub fn relink(
        &mut self,
        asset_id: AssetId,
        path: impl Into<PathBuf>,
    ) -> Result<(), SessionError> {
        self.execute(EditCommand::RelinkAsset {
            asset_id,
            path: path.into(),
        })
    }

    pub fn begin_asset_work(&mut self, asset_id: AssetId) -> Result<AssetWorkToken, SessionError> {
        if self.project().asset(asset_id).is_none() {
            return Err(SessionError::Invalid(format!(
                "asset {asset_id} is not in this project"
            )));
        }
        let generation = self
            .work_generations
            .get(&asset_id)
            .copied()
            .unwrap_or(0)
            .saturating_add(1);
        self.work_generations.insert(asset_id, generation);
        Ok(AssetWorkToken {
            project_id: self.project().id,
            asset_id,
            generation,
        })
    }

    pub fn accepts_asset_work(&self, token: AssetWorkToken) -> bool {
        token.project_id == self.project().id
            && self.project().asset(token.asset_id).is_some()
            && self.work_generations.get(&token.asset_id).copied() == Some(token.generation)
    }

    pub fn save(&mut self) -> Result<PathBuf, SessionError> {
        let path = self
            .project_path
            .clone()
            .ok_or(SessionError::NoProjectPath)?;
        self.save_as(path)
    }

    pub fn save_as(&mut self, path: impl Into<PathBuf>) -> Result<PathBuf, SessionError> {
        let path = path.into();
        save_atomic(self.project(), &path)?;
        self.project_path = Some(path.clone());
        self.clean_revision = self.project().revision;
        self.recovered = false;
        Ok(path)
    }

    pub fn autosave(&self) -> Result<PathBuf, SessionError> {
        let path = self
            .project_path
            .as_deref()
            .ok_or(SessionError::NoProjectPath)?;
        Ok(save_recovery(self.project(), path)?.path)
    }

    pub fn recovery_path(&self) -> Result<PathBuf, SessionError> {
        Ok(recovery_path(
            self.project_path
                .as_deref()
                .ok_or(SessionError::NoProjectPath)?,
        ))
    }

    fn next_track_order(&self) -> i64 {
        self.project()
            .tracks
            .iter()
            .map(|track| track.order)
            .max()
            .unwrap_or(-1)
            .saturating_add(1)
    }

    fn next_asset_id(&self) -> Result<AssetId, SessionError> {
        let mut candidate = 1_u64;
        loop {
            let id = AssetId::new(candidate);
            if !self.project().assets.contains(id) {
                return Ok(id);
            }
            candidate = candidate.checked_add(1).ok_or_else(|| {
                SessionError::Invalid("project has no available asset IDs".to_owned())
            })?;
        }
    }

    fn next_track_id(&self) -> Result<TrackId, SessionError> {
        let mut candidate = 1_u64;
        loop {
            let id = TrackId::new(candidate);
            if self.project().track(id).is_none() {
                return Ok(id);
            }
            candidate = candidate.checked_add(1).ok_or_else(|| {
                SessionError::Invalid("project has no available track IDs".to_owned())
            })?;
        }
    }

    fn next_clip_id(&self) -> Result<ClipId, SessionError> {
        let mut candidate = 1_u64;
        loop {
            let id = ClipId::new(candidate);
            if !self.project().contains_clip(id) {
                return Ok(id);
            }
            candidate = candidate.checked_add(1).ok_or_else(|| {
                SessionError::Invalid("project has no available clip IDs".to_owned())
            })?;
        }
    }
}

trait AssetKindName {
    fn kind_name(&self) -> &'static str;
}

impl AssetKindName for AssetKind {
    fn kind_name(&self) -> &'static str {
        match self {
            Self::Video => "Video",
            Self::Image => "Image",
            Self::Audio => "Audio",
        }
    }
}

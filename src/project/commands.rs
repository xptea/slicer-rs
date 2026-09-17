//! Validated project edit commands and bounded undo/redo history.

use super::assets::{Asset, AssetId, ClipId, ProjectId, TrackId};
use super::clips::{Clip, ClipKind, Color, Track, Transform};
use super::time::{Time, TimeRange};
use super::{DurationPolicy, Project, ProjectError};
use serde::{Deserialize, Serialize};
use std::error::Error;
use std::fmt;
use std::path::PathBuf;

/// Serializable mutations understood by the model owner.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum EditCommand {
    AddAsset {
        asset: Asset,
    },
    RemoveAsset {
        asset_id: AssetId,
    },
    RelinkAsset {
        asset_id: AssetId,
        path: PathBuf,
    },
    AddTrack {
        track: Track,
    },
    RemoveTrack {
        track_id: TrackId,
    },
    AddClip {
        track_id: TrackId,
        clip: Clip,
    },
    DeleteClip {
        clip_id: ClipId,
    },
    DuplicateClip {
        clip_id: ClipId,
        new_clip_id: ClipId,
    },
    MoveClip {
        clip_id: ClipId,
        new_start: Time,
    },
    SetClipRange {
        clip_id: ClipId,
        range: TimeRange,
    },
    SetClipSourceRange {
        clip_id: ClipId,
        source_range: TimeRange,
    },
    TrimClip {
        clip_id: ClipId,
        range: TimeRange,
        source_range: Option<TimeRange>,
    },
    SplitClip {
        clip_id: ClipId,
        at: Time,
        new_clip_id: ClipId,
    },
    SetClipTransform {
        clip_id: ClipId,
        transform: Transform,
    },
    SetClipVisibility {
        clip_id: ClipId,
        visible: bool,
    },
    SetClipOrder {
        clip_id: ClipId,
        order: i64,
    },
    SetVideoAudio {
        clip_id: ClipId,
        settings: Option<super::clips::AudioSettings>,
    },
    SetTrackVisibility {
        track_id: TrackId,
        visible: bool,
    },
    SetTrackLocked {
        track_id: TrackId,
        locked: bool,
    },
    SetTrackOrder {
        track_id: TrackId,
        order: i64,
    },
    SetCanvas {
        canvas: super::clips::Canvas,
    },
    SetBackground {
        background: Color,
    },
    SetDurationPolicy {
        policy: DurationPolicy,
    },
    SetWorkRange {
        range: Option<TimeRange>,
    },
    SetExportRange {
        range: Option<TimeRange>,
    },
    /// A drag or text-edit session can be committed as one undo step.
    Batch {
        commands: Vec<EditCommand>,
    },
}

impl EditCommand {
    pub fn apply(&self, project: &mut Project) -> Result<CommandReceipt, CommandError> {
        let before = project.clone();
        let mut candidate = before.clone();
        self.apply_uncommitted(&mut candidate)?;
        candidate.validate()?;
        candidate.bump_revision_from(before.revision);
        *project = candidate;
        Ok(CommandReceipt {
            project_id: project.id,
            revision: project.revision,
        })
    }

    pub fn apply_uncommitted(&self, project: &mut Project) -> Result<(), CommandError> {
        match self {
            Self::AddAsset { asset } => {
                project.assets.insert(asset.clone())?;
            }
            Self::RemoveAsset { asset_id } => {
                if project
                    .tracks
                    .iter()
                    .flat_map(|track| track.clips.iter())
                    .any(|clip| clip.kind.asset_id() == Some(*asset_id))
                {
                    return Err(CommandError::AssetInUse(*asset_id));
                }
                if project.assets.remove(*asset_id).is_none() {
                    return Err(CommandError::Project(ProjectError::AssetNotFound(
                        *asset_id,
                    )));
                }
            }
            Self::RelinkAsset { asset_id, path } => {
                let asset = project.asset_mut(*asset_id).ok_or(CommandError::Project(
                    ProjectError::AssetNotFound(*asset_id),
                ))?;
                asset.set_path(path.clone())?;
            }
            Self::AddTrack { track } => {
                if track.id.is_zero() {
                    return Err(CommandError::Project(ProjectError::Clip(
                        super::clips::ClipError::ZeroTrackId,
                    )));
                }
                if project
                    .tracks
                    .iter()
                    .any(|existing| existing.id == track.id)
                {
                    return Err(CommandError::Project(ProjectError::DuplicateTrack(
                        track.id,
                    )));
                }
                project.tracks.push(track.clone());
            }
            Self::RemoveTrack { track_id } => {
                let position = project
                    .tracks
                    .iter()
                    .position(|track| track.id == *track_id)
                    .ok_or(CommandError::Project(ProjectError::TrackNotFound(
                        *track_id,
                    )))?;
                if project.tracks[position].locked {
                    return Err(CommandError::Project(ProjectError::TrackLocked(*track_id)));
                }
                project.tracks.remove(position);
            }
            Self::AddClip { track_id, clip } => {
                ensure_clip_can_change(project, *track_id)?;
                if project.contains_clip(clip.id) {
                    return Err(CommandError::Project(ProjectError::DuplicateClip(clip.id)));
                }
                let track = project.track_mut(*track_id).ok_or(CommandError::Project(
                    ProjectError::TrackNotFound(*track_id),
                ))?;
                track.add_clip(clip.clone())?;
            }
            Self::DeleteClip { clip_id } => {
                let track_id = project
                    .clip_track(*clip_id)
                    .ok_or(CommandError::Project(ProjectError::ClipNotFound(*clip_id)))?;
                ensure_clip_can_change(project, track_id)?;
                let track = project
                    .track_mut(track_id)
                    .ok_or(CommandError::Project(ProjectError::TrackNotFound(track_id)))?;
                track.remove_clip(*clip_id);
            }
            Self::DuplicateClip {
                clip_id,
                new_clip_id,
            } => {
                if new_clip_id.is_zero() || project.contains_clip(*new_clip_id) {
                    return Err(CommandError::DuplicateClip(*new_clip_id));
                }
                let track_id = project
                    .clip_track(*clip_id)
                    .ok_or(CommandError::Project(ProjectError::ClipNotFound(*clip_id)))?;
                ensure_clip_can_change(project, track_id)?;
                let mut duplicate = project
                    .clip(*clip_id)
                    .ok_or(CommandError::Project(ProjectError::ClipNotFound(*clip_id)))?
                    .clone();
                duplicate.id = *new_clip_id;
                duplicate.order = duplicate.order.saturating_add(1);
                project
                    .track_mut(track_id)
                    .ok_or(CommandError::Project(ProjectError::TrackNotFound(track_id)))?
                    .add_clip(duplicate)?;
            }
            Self::MoveClip { clip_id, new_start } => {
                let track_id = project
                    .clip_track(*clip_id)
                    .ok_or(CommandError::Project(ProjectError::ClipNotFound(*clip_id)))?;
                ensure_clip_can_change(project, track_id)?;
                if *new_start < Time::ZERO {
                    return Err(CommandError::Invalid(
                        "clip start must be non-negative".to_owned(),
                    ));
                }
                let clip = project
                    .clip_mut(*clip_id)
                    .ok_or(CommandError::Project(ProjectError::ClipNotFound(*clip_id)))?;
                let duration = clip.duration()?;
                clip.range = TimeRange::from_start_duration(*new_start, duration)?;
            }
            Self::SetClipRange { clip_id, range } => {
                let track_id = project
                    .clip_track(*clip_id)
                    .ok_or(CommandError::Project(ProjectError::ClipNotFound(*clip_id)))?;
                ensure_clip_can_change(project, track_id)?;
                let clip = project
                    .clip_mut(*clip_id)
                    .ok_or(CommandError::Project(ProjectError::ClipNotFound(*clip_id)))?;
                if range.start < Time::ZERO {
                    return Err(CommandError::Invalid(
                        "clip start must be non-negative".to_owned(),
                    ));
                }
                if clip.duration()? != range.duration()? {
                    return Err(CommandError::Invalid(
                        "moving a clip cannot change its duration; use TrimClip".to_owned(),
                    ));
                }
                clip.range = *range;
            }
            Self::SetClipSourceRange {
                clip_id,
                source_range,
            } => {
                let track_id = project
                    .clip_track(*clip_id)
                    .ok_or(CommandError::Project(ProjectError::ClipNotFound(*clip_id)))?;
                ensure_clip_can_change(project, track_id)?;
                let clip = project
                    .clip_mut(*clip_id)
                    .ok_or(CommandError::Project(ProjectError::ClipNotFound(*clip_id)))?;
                let Some(current) = clip.source_range_ref() else {
                    return Err(CommandError::Invalid(
                        "only source-backed clips have a source range".to_owned(),
                    ));
                };
                if current.duration()? != source_range.duration()? {
                    return Err(CommandError::Invalid(
                        "source trim duration must match the timeline duration".to_owned(),
                    ));
                }
                *clip.source_range_mut().expect("checked above") = *source_range;
            }
            Self::TrimClip {
                clip_id,
                range,
                source_range,
            } => {
                let track_id = project
                    .clip_track(*clip_id)
                    .ok_or(CommandError::Project(ProjectError::ClipNotFound(*clip_id)))?;
                ensure_clip_can_change(project, track_id)?;
                if range.start < Time::ZERO {
                    return Err(CommandError::Invalid(
                        "clip start must be non-negative".to_owned(),
                    ));
                }
                let clip = project
                    .clip_mut(*clip_id)
                    .ok_or(CommandError::Project(ProjectError::ClipNotFound(*clip_id)))?;
                let duration = range.duration()?;
                match (clip.source_range_ref(), source_range) {
                    (Some(_current), Some(source_range)) => {
                        if source_range.duration()? != duration {
                            return Err(CommandError::Invalid(
                                "timeline and source trim durations must match".to_owned(),
                            ));
                        }
                        *clip.source_range_mut().expect("source range exists") = *source_range;
                    }
                    (Some(_), None) => {
                        return Err(CommandError::Invalid(
                            "source-backed trim must provide a source range".to_owned(),
                        ));
                    }
                    (None, Some(_)) => {
                        return Err(CommandError::Invalid(
                            "non-source clip cannot provide a source range".to_owned(),
                        ));
                    }
                    (None, None) => {}
                }
                clip.range = *range;
            }
            Self::SplitClip {
                clip_id,
                at,
                new_clip_id,
            } => split_clip(project, *clip_id, *at, *new_clip_id)?,
            Self::SetClipTransform { clip_id, transform } => {
                let track_id = project
                    .clip_track(*clip_id)
                    .ok_or(CommandError::Project(ProjectError::ClipNotFound(*clip_id)))?;
                ensure_clip_can_change(project, track_id)?;
                project
                    .clip_mut(*clip_id)
                    .ok_or(CommandError::Project(ProjectError::ClipNotFound(*clip_id)))?
                    .transform = *transform;
            }
            Self::SetClipVisibility { clip_id, visible } => {
                let track_id = project
                    .clip_track(*clip_id)
                    .ok_or(CommandError::Project(ProjectError::ClipNotFound(*clip_id)))?;
                ensure_clip_can_change(project, track_id)?;
                project
                    .clip_mut(*clip_id)
                    .ok_or(CommandError::Project(ProjectError::ClipNotFound(*clip_id)))?
                    .visible = *visible;
            }
            Self::SetClipOrder { clip_id, order } => {
                let track_id = project
                    .clip_track(*clip_id)
                    .ok_or(CommandError::Project(ProjectError::ClipNotFound(*clip_id)))?;
                ensure_clip_can_change(project, track_id)?;
                project
                    .clip_mut(*clip_id)
                    .ok_or(CommandError::Project(ProjectError::ClipNotFound(*clip_id)))?
                    .order = *order;
            }
            Self::SetVideoAudio { clip_id, settings } => {
                let track_id = project
                    .clip_track(*clip_id)
                    .ok_or(CommandError::Project(ProjectError::ClipNotFound(*clip_id)))?;
                ensure_clip_can_change(project, track_id)?;
                let clip = project
                    .clip_mut(*clip_id)
                    .ok_or(CommandError::Project(ProjectError::ClipNotFound(*clip_id)))?;
                let ClipKind::Video(video) = &mut clip.kind else {
                    return Err(CommandError::Invalid(
                        "embedded audio settings require a video clip".to_owned(),
                    ));
                };
                video.audio = settings
                    .map(super::clips::EmbeddedAudio::Enabled)
                    .unwrap_or(super::clips::EmbeddedAudio::Disabled);
            }
            Self::SetTrackVisibility { track_id, visible } => {
                let track = project.track_mut(*track_id).ok_or(CommandError::Project(
                    ProjectError::TrackNotFound(*track_id),
                ))?;
                track.visible = *visible;
            }
            Self::SetTrackLocked { track_id, locked } => {
                let track = project.track_mut(*track_id).ok_or(CommandError::Project(
                    ProjectError::TrackNotFound(*track_id),
                ))?;
                track.locked = *locked;
            }
            Self::SetTrackOrder { track_id, order } => {
                let track = project.track_mut(*track_id).ok_or(CommandError::Project(
                    ProjectError::TrackNotFound(*track_id),
                ))?;
                track.order = *order;
            }
            Self::SetCanvas { canvas } => {
                project.canvas = *canvas;
            }
            Self::SetBackground { background } => {
                project.background = *background;
            }
            Self::SetDurationPolicy { policy } => {
                project.duration_policy = policy.clone();
            }
            Self::SetWorkRange { range } => {
                project.work_range = *range;
            }
            Self::SetExportRange { range } => {
                project.export_range = *range;
            }
            Self::Batch { commands } => {
                for command in commands {
                    command.apply_uncommitted(project)?;
                }
            }
        }
        Ok(())
    }
}

fn ensure_clip_can_change(project: &Project, track_id: TrackId) -> Result<(), CommandError> {
    let track = project
        .track(track_id)
        .ok_or(CommandError::Project(ProjectError::TrackNotFound(track_id)))?;
    if track.locked {
        return Err(CommandError::Project(ProjectError::TrackLocked(track_id)));
    }
    Ok(())
}

fn split_clip(
    project: &mut Project,
    clip_id: ClipId,
    at: Time,
    new_clip_id: ClipId,
) -> Result<(), CommandError> {
    if new_clip_id.is_zero() || project.contains_clip(new_clip_id) {
        return Err(CommandError::DuplicateClip(new_clip_id));
    }
    let track_id = project
        .clip_track(clip_id)
        .ok_or(CommandError::Project(ProjectError::ClipNotFound(clip_id)))?;
    ensure_clip_can_change(project, track_id)?;
    let original = project
        .clip(clip_id)
        .ok_or(CommandError::Project(ProjectError::ClipNotFound(clip_id)))?
        .clone();
    if at <= original.range.start || at >= original.range.end {
        return Err(CommandError::Invalid(
            "split point must be strictly inside the clip interval".to_owned(),
        ));
    }
    let left_range = TimeRange::new(original.range.start, at)?;
    let right_range = TimeRange::new(at, original.range.end)?;
    let elapsed = at.checked_sub(original.range.start)?;
    let (left_source, right_source) = if let Some(source_range) = original.kind.source_range() {
        let source_split = source_range.start.checked_add(elapsed)?;
        (
            Some(TimeRange::new(source_range.start, source_split)?),
            Some(TimeRange::new(source_split, source_range.end)?),
        )
    } else {
        (None, None)
    };

    let mut left = original.clone();
    left.range = left_range;
    if let Some(source) = left_source {
        *left.source_range_mut().expect("source range exists") = source;
    }

    let mut right = original;
    right.id = new_clip_id;
    right.range = right_range;
    right.order = right.order.saturating_add(1);
    if let Some(source) = right_source {
        *right.source_range_mut().expect("source range exists") = source;
    }

    let track = project
        .track_mut(track_id)
        .ok_or(CommandError::Project(ProjectError::TrackNotFound(track_id)))?;
    let position = track
        .clips
        .iter()
        .position(|clip| clip.id == clip_id)
        .ok_or(CommandError::Project(ProjectError::ClipNotFound(clip_id)))?;
    track.clips[position] = left;
    track.clips.insert(position + 1, right);
    Ok(())
}

/// Result of a committed command.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CommandReceipt {
    pub project_id: ProjectId,
    pub revision: u64,
}

/// A bounded before/after record.  The project model is metadata-sized and a
/// full snapshot keeps undo exact without inventing lossy inverse commands.
#[derive(Clone, Debug, PartialEq)]
pub struct HistoryEntry {
    pub command: EditCommand,
    pub before: Project,
    pub after: Project,
}

/// Model owner for command execution and undo/redo.
#[derive(Clone, Debug)]
pub struct ProjectHistory {
    project: Project,
    undo: Vec<HistoryEntry>,
    redo: Vec<HistoryEntry>,
    max_entries: usize,
}

impl ProjectHistory {
    pub fn new(project: Project) -> Self {
        Self {
            project,
            undo: Vec::new(),
            redo: Vec::new(),
            max_entries: 128,
        }
    }

    pub fn try_new(project: Project) -> Result<Self, CommandError> {
        project.validate()?;
        Ok(Self::new(project))
    }

    pub fn with_capacity(project: Project, max_entries: usize) -> Self {
        Self {
            max_entries,
            ..Self::new(project)
        }
    }

    pub fn project(&self) -> &Project {
        &self.project
    }

    pub fn project_mut(&mut self) -> &mut Project {
        &mut self.project
    }

    pub fn into_project(self) -> Project {
        self.project
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    pub fn undo_len(&self) -> usize {
        self.undo.len()
    }

    pub fn redo_len(&self) -> usize {
        self.redo.len()
    }

    pub fn execute(&mut self, command: EditCommand) -> Result<CommandReceipt, CommandError> {
        let before = self.project.clone();
        let mut after = before.clone();
        command.apply_uncommitted(&mut after)?;
        after.validate()?;
        after.bump_revision_from(before.revision);
        self.project = after.clone();
        self.undo.push(HistoryEntry {
            command,
            before,
            after: after.clone(),
        });
        self.redo.clear();
        self.trim_undo();
        Ok(CommandReceipt {
            project_id: self.project.id,
            revision: self.project.revision,
        })
    }

    pub fn execute_batch<I>(&mut self, commands: I) -> Result<CommandReceipt, CommandError>
    where
        I: IntoIterator<Item = EditCommand>,
    {
        self.execute(EditCommand::Batch {
            commands: commands.into_iter().collect(),
        })
    }

    pub fn undo(&mut self) -> Result<bool, CommandError> {
        let Some(entry) = self.undo.pop() else {
            return Ok(false);
        };
        let before_revision = self.project.revision;
        self.project
            .replace_from_history(entry.before.clone(), before_revision);
        self.redo.push(entry);
        Ok(true)
    }

    pub fn redo(&mut self) -> Result<bool, CommandError> {
        let Some(entry) = self.redo.pop() else {
            return Ok(false);
        };
        let before_revision = self.project.revision;
        self.project
            .replace_from_history(entry.after.clone(), before_revision);
        self.undo.push(entry);
        self.trim_undo();
        Ok(true)
    }

    fn trim_undo(&mut self) {
        if self.max_entries == 0 {
            self.undo.clear();
            return;
        }
        let excess = self.undo.len().saturating_sub(self.max_entries);
        if excess > 0 {
            self.undo.drain(..excess);
        }
    }
}

/// Command validation failures.
#[derive(Clone, Debug, PartialEq)]
pub enum CommandError {
    Project(ProjectError),
    Asset(super::assets::AssetError),
    Clip(super::clips::ClipError),
    Time(super::RationalError),
    Invalid(String),
    AssetInUse(AssetId),
    DuplicateClip(ClipId),
}

impl fmt::Display for CommandError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Project(error) => error.fmt(formatter),
            Self::Asset(error) => error.fmt(formatter),
            Self::Clip(error) => error.fmt(formatter),
            Self::Time(error) => error.fmt(formatter),
            Self::Invalid(error) => formatter.write_str(error),
            Self::AssetInUse(id) => write!(formatter, "asset {id} is still used by a clip"),
            Self::DuplicateClip(id) => write!(formatter, "clip {id} is already present"),
        }
    }
}

impl Error for CommandError {}

impl From<ProjectError> for CommandError {
    fn from(error: ProjectError) -> Self {
        Self::Project(error)
    }
}

impl From<super::assets::AssetError> for CommandError {
    fn from(error: super::assets::AssetError) -> Self {
        Self::Asset(error)
    }
}

impl From<super::clips::ClipError> for CommandError {
    fn from(error: super::clips::ClipError) -> Self {
        Self::Clip(error)
    }
}

impl From<super::RationalError> for CommandError {
    fn from(error: super::RationalError) -> Self {
        Self::Time(error)
    }
}

impl From<super::TimeRangeError> for CommandError {
    fn from(error: super::TimeRangeError) -> Self {
        Self::Invalid(error.to_string())
    }
}

impl From<super::SceneError> for CommandError {
    fn from(error: super::SceneError) -> Self {
        Self::Invalid(error.to_string())
    }
}

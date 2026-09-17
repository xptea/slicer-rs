//! Model-authoritative layered project contracts.
//!
//! This module deliberately has no GPUI, FFmpeg, GPU, or application-state
//! dependency.  It can be used by preview, export, and UI adapters through an
//! immutable [`ProjectSnapshot`].

pub mod adapter;
pub mod assets;
pub mod clips;
pub mod commands;
pub mod scene;
pub mod storage;
pub mod time;

pub use adapter::{LegacyVideoEdit, SingleVideoAdapter};
pub use assets::{
    Asset, AssetError, AssetId, AssetKind, AssetMetadata, AssetRegistry, AudioMetadata,
    CacheMetadata, ClipId, ImageMetadata, Orientation, ProjectId, ProxyMetadata, SourceFrame,
    SourceFrameSelection, SourceTimestamp, TrackId, VideoMetadata,
};
pub use clips::{
    AudioClip, AudioSettings, Canvas, Clip, ClipError, ClipKind, Color, CropRect, EmbeddedAudio,
    ImageClip, Point, ShapeClip, ShapeKind, Stroke, TextAlignment, TextClip, TextStyle, Track,
    TrackKind, Transform, VideoClip,
};
pub use commands::{CommandError, CommandReceipt, EditCommand, HistoryEntry, ProjectHistory};
pub use scene::{AudioInterval, DrawItem, SceneError, SceneKind, SceneSnapshot, StackOrder};
pub use storage::{
    LoadReport, MissingAsset, ProjectStorageError, SaveReport, load, load_project, load_recovery,
    recovery_path, save_atomic, save_project, save_recovery,
};
pub use time::{
    FrameRate, FrameRateError, Rational, RationalError, RationalTime, Time, TimeRange,
    TimeRangeError,
};

use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::error::Error;
use std::fmt;

/// Current on-disk project schema.
pub const CURRENT_SCHEMA_VERSION: u32 = 1;

/// Whether an explicit value or the content bounds determine project length.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub enum DurationPolicy {
    #[default]
    LongestClip,
    Explicit {
        duration: Time,
    },
    WorkRange,
}

impl DurationPolicy {
    pub fn explicit(duration: Time) -> Self {
        Self::Explicit { duration }
    }

    pub fn validate(&self) -> Result<(), ProjectError> {
        if let Self::Explicit { duration } = self
            && *duration < Time::ZERO
        {
            return Err(ProjectError::InvalidDuration);
        }
        Ok(())
    }
}

/// The authoritative layered project.  `tracks` are model entries, not UI
/// rows; their `order` and each clip's `order` define deterministic stacking.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Project {
    pub schema_version: u32,
    pub id: ProjectId,
    pub revision: u64,
    pub assets: AssetRegistry,
    pub tracks: Vec<Track>,
    pub canvas: Canvas,
    pub frame_rate: FrameRate,
    pub duration_policy: DurationPolicy,
    pub work_range: Option<TimeRange>,
    pub export_range: Option<TimeRange>,
    pub background: Color,
}

impl Project {
    pub fn new(canvas: Canvas, frame_rate: FrameRate) -> Self {
        Self {
            schema_version: CURRENT_SCHEMA_VERSION,
            id: ProjectId::fresh(),
            revision: 0,
            assets: AssetRegistry::new(),
            tracks: Vec::new(),
            canvas,
            frame_rate,
            duration_policy: DurationPolicy::LongestClip,
            work_range: None,
            export_range: None,
            background: Color::BLACK,
        }
    }

    pub fn new_empty() -> Self {
        Self::new(
            Canvas {
                width: 1_920,
                height: 1_080,
            },
            FrameRate::FPS_30,
        )
    }

    pub fn snapshot(&self) -> ProjectSnapshot {
        ProjectSnapshot::from(self)
    }

    pub fn add_asset(&mut self, asset: Asset) -> Result<(), ProjectError> {
        let mut candidate = self.clone();
        candidate.assets.insert(asset)?;
        candidate.validate()?;
        *self = candidate;
        Ok(())
    }

    pub fn add_track(&mut self, track: Track) -> Result<(), ProjectError> {
        let mut candidate = self.clone();
        if track.id.is_zero() {
            return Err(ProjectError::Clip(ClipError::ZeroTrackId));
        }
        if candidate
            .tracks
            .iter()
            .any(|existing| existing.id == track.id)
        {
            return Err(ProjectError::DuplicateTrack(track.id));
        }
        candidate.tracks.push(track);
        candidate.validate()?;
        *self = candidate;
        Ok(())
    }

    pub fn add_clip(&mut self, track_id: TrackId, clip: Clip) -> Result<(), ProjectError> {
        let mut candidate = self.clone();
        let locked = candidate
            .track(track_id)
            .ok_or(ProjectError::TrackNotFound(track_id))?
            .locked;
        if locked {
            return Err(ProjectError::TrackLocked(track_id));
        }
        if candidate
            .tracks
            .iter()
            .any(|track| track.clips.iter().any(|existing| existing.id == clip.id))
        {
            return Err(ProjectError::DuplicateClip(clip.id));
        }
        candidate
            .track_mut(track_id)
            .expect("track checked above")
            .add_clip(clip)?;
        candidate.validate()?;
        *self = candidate;
        Ok(())
    }

    pub fn remove_clip(
        &mut self,
        clip_id: super::project::assets::ClipId,
    ) -> Result<Clip, ProjectError> {
        let mut candidate = self.clone();
        for track in &mut candidate.tracks {
            if track.clip(clip_id).is_some() {
                if track.locked {
                    return Err(ProjectError::TrackLocked(track.id));
                }
                let clip = track
                    .remove_clip(clip_id)
                    .ok_or(ProjectError::ClipNotFound(clip_id))?;
                candidate.validate()?;
                *self = candidate;
                return Ok(clip);
            }
        }
        Err(ProjectError::ClipNotFound(clip_id))
    }

    pub fn remove_track(&mut self, track_id: TrackId) -> Result<Track, ProjectError> {
        let mut candidate = self.clone();
        let position = candidate
            .tracks
            .iter()
            .position(|track| track.id == track_id)
            .ok_or(ProjectError::TrackNotFound(track_id))?;
        if candidate.tracks[position].locked {
            return Err(ProjectError::TrackLocked(track_id));
        }
        let track = candidate.tracks.remove(position);
        candidate.validate()?;
        *self = candidate;
        Ok(track)
    }

    pub fn asset(&self, id: AssetId) -> Option<&Asset> {
        self.assets.get(id)
    }

    pub fn asset_mut(&mut self, id: AssetId) -> Option<&mut Asset> {
        self.assets.get_mut(id)
    }

    pub fn track(&self, id: TrackId) -> Option<&Track> {
        self.tracks.iter().find(|track| track.id == id)
    }

    pub fn track_mut(&mut self, id: TrackId) -> Option<&mut Track> {
        self.tracks.iter_mut().find(|track| track.id == id)
    }

    pub fn clip(&self, id: assets::ClipId) -> Option<&Clip> {
        self.tracks.iter().find_map(|track| track.clip(id))
    }

    pub fn clip_mut(&mut self, id: assets::ClipId) -> Option<&mut Clip> {
        self.tracks.iter_mut().find_map(|track| track.clip_mut(id))
    }

    pub fn clip_track(&self, id: assets::ClipId) -> Option<TrackId> {
        self.tracks
            .iter()
            .find(|track| track.clip(id).is_some())
            .map(|track| track.id)
    }

    pub fn contains_clip(&self, id: assets::ClipId) -> bool {
        self.clip(id).is_some()
    }

    pub fn effective_duration(&self) -> Time {
        match self.duration_policy {
            DurationPolicy::Explicit { duration } => duration,
            DurationPolicy::LongestClip => self.longest_clip_end(),
            DurationPolicy::WorkRange => self
                .work_range
                .map(|range| range.end)
                .unwrap_or_else(|| self.longest_clip_end()),
        }
    }

    pub fn duration(&self) -> Time {
        self.effective_duration()
    }

    pub fn output_range(&self) -> Option<TimeRange> {
        self.export_range.or_else(|| {
            let duration = self.effective_duration();
            TimeRange::new(Time::ZERO, duration).ok()
        })
    }

    pub fn longest_clip_end(&self) -> Time {
        self.tracks
            .iter()
            .flat_map(|track| track.clips.iter().map(|clip| clip.range.end))
            .max()
            .unwrap_or(Time::ZERO)
    }

    pub fn source_time_at(
        &self,
        clip_id: assets::ClipId,
        project_time: Time,
    ) -> Result<Option<Time>, ProjectError> {
        let clip = self
            .clip(clip_id)
            .ok_or(ProjectError::ClipNotFound(clip_id))?;
        clip.source_time_at(project_time)
            .map_err(ProjectError::Time)
    }

    pub fn evaluate_scene(&self, time: Time) -> Result<SceneSnapshot, ProjectError> {
        scene::evaluate(self, time).map_err(ProjectError::Scene)
    }

    pub fn audio_intervals(&self, range: TimeRange) -> Result<Vec<AudioInterval>, ProjectError> {
        scene::audio_intervals(self, range).map_err(ProjectError::Scene)
    }

    pub fn relink_asset(
        &mut self,
        asset_id: AssetId,
        path: impl Into<std::path::PathBuf>,
    ) -> Result<(), ProjectError> {
        let mut candidate = self.clone();
        let asset = candidate
            .asset_mut(asset_id)
            .ok_or(ProjectError::AssetNotFound(asset_id))?;
        asset.set_path(path)?;
        candidate.validate()?;
        *self = candidate;
        Ok(())
    }

    pub fn validate(&self) -> Result<(), ProjectError> {
        if self.schema_version != CURRENT_SCHEMA_VERSION {
            return Err(ProjectError::UnsupportedSchema(self.schema_version));
        }
        if self.id.is_zero() {
            return Err(ProjectError::InvalidProjectId);
        }
        self.assets.validate()?;
        Canvas::new(self.canvas.width, self.canvas.height)?;
        FrameRate::new(self.frame_rate.numerator, self.frame_rate.denominator)?;
        self.background.validate()?;
        self.duration_policy.validate()?;
        validate_optional_range(self.work_range)?;
        validate_optional_range(self.export_range)?;

        let duration = self.effective_duration();
        if let Some(range) = self.work_range
            && range.end > duration
            && !matches!(self.duration_policy, DurationPolicy::LongestClip)
        {
            return Err(ProjectError::RangeOutsideDuration);
        }
        if let Some(range) = self.export_range
            && range.end > duration
            && !matches!(self.duration_policy, DurationPolicy::LongestClip)
        {
            return Err(ProjectError::RangeOutsideDuration);
        }

        let mut track_ids = BTreeSet::new();
        let mut clip_ids = BTreeSet::new();
        for track in &self.tracks {
            track.validate()?;
            if !track_ids.insert(track.id) {
                return Err(ProjectError::DuplicateTrack(track.id));
            }
            for clip in &track.clips {
                if !clip_ids.insert(clip.id) {
                    return Err(ProjectError::DuplicateClip(clip.id));
                }
                if let Some(source_range) = clip.kind.source_range() {
                    let clip_duration = clip.range.duration()?;
                    let source_duration = source_range.duration()?;
                    if clip_duration != source_duration {
                        return Err(ProjectError::SourceDurationMismatch(clip.id));
                    }
                    if source_range.start < Time::ZERO {
                        return Err(ProjectError::SourceOutOfBounds(clip.id));
                    }
                }

                if let Some(asset_id) = clip.kind.asset_id() {
                    let asset = self
                        .assets
                        .get(asset_id)
                        .ok_or(ProjectError::AssetNotFound(asset_id))?;
                    if !clip.asset_kind_compatible(asset) {
                        return Err(ProjectError::AssetKindMismatch {
                            clip_id: clip.id,
                            asset_id,
                        });
                    }
                    validate_clip_against_asset(clip, asset)?;
                } else if clip.transform.crop.is_some() && !matches!(clip.kind, ClipKind::Image(_))
                {
                    return Err(ProjectError::CropRequiresMedia(clip.id));
                }

                if let ClipKind::Video(video) = &clip.kind
                    && let Some(asset) = self.assets.get(video.asset_id)
                    && let Some(metadata) = asset.video_metadata()
                    && let Ok(source_duration) = video.source_range.duration()
                    && let Ok(source_end) = video.source_range.start.checked_add(source_duration)
                    && source_end > metadata.duration
                {
                    return Err(ProjectError::SourceOutOfBounds(clip.id));
                }
            }
        }
        Ok(())
    }

    pub(crate) fn bump_revision_from(&mut self, previous: u64) {
        self.revision = previous.saturating_add(1);
    }

    pub(crate) fn replace_from_history(&mut self, mut project: Project, previous: u64) {
        project.revision = previous.saturating_add(1);
        *self = project;
    }
}

fn validate_clip_against_asset(clip: &Clip, asset: &Asset) -> Result<(), ProjectError> {
    if let Some(source_range) = clip.kind.source_range() {
        let source_duration = asset
            .duration()
            .ok_or(ProjectError::SourceOutOfBounds(clip.id))?;
        if source_range.end > source_duration {
            return Err(ProjectError::SourceOutOfBounds(clip.id));
        }
    }
    let (width, height) = match &asset.metadata {
        AssetMetadata::Video(metadata) => (metadata.width, metadata.height),
        AssetMetadata::Image(metadata) => (metadata.width, metadata.height),
        AssetMetadata::Audio(_) => (0, 0),
    };
    if let Some(crop) = clip.transform.crop
        && (width == 0 || height == 0 || !crop.fits_within(width, height)?)
    {
        return Err(ProjectError::CropOutOfBounds(clip.id));
    }
    Ok(())
}

fn validate_optional_range(range: Option<TimeRange>) -> Result<(), ProjectError> {
    if let Some(range) = range {
        if range.start < Time::ZERO {
            return Err(ProjectError::NegativeRange);
        }
        range.duration()?;
    }
    Ok(())
}

/// A project copy with a stable revision, suitable for handing to work that
/// may outlive the mutable editor session.
#[derive(Clone, Debug, PartialEq)]
pub struct ProjectSnapshot {
    pub project_id: ProjectId,
    pub revision: u64,
    pub project: Project,
}

impl From<&Project> for ProjectSnapshot {
    fn from(project: &Project) -> Self {
        Self {
            project_id: project.id,
            revision: project.revision,
            project: project.clone(),
        }
    }
}

impl ProjectSnapshot {
    pub fn validate(&self) -> Result<(), ProjectError> {
        self.project.validate()?;
        if self.project.id != self.project_id || self.project.revision != self.revision {
            return Err(ProjectError::SnapshotMismatch);
        }
        Ok(())
    }

    pub fn evaluate_scene(&self, time: Time) -> Result<SceneSnapshot, ProjectError> {
        self.project.evaluate_scene(time)
    }
}

/// Project-model failures.  Storage wraps these in recoverable load/save
/// errors while command callers can match the semantic variants directly.
#[derive(Clone, Debug, PartialEq)]
pub enum ProjectError {
    InvalidProjectId,
    UnsupportedSchema(u32),
    InvalidDuration,
    NegativeRange,
    RangeOutsideDuration,
    SnapshotMismatch,
    DuplicateTrack(TrackId),
    DuplicateClip(assets::ClipId),
    TrackNotFound(TrackId),
    ClipNotFound(assets::ClipId),
    AssetNotFound(AssetId),
    TrackLocked(TrackId),
    AssetKindMismatch {
        clip_id: assets::ClipId,
        asset_id: AssetId,
    },
    SourceDurationMismatch(assets::ClipId),
    SourceOutOfBounds(assets::ClipId),
    CropOutOfBounds(assets::ClipId),
    CropRequiresMedia(assets::ClipId),
    Asset(AssetError),
    Clip(ClipError),
    Time(RationalError),
    FrameRate(FrameRateError),
    Scene(SceneError),
}

impl fmt::Display for ProjectError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidProjectId => formatter.write_str("project id must be non-zero"),
            Self::UnsupportedSchema(version) => {
                write!(formatter, "unsupported project schema {version}")
            }
            Self::InvalidDuration => formatter.write_str("project duration must be non-negative"),
            Self::NegativeRange => formatter.write_str("project ranges must be non-negative"),
            Self::RangeOutsideDuration => {
                formatter.write_str("project range extends beyond project duration")
            }
            Self::SnapshotMismatch => {
                formatter.write_str("project snapshot identity does not match its contents")
            }
            Self::DuplicateTrack(id) => write!(formatter, "track {id} is already present"),
            Self::DuplicateClip(id) => write!(formatter, "clip {id} is already present"),
            Self::TrackNotFound(id) => write!(formatter, "track {id} was not found"),
            Self::ClipNotFound(id) => write!(formatter, "clip {id} was not found"),
            Self::AssetNotFound(id) => write!(formatter, "asset {id} was not found"),
            Self::TrackLocked(id) => write!(formatter, "track {id} is locked"),
            Self::AssetKindMismatch { clip_id, asset_id } => {
                write!(formatter, "clip {clip_id} cannot use asset {asset_id}")
            }
            Self::SourceDurationMismatch(id) => {
                write!(formatter, "clip {id} timeline and source durations differ")
            }
            Self::SourceOutOfBounds(id) => {
                write!(formatter, "clip {id} source range is outside its asset")
            }
            Self::CropOutOfBounds(id) => write!(formatter, "clip {id} crop is outside its asset"),
            Self::CropRequiresMedia(id) => {
                write!(formatter, "clip {id} crop requires image or video media")
            }
            Self::Asset(error) => error.fmt(formatter),
            Self::Clip(error) => error.fmt(formatter),
            Self::Time(error) => error.fmt(formatter),
            Self::FrameRate(error) => error.fmt(formatter),
            Self::Scene(error) => error.fmt(formatter),
        }
    }
}

impl Error for ProjectError {}

impl From<AssetError> for ProjectError {
    fn from(error: AssetError) -> Self {
        Self::Asset(error)
    }
}

impl From<ClipError> for ProjectError {
    fn from(error: ClipError) -> Self {
        Self::Clip(error)
    }
}

impl From<RationalError> for ProjectError {
    fn from(error: RationalError) -> Self {
        Self::Time(error)
    }
}

impl From<TimeRangeError> for ProjectError {
    fn from(error: TimeRangeError) -> Self {
        Self::Time(match error {
            TimeRangeError::Arithmetic(error) => error,
            TimeRangeError::EmptyOrReversed => RationalError::InvalidDecimal,
        })
    }
}

impl From<FrameRateError> for ProjectError {
    fn from(error: FrameRateError) -> Self {
        Self::FrameRate(error)
    }
}

impl From<SceneError> for ProjectError {
    fn from(error: SceneError) -> Self {
        Self::Scene(error)
    }
}

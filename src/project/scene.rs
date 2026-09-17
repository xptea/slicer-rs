//! Immutable scene evaluation shared by preview and export.

use super::Project;
use super::assets::{AssetId, AssetKind, ClipId, Orientation, SourceFrameSelection, TrackId};
use super::clips::{AudioSettings, Canvas, Clip, ClipKind, Color, ShapeClip, TextClip, Transform};
use super::time::{FrameRate, RationalError, Time, TimeRange};
use std::cmp::Ordering;
use std::error::Error;
use std::fmt;

/// A total stacking key.  Every visual overlap is ordered by track order,
/// clip order, and stable IDs; input vector order is never an implicit tie.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StackOrder {
    pub track_order: i64,
    pub clip_order: i64,
    pub track_id: TrackId,
    pub clip_id: ClipId,
}

impl Ord for StackOrder {
    fn cmp(&self, other: &Self) -> Ordering {
        self.track_order
            .cmp(&other.track_order)
            .then(self.clip_order.cmp(&other.clip_order))
            .then(self.track_id.cmp(&other.track_id))
            .then(self.clip_id.cmp(&other.clip_id))
    }
}

impl PartialOrd for StackOrder {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// The resolved visual payload for one active clip.
#[derive(Clone, Debug, PartialEq)]
pub enum SceneKind {
    Video {
        asset_id: AssetId,
        source_time: Time,
        frame: Option<SourceFrameSelection>,
        orientation: Orientation,
        pixel_aspect: super::Rational,
    },
    Image {
        asset_id: AssetId,
        orientation: Orientation,
        pixel_aspect: super::Rational,
    },
    Text(TextClip),
    Shape(ShapeClip),
}

/// One ordered draw operation.  It contains only owned data, so a renderer
/// can retain it after the mutable project has advanced to another revision.
#[derive(Clone, Debug, PartialEq)]
pub struct DrawItem {
    pub clip_id: ClipId,
    pub track_id: TrackId,
    pub order: StackOrder,
    pub range: TimeRange,
    pub transform: Transform,
    pub kind: SceneKind,
}

/// An active audio interval for a mixer or offline renderer.
#[derive(Clone, Debug, PartialEq)]
pub struct AudioInterval {
    pub clip_id: ClipId,
    pub track_id: TrackId,
    pub asset_id: AssetId,
    pub project_range: TimeRange,
    pub source_range: TimeRange,
    pub gain: f64,
    pub muted: bool,
    /// True when this interval came from a video's embedded audio stream.
    pub embedded: bool,
}

impl AudioInterval {
    pub fn source_time_at(&self, project_time: Time) -> Result<Option<Time>, RationalError> {
        if !self.project_range.contains(project_time) {
            return Ok(None);
        }
        let elapsed = project_time.checked_sub(self.project_range.start)?;
        Ok(Some(self.source_range.start.checked_add(elapsed)?))
    }

    pub fn settings(&self) -> AudioSettings {
        AudioSettings {
            gain: self.gain,
            muted: self.muted,
        }
    }
}

/// Immutable, revision-tagged output from the common scene evaluator.
#[derive(Clone, Debug, PartialEq)]
pub struct SceneSnapshot {
    pub project_id: super::ProjectId,
    pub revision: u64,
    pub time: Time,
    pub duration: Time,
    pub canvas: Canvas,
    pub frame_rate: FrameRate,
    pub background: Color,
    /// Back-to-front order.
    pub draw_items: Vec<DrawItem>,
    pub audio_items: Vec<AudioInterval>,
}

impl SceneSnapshot {
    pub fn visual_items(&self) -> &[DrawItem] {
        &self.draw_items
    }

    pub fn audio_intervals(&self) -> &[AudioInterval] {
        &self.audio_items
    }
}

pub(crate) fn evaluate(project: &Project, time: Time) -> Result<SceneSnapshot, SceneError> {
    project
        .validate()
        .map_err(|error| SceneError::InvalidProject(error.to_string()))?;
    let duration = project.effective_duration();
    if time < Time::ZERO || time > duration {
        return Err(SceneError::TimeOutsideProject);
    }

    let mut draw_items = Vec::new();
    let mut audio_items = Vec::new();
    for track in &project.tracks {
        if !track.visible {
            continue;
        }
        for clip in &track.clips {
            if !clip.range.contains(time) {
                continue;
            }
            let order = StackOrder {
                track_order: track.order,
                clip_order: clip.order,
                track_id: track.id,
                clip_id: clip.id,
            };
            if clip.kind.is_visual() && clip.visible {
                let kind = resolve_visual_kind(project, clip, time)?;
                draw_items.push(DrawItem {
                    clip_id: clip.id,
                    track_id: track.id,
                    order,
                    range: clip.range,
                    transform: clip.transform,
                    kind,
                });
            }
            if let Some(interval) = resolve_audio_interval(project, track.id, clip, clip.range)? {
                audio_items.push(interval);
            }
        }
    }
    draw_items.sort_by_key(|item| item.order);
    audio_items.sort_by_key(|item| {
        (
            item.project_range.start,
            item.track_id,
            item.clip_id,
            item.embedded,
        )
    });

    Ok(SceneSnapshot {
        project_id: project.id,
        revision: project.revision,
        time,
        duration,
        canvas: project.canvas,
        frame_rate: project.frame_rate,
        background: project.background,
        draw_items,
        audio_items,
    })
}

pub(crate) fn audio_intervals(
    project: &Project,
    range: TimeRange,
) -> Result<Vec<AudioInterval>, SceneError> {
    project
        .validate()
        .map_err(|error| SceneError::InvalidProject(error.to_string()))?;
    if range.start < Time::ZERO {
        return Err(SceneError::NegativeRange);
    }

    let mut intervals = Vec::new();
    for track in &project.tracks {
        if !track.visible {
            continue;
        }
        for clip in &track.clips {
            let Some(overlap) = clip.range.intersection(range) else {
                continue;
            };
            if let Some(interval) = resolve_audio_interval(project, track.id, clip, overlap)? {
                intervals.push(interval);
            }
        }
    }
    intervals.sort_by_key(|item| {
        (
            item.project_range.start,
            item.track_id,
            item.clip_id,
            item.embedded,
        )
    });
    Ok(intervals)
}

fn resolve_visual_kind(
    project: &Project,
    clip: &Clip,
    time: Time,
) -> Result<SceneKind, SceneError> {
    match &clip.kind {
        ClipKind::Video(video) => {
            let asset = project
                .asset(video.asset_id)
                .ok_or(SceneError::MissingAsset(video.asset_id))?;
            let metadata = asset
                .video_metadata()
                .ok_or(SceneError::AssetKindMismatch(video.asset_id))?;
            let source_time = clip
                .source_time_at(time)
                .map_err(SceneError::Time)?
                .ok_or(SceneError::MissingSourceTime(clip.id))?;
            let frame = metadata
                .frame_at(source_time)
                .map_err(|error| SceneError::Asset(error.to_string()))?;
            Ok(SceneKind::Video {
                asset_id: video.asset_id,
                source_time,
                frame,
                orientation: metadata.orientation,
                pixel_aspect: metadata.pixel_aspect,
            })
        }
        ClipKind::Image(image) => {
            let asset = project
                .asset(image.asset_id)
                .ok_or(SceneError::MissingAsset(image.asset_id))?;
            let metadata = asset
                .image_metadata()
                .ok_or(SceneError::AssetKindMismatch(image.asset_id))?;
            Ok(SceneKind::Image {
                asset_id: image.asset_id,
                orientation: metadata.orientation,
                pixel_aspect: metadata.pixel_aspect,
            })
        }
        ClipKind::Text(text) => Ok(SceneKind::Text(text.clone())),
        ClipKind::Shape(shape) => Ok(SceneKind::Shape(shape.clone())),
        ClipKind::Audio(_) => Err(SceneError::AudioIsNotVisual(clip.id)),
    }
}

fn resolve_audio_interval(
    project: &Project,
    track_id: TrackId,
    clip: &Clip,
    project_range: TimeRange,
) -> Result<Option<AudioInterval>, SceneError> {
    let (asset_id, source_range, settings, embedded) = match &clip.kind {
        ClipKind::Video(video) => {
            let Some(settings) = video.audio.settings() else {
                return Ok(None);
            };
            let asset = project
                .asset(video.asset_id)
                .ok_or(SceneError::MissingAsset(video.asset_id))?;
            let has_audio = asset
                .video_metadata()
                .is_some_and(|metadata| metadata.has_audio);
            if !has_audio {
                return Ok(None);
            }
            (video.asset_id, video.source_range, settings, true)
        }
        ClipKind::Audio(audio) => {
            let asset = project
                .asset(audio.asset_id)
                .ok_or(SceneError::MissingAsset(audio.asset_id))?;
            if asset.kind == AssetKind::Video
                && !asset
                    .video_metadata()
                    .is_some_and(|metadata| metadata.has_audio)
            {
                return Ok(None);
            }
            (audio.asset_id, audio.source_range, audio.settings, false)
        }
        ClipKind::Image(_) | ClipKind::Text(_) | ClipKind::Shape(_) => return Ok(None),
    };

    let elapsed = project_range
        .start
        .checked_sub(clip.range.start)
        .map_err(SceneError::Time)?;
    let source_start = source_range
        .start
        .checked_add(elapsed)
        .map_err(SceneError::Time)?;
    let source_end = source_start
        .checked_add(project_range.duration().map_err(SceneError::Time)?)
        .map_err(SceneError::Time)?;
    let source_range = TimeRange::new(source_start, source_end).map_err(SceneError::Range)?;
    Ok(Some(AudioInterval {
        clip_id: clip.id,
        track_id,
        asset_id,
        project_range,
        source_range,
        gain: settings.gain,
        muted: settings.muted,
        embedded,
    }))
}

/// Scene evaluation failures.
#[derive(Clone, Debug, PartialEq)]
pub enum SceneError {
    InvalidProject(String),
    TimeOutsideProject,
    NegativeRange,
    MissingAsset(AssetId),
    AssetKindMismatch(AssetId),
    MissingSourceTime(ClipId),
    AudioIsNotVisual(ClipId),
    Asset(String),
    Range(super::time::TimeRangeError),
    Time(RationalError),
}

impl fmt::Display for SceneError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidProject(error) => write!(formatter, "invalid project: {error}"),
            Self::TimeOutsideProject => {
                formatter.write_str("scene time is outside project duration")
            }
            Self::NegativeRange => formatter.write_str("audio range must be non-negative"),
            Self::MissingAsset(id) => write!(formatter, "scene asset {id} is missing"),
            Self::AssetKindMismatch(id) => write!(formatter, "scene asset {id} has the wrong kind"),
            Self::MissingSourceTime(id) => write!(formatter, "clip {id} has no source time"),
            Self::AudioIsNotVisual(id) => write!(formatter, "audio clip {id} cannot be drawn"),
            Self::Asset(error) => formatter.write_str(error),
            Self::Range(error) => error.fmt(formatter),
            Self::Time(error) => error.fmt(formatter),
        }
    }
}

impl Error for SceneError {}

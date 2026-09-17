//! Compatibility adapter between the original one-video trim state and a
//! layered [`Project`].

use super::assets::{Asset, AssetId, AssetKind, ClipId, TrackId, VideoMetadata};
use super::clips::{Canvas, Clip, ClipKind, CropRect, Track};
use super::time::{FrameRate, Time, TimeRange};
use super::{Project, ProjectError};
use std::path::PathBuf;

/// The old editor's observable edit state, represented with exact project
/// times.  UI adapters may convert their text fields to this value at the
/// boundary; the model never stores those text fields as authoritative data.
#[derive(Clone, Debug, PartialEq)]
pub struct LegacyVideoEdit {
    pub input: PathBuf,
    pub start: Time,
    pub end: Time,
    pub crop: Option<CropRect>,
}

impl LegacyVideoEdit {
    pub fn new(input: impl Into<PathBuf>, start: Time, end: Time) -> Result<Self, ProjectError> {
        if start < Time::ZERO || end <= start {
            return Err(ProjectError::InvalidDuration);
        }
        Ok(Self {
            input: input.into(),
            start,
            end,
            crop: None,
        })
    }

    pub fn range(&self) -> Result<TimeRange, ProjectError> {
        TimeRange::new(self.start, self.end).map_err(ProjectError::from)
    }
}

/// Conversion helpers for opening a legacy source as a layered project and
/// recognizing projects that can still use the native player fast path.
pub struct SingleVideoAdapter;

impl SingleVideoAdapter {
    /// Create a layered project containing one video asset, one mixed track,
    /// and one clip whose source and timeline ranges are identical.
    pub fn from_media(
        input: impl Into<PathBuf>,
        duration: Time,
        width: u32,
        height: u32,
    ) -> Result<Project, ProjectError> {
        if duration <= Time::ZERO {
            return Err(ProjectError::InvalidDuration);
        }
        let mut project = Project::new(Canvas::new(width, height)?, FrameRate::FPS_30);
        let asset_id = AssetId::fresh();
        let track_id = TrackId::fresh();
        let clip_id = ClipId::fresh();
        project.add_asset(Asset::video(asset_id, input, duration, width, height)?)?;
        project.add_track(Track::new(track_id, "Video", 0))?;
        let range = TimeRange::new(Time::ZERO, duration)?;
        project.add_clip(track_id, Clip::video(clip_id, range, asset_id, range))?;
        Ok(project)
    }

    /// Alias used by callers that treat the adapter as an importer.
    pub fn import(
        input: impl Into<PathBuf>,
        duration: Time,
        width: u32,
        height: u32,
    ) -> Result<Project, ProjectError> {
        Self::from_media(input, duration, width, height)
    }

    /// Recognize a project that contains exactly one video clip and no other
    /// media/content.  The returned state is suitable for the legacy trim UI.
    pub fn to_legacy(project: &Project) -> Result<LegacyVideoEdit, ProjectError> {
        project.validate()?;
        let mut matching = project
            .tracks
            .iter()
            .flat_map(|track| track.clips.iter().map(move |clip| (track, clip)))
            .filter(|(_, clip)| matches!(clip.kind, ClipKind::Video(_)));
        let Some((_, clip)) = matching.next() else {
            return Err(ProjectError::UnsupportedSchema(project.schema_version));
        };
        if matching.next().is_some()
            || project
                .tracks
                .iter()
                .flat_map(|track| track.clips.iter())
                .any(|clip| !matches!(clip.kind, ClipKind::Video(_)))
            || project
                .assets
                .values()
                .any(|asset| asset.kind != AssetKind::Video)
        {
            return Err(ProjectError::UnsupportedSchema(project.schema_version));
        }
        let ClipKind::Video(video) = &clip.kind else {
            unreachable!("matching iterator only yields video clips")
        };
        Ok(LegacyVideoEdit {
            input: project
                .asset(video.asset_id)
                .ok_or(ProjectError::AssetNotFound(video.asset_id))?
                .path
                .clone(),
            start: video.source_range.start,
            end: video.source_range.end,
            crop: clip.transform.crop,
        })
    }

    /// Alias for callers that use the direction of conversion in the name.
    pub fn export(project: &Project) -> Result<LegacyVideoEdit, ProjectError> {
        Self::to_legacy(project)
    }

    /// Update the single compatible clip from legacy trim/crop values.
    pub fn apply_legacy(project: &mut Project, edit: &LegacyVideoEdit) -> Result<(), ProjectError> {
        let range = edit.range()?;
        let mut candidate = project.clone();
        let (clip_id, asset_id) = candidate
            .tracks
            .iter()
            .flat_map(|track| track.clips.iter())
            .find_map(|clip| match &clip.kind {
                ClipKind::Video(video) => Some((clip.id, video.asset_id)),
                _ => None,
            })
            .ok_or(ProjectError::ClipNotFound(ClipId::new(0)))?;
        let asset_duration = candidate
            .asset(asset_id)
            .and_then(|asset| asset.duration())
            .ok_or(ProjectError::AssetNotFound(asset_id))?;
        if edit.end > asset_duration {
            return Err(ProjectError::SourceOutOfBounds(clip_id));
        }
        let clip = candidate
            .clip_mut(clip_id)
            .ok_or(ProjectError::ClipNotFound(clip_id))?;
        clip.range = range;
        let ClipKind::Video(video) = &mut clip.kind else {
            unreachable!("selected clip is a video")
        };
        video.source_range = range;
        clip.transform.crop = edit.crop;
        candidate.validate()?;
        *project = candidate;
        Ok(())
    }

    /// Return the source metadata when a project has the legacy-compatible
    /// single video asset.
    pub fn video_metadata(project: &Project) -> Option<&VideoMetadata> {
        project
            .assets
            .values()
            .find_map(|asset| asset.video_metadata())
    }
}

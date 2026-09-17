//! GPUI-independent timeline geometry and editing commands.
//!
//! The UI can use this module as a thin adapter around the model without
//! copying clip state into widget state.  Coordinates are floating point at
//! the screen boundary, but time is kept as the project's exact [`Time`]
//! rational everywhere else.  Clip and row intervals are half-open: a point
//! at an end boundary belongs to the following interval, never to the clip
//! that ended there.

use slicer::project::{
    Clip, ClipId, CommandError, CommandReceipt, EditCommand, Project, ProjectError, Rational,
    RationalError, Time, TimeRange, TimeRangeError, Track, TrackId,
};
use std::collections::BTreeSet;
use std::error::Error;
use std::fmt;

/// Reasonable defaults for a timeline adapter.  A GPUI view can choose its
/// own values; these constants only make small headless clients convenient.
pub const DEFAULT_ROW_HEIGHT: f64 = 56.0;
pub const DEFAULT_ROW_GAP: f64 = 4.0;
pub const DEFAULT_SNAP_THRESHOLD_PIXELS: f64 = 8.0;

/// Errors raised while translating screen interaction into model operations.
#[derive(Clone, Debug, PartialEq)]
pub enum TimelineError {
    InvalidGeometry(&'static str),
    NonFiniteCoordinate,
    InvalidArgument(&'static str),
    EmptySelection,
    DuplicateSelection(ClipId),
    TrackLocked(TrackId),
    SourceOutOfBounds(ClipId),
    SourceDurationMismatch(ClipId),
    Rational(RationalError),
    TimeRange(TimeRangeError),
    Project(ProjectError),
    Command(CommandError),
}

impl fmt::Display for TimelineError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidGeometry(message) => formatter.write_str(message),
            Self::NonFiniteCoordinate => formatter.write_str("timeline coordinate must be finite"),
            Self::InvalidArgument(message) => formatter.write_str(message),
            Self::EmptySelection => formatter.write_str("timeline selection must not be empty"),
            Self::DuplicateSelection(id) => {
                write!(formatter, "clip {id} is selected more than once")
            }
            Self::TrackLocked(id) => write!(formatter, "track {id} is locked"),
            Self::SourceOutOfBounds(id) => write!(formatter, "clip {id} trim exceeds its source"),
            Self::SourceDurationMismatch(id) => {
                write!(formatter, "clip {id} timeline and source durations differ")
            }
            Self::Rational(error) => error.fmt(formatter),
            Self::TimeRange(error) => error.fmt(formatter),
            Self::Project(error) => error.fmt(formatter),
            Self::Command(error) => error.fmt(formatter),
        }
    }
}

impl Error for TimelineError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Rational(error) => Some(error),
            Self::TimeRange(error) => Some(error),
            Self::Project(error) => Some(error),
            Self::Command(error) => Some(error),
            _ => None,
        }
    }
}

impl From<RationalError> for TimelineError {
    fn from(error: RationalError) -> Self {
        Self::Rational(error)
    }
}

impl From<TimeRangeError> for TimelineError {
    fn from(error: TimeRangeError) -> Self {
        Self::TimeRange(error)
    }
}

impl From<ProjectError> for TimelineError {
    fn from(error: ProjectError) -> Self {
        Self::Project(error)
    }
}

impl From<CommandError> for TimelineError {
    fn from(error: CommandError) -> Self {
        Self::Command(error)
    }
}

/// Start/end is shared by trimming and snap-boundary descriptions.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ClipEdge {
    Start,
    End,
}

/// More descriptive spelling for callers that are building trim commands.
pub type TrimEdge = ClipEdge;

/// Exact horizontal mapping between project time and screen pixels.
///
/// `scroll_time` is the time at `origin_x`.  Thus scrolling horizontally is
/// represented without accumulating pixel rounding error, and the scale
/// itself is also rational.  The final screen coordinate remains `f64`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TimelineScale {
    pub origin_x: f64,
    pub pixels_per_second: Rational,
    pub scroll_time: Time,
}

impl TimelineScale {
    pub fn new(
        origin_x: f64,
        pixels_per_second: Rational,
        scroll_time: Time,
    ) -> Result<Self, TimelineError> {
        if !origin_x.is_finite() {
            return Err(TimelineError::NonFiniteCoordinate);
        }
        if pixels_per_second <= Time::ZERO {
            return Err(TimelineError::InvalidGeometry(
                "timeline scale must have positive pixels per second",
            ));
        }
        Ok(Self {
            origin_x,
            pixels_per_second,
            scroll_time,
        })
    }

    pub fn from_pixels_per_second(
        origin_x: f64,
        pixels_per_second: f64,
        scroll_time: Time,
    ) -> Result<Self, TimelineError> {
        let pixels_per_second = Rational::from_seconds(pixels_per_second)?;
        Self::new(origin_x, pixels_per_second, scroll_time)
    }

    pub fn pixels_per_second_f64(self) -> f64 {
        self.pixels_per_second.to_f64()
    }

    /// Convert a rational project time to a screen coordinate.
    pub fn time_to_pixel(self, time: Time) -> Result<f64, TimelineError> {
        let elapsed = time.checked_sub(self.scroll_time)?;
        let pixels = elapsed.checked_mul(self.pixels_per_second)?;
        let pixel = self.origin_x + pixels.to_f64();
        if !pixel.is_finite() {
            return Err(TimelineError::NonFiniteCoordinate);
        }
        Ok(pixel)
    }

    /// Convert a screen coordinate back to the exact rational project time.
    pub fn pixel_to_time(self, pixel: f64) -> Result<Time, TimelineError> {
        if !pixel.is_finite() {
            return Err(TimelineError::NonFiniteCoordinate);
        }
        let pixel_delta = pixel - self.origin_x;
        if !pixel_delta.is_finite() {
            return Err(TimelineError::NonFiniteCoordinate);
        }
        let pixel_delta = Rational::from_seconds(pixel_delta)?;
        let elapsed = pixel_delta.checked_div(self.pixels_per_second)?;
        Ok(self.scroll_time.checked_add(elapsed)?)
    }

    pub fn time_to_x(self, time: Time) -> Result<f64, TimelineError> {
        self.time_to_pixel(time)
    }

    pub fn x_to_time(self, pixel: f64) -> Result<Time, TimelineError> {
        self.pixel_to_time(pixel)
    }

    pub fn with_scroll_time(self, scroll_time: Time) -> Self {
        Self {
            scroll_time,
            ..self
        }
    }

    pub fn scroll_by_time(self, delta: Time) -> Result<Self, TimelineError> {
        Ok(self.with_scroll_time(self.scroll_time.checked_add(delta)?))
    }

    /// Scroll by screen pixels.  Positive values move the viewport later in
    /// the project, so the content appears to move left.
    pub fn scroll_by_pixels(self, pixels: f64) -> Result<Self, TimelineError> {
        if !pixels.is_finite() {
            return Err(TimelineError::NonFiniteCoordinate);
        }
        let pixel_delta = Rational::from_seconds(pixels)?;
        let time_delta = pixel_delta.checked_div(self.pixels_per_second)?;
        self.scroll_by_time(time_delta)
    }

    /// Return a scale with a different zoom while keeping `anchor_x` mapped
    /// to the same exact project time.
    pub fn zoom_about(self, factor: f64, anchor_x: f64) -> Result<Self, TimelineError> {
        if !factor.is_finite() || factor <= 0.0 {
            return Err(TimelineError::InvalidGeometry(
                "timeline zoom factor must be finite and positive",
            ));
        }
        if !anchor_x.is_finite() {
            return Err(TimelineError::NonFiniteCoordinate);
        }
        let factor = Rational::from_seconds(factor)?;
        let next_pixels_per_second = self.pixels_per_second.checked_mul(factor)?;
        if next_pixels_per_second <= Time::ZERO {
            return Err(TimelineError::InvalidGeometry(
                "timeline zoom factor must produce a positive scale",
            ));
        }
        let anchor_time = self.pixel_to_time(anchor_x)?;
        let anchor_delta = anchor_x - self.origin_x;
        if !anchor_delta.is_finite() {
            return Err(TimelineError::NonFiniteCoordinate);
        }
        let anchor_delta = Rational::from_seconds(anchor_delta)?;
        let next_scroll =
            anchor_time.checked_sub(anchor_delta.checked_div(next_pixels_per_second)?)?;
        Self::new(self.origin_x, next_pixels_per_second, next_scroll)
    }

    pub fn zoomed_about(self, factor: f64, anchor_x: f64) -> Result<Self, TimelineError> {
        self.zoom_about(factor, anchor_x)
    }

    /// The exact time range covered by `[origin_x, origin_x + width)`.
    pub fn visible_time_range(self, width: f64) -> Result<TimeRange, TimelineError> {
        if !width.is_finite() || width <= 0.0 {
            return Err(TimelineError::InvalidGeometry(
                "timeline viewport width must be finite and positive",
            ));
        }
        let end_x = self.origin_x + width;
        if !end_x.is_finite() {
            return Err(TimelineError::NonFiniteCoordinate);
        }
        TimeRange::new(self.scroll_time, self.pixel_to_time(end_x)?).map_err(Into::into)
    }
}

/// A viewport combines horizontal time mapping with the vertical scroll area.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TimelineViewport {
    pub scale: TimelineScale,
    pub width: f64,
    pub height: f64,
    pub origin_y: f64,
    pub scroll_y: f64,
}

impl TimelineViewport {
    pub fn new(
        origin_x: f64,
        origin_y: f64,
        width: f64,
        height: f64,
        pixels_per_second: Rational,
        scroll_time: Time,
        scroll_y: f64,
    ) -> Result<Self, TimelineError> {
        let scale = TimelineScale::new(origin_x, pixels_per_second, scroll_time)?;
        Self::from_scale(scale, origin_y, width, height, scroll_y)
    }

    pub fn from_scale(
        scale: TimelineScale,
        origin_y: f64,
        width: f64,
        height: f64,
        scroll_y: f64,
    ) -> Result<Self, TimelineError> {
        if !origin_y.is_finite() || !scroll_y.is_finite() {
            return Err(TimelineError::NonFiniteCoordinate);
        }
        if !width.is_finite() || width <= 0.0 {
            return Err(TimelineError::InvalidGeometry(
                "timeline viewport width must be finite and positive",
            ));
        }
        if !height.is_finite() || height <= 0.0 {
            return Err(TimelineError::InvalidGeometry(
                "timeline viewport height must be finite and positive",
            ));
        }
        Ok(Self {
            scale,
            width,
            height,
            origin_y,
            scroll_y: scroll_y.max(0.0),
        })
    }

    pub fn time_to_pixel(self, time: Time) -> Result<f64, TimelineError> {
        self.scale.time_to_pixel(time)
    }

    pub fn pixel_to_time(self, pixel: f64) -> Result<Time, TimelineError> {
        self.scale.pixel_to_time(pixel)
    }

    pub fn time_to_x(self, time: Time) -> Result<f64, TimelineError> {
        self.scale.time_to_pixel(time)
    }

    pub fn x_to_time(self, pixel: f64) -> Result<Time, TimelineError> {
        self.scale.pixel_to_time(pixel)
    }

    pub fn visible_time_range(self) -> Result<TimeRange, TimelineError> {
        self.scale.visible_time_range(self.width)
    }

    pub fn contains_x(self, x: f64) -> bool {
        x.is_finite() && x >= self.scale.origin_x && x < self.scale.origin_x + self.width
    }

    pub fn contains_y(self, y: f64) -> bool {
        y.is_finite() && y >= self.origin_y && y < self.origin_y + self.height
    }

    pub fn zoom_about(&mut self, factor: f64, anchor_x: f64) -> Result<(), TimelineError> {
        self.scale = self.scale.zoom_about(factor, anchor_x)?;
        Ok(())
    }

    pub fn zoomed_about(self, factor: f64, anchor_x: f64) -> Result<Self, TimelineError> {
        let scale = self.scale.zoom_about(factor, anchor_x)?;
        Self::from_scale(scale, self.origin_y, self.width, self.height, self.scroll_y)
    }

    pub fn scroll_horizontal_by_pixels(&mut self, pixels: f64) -> Result<(), TimelineError> {
        self.scale = self.scale.scroll_by_pixels(pixels)?;
        Ok(())
    }

    pub fn scroll_horizontal_to(&mut self, time: Time) {
        self.scale = self.scale.with_scroll_time(time);
    }

    pub fn scroll_vertical_by(&mut self, pixels: f64) -> Result<(), TimelineError> {
        if !pixels.is_finite() {
            return Err(TimelineError::NonFiniteCoordinate);
        }
        self.scroll_y = (self.scroll_y + pixels).max(0.0);
        Ok(())
    }

    pub fn scroll_vertical_to(&mut self, pixels: f64) -> Result<(), TimelineError> {
        if !pixels.is_finite() {
            return Err(TimelineError::NonFiniteCoordinate);
        }
        self.scroll_y = pixels.max(0.0);
        Ok(())
    }

    pub fn row_layout(
        self,
        row_height: f64,
        row_gap: f64,
    ) -> Result<TrackRowLayout, TimelineError> {
        TrackRowLayout::new(self.origin_y, self.scroll_y, row_height, row_gap)
    }

    pub fn rows(
        self,
        project: &Project,
        row_height: f64,
        row_gap: f64,
    ) -> Result<Vec<TrackRow>, TimelineError> {
        Ok(self
            .row_layout(row_height, row_gap)?
            .visible_rows(project, self.height))
    }

    pub fn visible_clips(
        self,
        project: &Project,
        rows: &TrackRowLayout,
    ) -> Result<Vec<VisibleClip>, TimelineError> {
        visible_clips(project, self, rows)
    }

    pub fn hit_test(
        self,
        project: &Project,
        rows: &TrackRowLayout,
        x: f64,
        y: f64,
    ) -> Result<Option<ClipHit>, TimelineError> {
        hit_test_visible_clip(project, self, rows, x, y)
    }
}

/// A model track's row in viewport coordinates.  Row bottoms are exclusive;
/// the gap after a row is intentionally not part of any row.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TrackRow {
    pub track_id: TrackId,
    pub index: usize,
    pub top: f64,
    pub bottom: f64,
    pub order: i64,
    pub visible: bool,
    pub locked: bool,
}

/// Vertical row geometry independent of any GPUI layout primitives.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TrackRowLayout {
    pub origin_y: f64,
    pub scroll_y: f64,
    pub row_height: f64,
    pub row_gap: f64,
}

impl TrackRowLayout {
    pub fn new(
        origin_y: f64,
        scroll_y: f64,
        row_height: f64,
        row_gap: f64,
    ) -> Result<Self, TimelineError> {
        if !origin_y.is_finite() || !scroll_y.is_finite() {
            return Err(TimelineError::NonFiniteCoordinate);
        }
        if !row_height.is_finite() || row_height <= 0.0 {
            return Err(TimelineError::InvalidGeometry(
                "track row height must be finite and positive",
            ));
        }
        if !row_gap.is_finite() || row_gap < 0.0 {
            return Err(TimelineError::InvalidGeometry(
                "track row gap must be finite and non-negative",
            ));
        }
        Ok(Self {
            origin_y,
            scroll_y: scroll_y.max(0.0),
            row_height,
            row_gap,
        })
    }

    pub fn with_defaults(origin_y: f64, scroll_y: f64) -> Result<Self, TimelineError> {
        Self::new(origin_y, scroll_y, DEFAULT_ROW_HEIGHT, DEFAULT_ROW_GAP)
    }

    pub fn row_step(self) -> f64 {
        self.row_height + self.row_gap
    }

    pub fn row_top(self, index: usize) -> f64 {
        self.origin_y + index as f64 * self.row_step() - self.scroll_y
    }

    pub fn total_height(self, project: &Project) -> f64 {
        if project.tracks.is_empty() {
            0.0
        } else {
            project.tracks.len() as f64 * self.row_height
                + project.tracks.len().saturating_sub(1) as f64 * self.row_gap
        }
    }

    /// Return rows in visual stack order: the highest layer is at the top.
    /// Track ID is the deterministic tie-breaker for equal explicit orders.
    pub fn rows(self, project: &Project) -> Vec<TrackRow> {
        let mut tracks: Vec<&Track> = project.tracks.iter().collect();
        tracks.sort_by(|left, right| {
            right
                .order
                .cmp(&left.order)
                .then_with(|| left.id.cmp(&right.id))
        });
        tracks
            .into_iter()
            .enumerate()
            .map(|(index, track)| {
                let top = self.row_top(index);
                TrackRow {
                    track_id: track.id,
                    index,
                    top,
                    bottom: top + self.row_height,
                    order: track.order,
                    visible: track.visible,
                    locked: track.locked,
                }
            })
            .collect()
    }

    pub fn visible_rows(self, project: &Project, viewport_height: f64) -> Vec<TrackRow> {
        if !viewport_height.is_finite() || viewport_height <= 0.0 {
            return Vec::new();
        }
        let viewport_bottom = self.origin_y + viewport_height;
        self.rows(project)
            .into_iter()
            .filter(|row| row.bottom > self.origin_y && row.top < viewport_bottom)
            .collect()
    }

    pub fn row_for_track(self, project: &Project, track_id: TrackId) -> Option<TrackRow> {
        self.rows(project)
            .into_iter()
            .find(|row| row.track_id == track_id)
    }

    pub fn row_at(self, project: &Project, y: f64) -> Option<TrackRow> {
        if !y.is_finite() {
            return None;
        }
        self.rows(project)
            .into_iter()
            .find(|row| y >= row.top && y < row.bottom)
    }
}

/// A visible clip and its screen span.  The span may extend beyond the
/// viewport when the clip is only partially visible.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct VisibleClip {
    pub track_id: TrackId,
    pub clip_id: ClipId,
    pub row_index: usize,
    pub range: TimeRange,
    pub left: f64,
    pub right: f64,
    pub track_order: i64,
    pub clip_order: i64,
    pub locked: bool,
}

/// Result of a point hit test.  `time` is the exact time represented by the
/// pointer; the matching clip interval still uses `[start, end)` semantics.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ClipHit {
    pub track_id: TrackId,
    pub clip_id: ClipId,
    pub row_index: usize,
    pub range: TimeRange,
    pub time: Time,
    pub track_order: i64,
    pub clip_order: i64,
    pub locked: bool,
}

/// Enumerate visible clips intersecting the current time and row viewport.
pub fn visible_clips(
    project: &Project,
    viewport: TimelineViewport,
    rows: &TrackRowLayout,
) -> Result<Vec<VisibleClip>, TimelineError> {
    let visible_time = viewport.visible_time_range()?;
    let mut clips = Vec::new();
    for row in rows.visible_rows(project, viewport.height) {
        let Some(track) = project.track(row.track_id) else {
            continue;
        };
        if !track.visible {
            continue;
        }
        for clip in &track.clips {
            if !clip.visible || !clip.range.intersects(visible_time) {
                continue;
            }
            clips.push(VisibleClip {
                track_id: track.id,
                clip_id: clip.id,
                row_index: row.index,
                range: clip.range,
                left: viewport.time_to_pixel(clip.range.start)?,
                right: viewport.time_to_pixel(clip.range.end)?,
                track_order: track.order,
                clip_order: clip.order,
                locked: track.locked,
            });
        }
    }
    clips.sort_by(|left, right| {
        left.row_index
            .cmp(&right.row_index)
            .then(left.clip_order.cmp(&right.clip_order))
            .then(left.clip_id.cmp(&right.clip_id))
    });
    Ok(clips)
}

/// Hit-test one visible clip.  Hidden tracks/clips and points in the row gap
/// are not selectable.  If clips overlap in one row, the highest clip order
/// (then highest clip ID) is the visible topmost hit.
pub fn hit_test_visible_clip(
    project: &Project,
    viewport: TimelineViewport,
    rows: &TrackRowLayout,
    x: f64,
    y: f64,
) -> Result<Option<ClipHit>, TimelineError> {
    if !viewport.contains_x(x) || !viewport.contains_y(y) {
        return Ok(None);
    }
    let Some(row) = rows.row_at(project, y) else {
        return Ok(None);
    };
    let Some(track) = project.track(row.track_id) else {
        return Ok(None);
    };
    if !track.visible {
        return Ok(None);
    }
    let time = viewport.pixel_to_time(x)?;
    let clip = track
        .clips
        .iter()
        .filter(|clip| clip.visible && clip.range.contains(time))
        .max_by(|left, right| {
            left.order
                .cmp(&right.order)
                .then_with(|| left.id.cmp(&right.id))
        });
    Ok(clip.map(|clip| ClipHit {
        track_id: track.id,
        clip_id: clip.id,
        row_index: row.index,
        range: clip.range,
        time,
        track_order: track.order,
        clip_order: clip.order,
        locked: track.locked,
    }))
}

/// Alias with a shorter name for event handlers.
pub fn hit_test(
    project: &Project,
    viewport: TimelineViewport,
    rows: &TrackRowLayout,
    x: f64,
    y: f64,
) -> Result<Option<ClipHit>, TimelineError> {
    hit_test_visible_clip(project, viewport, rows, x, y)
}

/// What a snap operation selected, if anything.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SnapTarget {
    ClipBoundary {
        track_id: TrackId,
        clip_id: ClipId,
        edge: ClipEdge,
    },
    Frame {
        frame_index: u64,
    },
}

/// Snap policy.  The threshold is measured in screen pixels at the current
/// zoom so the interaction feels consistent while zooming.
#[derive(Clone, Debug, PartialEq)]
pub struct SnapOptions {
    pub enabled: bool,
    pub clip_boundaries: bool,
    pub frame_grid: bool,
    pub threshold_pixels: f64,
    pub ignored_clip_ids: Vec<ClipId>,
}

impl Default for SnapOptions {
    fn default() -> Self {
        Self {
            enabled: true,
            clip_boundaries: true,
            frame_grid: true,
            threshold_pixels: DEFAULT_SNAP_THRESHOLD_PIXELS,
            ignored_clip_ids: Vec::new(),
        }
    }
}

impl SnapOptions {
    pub fn disabled() -> Self {
        Self {
            enabled: false,
            ..Self::default()
        }
    }

    pub fn with_threshold_pixels(mut self, threshold_pixels: f64) -> Self {
        self.threshold_pixels = threshold_pixels;
        self
    }

    pub fn ignoring_clip(mut self, clip_id: ClipId) -> Self {
        if !self.ignored_clip_ids.contains(&clip_id) {
            self.ignored_clip_ids.push(clip_id);
        }
        self
    }

    pub fn ignoring_clips<I>(mut self, clip_ids: I) -> Self
    where
        I: IntoIterator<Item = ClipId>,
    {
        for clip_id in clip_ids {
            if !self.ignored_clip_ids.contains(&clip_id) {
                self.ignored_clip_ids.push(clip_id);
            }
        }
        self
    }

    fn ignores(&self, clip_id: ClipId) -> bool {
        self.ignored_clip_ids.contains(&clip_id)
    }
}

/// Result of snapping a candidate time.  An absent target means the original
/// candidate was outside the pixel threshold.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SnapResult {
    pub time: Time,
    pub target: Option<SnapTarget>,
    pub distance_pixels: f64,
}

impl SnapResult {
    pub fn is_snapped(self) -> bool {
        self.target.is_some()
    }
}

#[derive(Clone, Copy)]
struct SnapCandidate {
    time: Time,
    target: SnapTarget,
    distance_pixels: f64,
}

/// Snap to the nearest visible-model-independent clip boundary and/or output
/// frame boundary.  All clip boundaries are considered, including boundaries
/// outside the current viewport; the pixel threshold decides whether one is
/// actionable.
pub fn snap_time(
    project: &Project,
    candidate: Time,
    scale: TimelineScale,
    options: &SnapOptions,
) -> Result<SnapResult, TimelineError> {
    if !options.enabled {
        return Ok(SnapResult {
            time: candidate,
            target: None,
            distance_pixels: 0.0,
        });
    }
    if !options.threshold_pixels.is_finite() || options.threshold_pixels < 0.0 {
        return Err(TimelineError::InvalidArgument(
            "snap threshold must be finite and non-negative",
        ));
    }
    let candidate_pixel = scale.time_to_pixel(candidate)?;
    let mut best = None;

    if options.clip_boundaries {
        for track in &project.tracks {
            for clip in &track.clips {
                if options.ignores(clip.id) {
                    continue;
                }
                consider_snap_candidate(
                    &mut best,
                    candidate_pixel,
                    clip.range.start,
                    SnapTarget::ClipBoundary {
                        track_id: track.id,
                        clip_id: clip.id,
                        edge: ClipEdge::Start,
                    },
                    scale,
                    options.threshold_pixels,
                )?;
                consider_snap_candidate(
                    &mut best,
                    candidate_pixel,
                    clip.range.end,
                    SnapTarget::ClipBoundary {
                        track_id: track.id,
                        clip_id: clip.id,
                        edge: ClipEdge::End,
                    },
                    scale,
                    options.threshold_pixels,
                )?;
            }
        }
    }

    if options.frame_grid {
        let frame_rate = project.frame_rate;
        let frame_indices = if candidate < Time::ZERO {
            vec![0]
        } else {
            let floor = frame_rate.frame_index_at(candidate)?;
            let mut indices = vec![floor];
            if let Some(next) = floor.checked_add(1) {
                indices.push(next);
            }
            indices
        };
        for frame_index in frame_indices {
            let frame_time = frame_rate.frame_start(frame_index)?;
            consider_snap_candidate(
                &mut best,
                candidate_pixel,
                frame_time,
                SnapTarget::Frame { frame_index },
                scale,
                options.threshold_pixels,
            )?;
        }
    }

    Ok(match best {
        Some(best) => SnapResult {
            time: best.time,
            target: Some(best.target),
            distance_pixels: best.distance_pixels,
        },
        None => SnapResult {
            time: candidate,
            target: None,
            distance_pixels: 0.0,
        },
    })
}

fn consider_snap_candidate(
    best: &mut Option<SnapCandidate>,
    candidate_pixel: f64,
    snap_time: Time,
    target: SnapTarget,
    scale: TimelineScale,
    threshold_pixels: f64,
) -> Result<(), TimelineError> {
    let snap_pixel = scale.time_to_pixel(snap_time)?;
    let distance_pixels = (candidate_pixel - snap_pixel).abs();
    if !distance_pixels.is_finite() || distance_pixels > threshold_pixels {
        return Ok(());
    }
    let next = SnapCandidate {
        time: snap_time,
        target,
        distance_pixels,
    };
    let should_replace = best.is_none_or(|current| {
        let distance_order = distance_pixels.total_cmp(&current.distance_pixels);
        distance_order.is_lt()
            || (distance_order.is_eq()
                && snap_target_key(next.target, next.time)
                    < snap_target_key(current.target, current.time))
    });
    if should_replace {
        *best = Some(next);
    }
    Ok(())
}

fn snap_target_key(target: SnapTarget, time: Time) -> (u8, u64, u64, u8, u64, Time) {
    match target {
        SnapTarget::ClipBoundary {
            track_id,
            clip_id,
            edge,
        } => (
            0,
            track_id.value(),
            clip_id.value(),
            match edge {
                ClipEdge::Start => 0,
                ClipEdge::End => 1,
            },
            0,
            time,
        ),
        SnapTarget::Frame { frame_index } => (1, 0, 0, 0, frame_index, time),
    }
}

/// Build a model command that moves one clip without changing its duration or
/// source range.  `EditCommand::MoveClip` derives the new end from the old
/// duration, which is the invariant that preserves source trim.
pub fn build_move_command(
    project: &Project,
    clip_id: ClipId,
    new_start: Time,
) -> Result<EditCommand, TimelineError> {
    let (clip, _) = editable_clip(project, clip_id)?;
    if new_start < Time::ZERO {
        return Err(TimelineError::InvalidArgument(
            "clip start must be non-negative",
        ));
    }
    let duration = clip.duration()?;
    let _ = new_start.checked_add(duration)?;
    Ok(EditCommand::MoveClip { clip_id, new_start })
}

/// Build a trim command by moving one timeline edge.  For source-backed
/// clips, the corresponding source edge moves by the same exact delta, and
/// the source range is checked against the asset duration.
pub fn build_trim_command(
    project: &Project,
    clip_id: ClipId,
    edge: TrimEdge,
    new_time: Time,
) -> Result<EditCommand, TimelineError> {
    let (clip, _) = editable_clip(project, clip_id)?;
    let old_range = clip.range;
    let (range, source_range) = match edge {
        ClipEdge::Start => {
            if new_time < Time::ZERO {
                return Err(TimelineError::InvalidArgument(
                    "clip start must be non-negative",
                ));
            }
            let range = TimeRange::new(new_time, old_range.end)?;
            let source_range = trim_source_range(project, clip, clip_id, edge, new_time, range)?;
            (range, source_range)
        }
        ClipEdge::End => {
            let range = TimeRange::new(old_range.start, new_time)?;
            let source_range = trim_source_range(project, clip, clip_id, edge, new_time, range)?;
            (range, source_range)
        }
    };
    Ok(EditCommand::TrimClip {
        clip_id,
        range,
        source_range,
    })
}

fn trim_source_range(
    project: &Project,
    clip: &Clip,
    clip_id: ClipId,
    edge: ClipEdge,
    new_time: Time,
    new_range: TimeRange,
) -> Result<Option<TimeRange>, TimelineError> {
    let Some(current_source) = clip.source_range_ref() else {
        return Ok(None);
    };
    let current_duration = clip.duration()?;
    if current_source.duration()? != current_duration {
        return Err(TimelineError::SourceDurationMismatch(clip_id));
    }
    let source_duration = source_duration(project, clip, clip_id)?;
    let source_range = match edge {
        ClipEdge::Start => {
            let delta = new_time.checked_sub(clip.range.start)?;
            let start = current_source.start.checked_add(delta)?;
            TimeRange::new(start, current_source.end)?
        }
        ClipEdge::End => {
            let duration = new_range.duration()?;
            let end = current_source.start.checked_add(duration)?;
            TimeRange::new(current_source.start, end)?
        }
    };
    validate_source_bounds(source_range, source_duration, clip_id)?;
    Ok(Some(source_range))
}

fn source_duration(project: &Project, clip: &Clip, clip_id: ClipId) -> Result<Time, TimelineError> {
    let asset_id = clip.kind.asset_id().ok_or(TimelineError::InvalidArgument(
        "only source-backed clips have a source duration",
    ))?;
    let asset =
        project
            .asset(asset_id)
            .ok_or(TimelineError::Project(ProjectError::AssetNotFound(
                asset_id,
            )))?;
    asset
        .duration()
        .ok_or(TimelineError::SourceOutOfBounds(clip_id))
}

fn validate_source_bounds(
    source_range: TimeRange,
    source_duration: Time,
    clip_id: ClipId,
) -> Result<(), TimelineError> {
    if source_range.start < Time::ZERO || source_range.end > source_duration {
        return Err(TimelineError::SourceOutOfBounds(clip_id));
    }
    Ok(())
}

/// Build a split command.  The model command performs the paired source split
/// and preserves the half-open left/right intervals.
pub fn build_split_command(
    project: &Project,
    clip_id: ClipId,
    at: Time,
    new_clip_id: ClipId,
) -> Result<EditCommand, TimelineError> {
    let (clip, _) = editable_clip(project, clip_id)?;
    if new_clip_id.is_zero() || project.contains_clip(new_clip_id) {
        return Err(TimelineError::InvalidArgument(
            "split destination clip id must be new and non-zero",
        ));
    }
    if at <= clip.range.start || at >= clip.range.end {
        return Err(TimelineError::InvalidArgument(
            "split point must be strictly inside the clip interval",
        ));
    }
    Ok(EditCommand::SplitClip {
        clip_id,
        at,
        new_clip_id,
    })
}

/// A layer target can be either a clip within a track or a whole track.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LayerTarget {
    Clip(ClipId),
    Track(TrackId),
}

pub fn build_set_clip_order_command(
    project: &Project,
    clip_id: ClipId,
    order: i64,
) -> Result<EditCommand, TimelineError> {
    let _ = editable_clip(project, clip_id)?;
    Ok(EditCommand::SetClipOrder { clip_id, order })
}

pub fn build_set_track_order_command(
    project: &Project,
    track_id: TrackId,
    order: i64,
) -> Result<EditCommand, TimelineError> {
    let _ = ensure_track(project, track_id)?;
    Ok(EditCommand::SetTrackOrder { track_id, order })
}

pub fn build_set_layer_order_command(
    project: &Project,
    target: LayerTarget,
    order: i64,
) -> Result<EditCommand, TimelineError> {
    match target {
        LayerTarget::Clip(clip_id) => build_set_clip_order_command(project, clip_id, order),
        LayerTarget::Track(track_id) => build_set_track_order_command(project, track_id, order),
    }
}

/// A visibility target can be a clip or a track.  Lock is intentionally a
/// track operation because that is the lock granularity in the project model.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VisibilityTarget {
    Clip(ClipId),
    Track(TrackId),
}

pub fn build_set_clip_visibility_command(
    project: &Project,
    clip_id: ClipId,
    visible: bool,
) -> Result<EditCommand, TimelineError> {
    let _ = editable_clip(project, clip_id)?;
    Ok(EditCommand::SetClipVisibility { clip_id, visible })
}

pub fn build_set_track_visibility_command(
    project: &Project,
    track_id: TrackId,
    visible: bool,
) -> Result<EditCommand, TimelineError> {
    ensure_track(project, track_id)?;
    Ok(EditCommand::SetTrackVisibility { track_id, visible })
}

pub fn build_set_track_locked_command(
    project: &Project,
    track_id: TrackId,
    locked: bool,
) -> Result<EditCommand, TimelineError> {
    ensure_track(project, track_id)?;
    Ok(EditCommand::SetTrackLocked { track_id, locked })
}

pub fn build_set_visibility_command(
    project: &Project,
    target: VisibilityTarget,
    visible: bool,
) -> Result<EditCommand, TimelineError> {
    match target {
        VisibilityTarget::Clip(clip_id) => {
            build_set_clip_visibility_command(project, clip_id, visible)
        }
        VisibilityTarget::Track(track_id) => {
            build_set_track_visibility_command(project, track_id, visible)
        }
    }
}

fn ensure_track(project: &Project, track_id: TrackId) -> Result<&Track, TimelineError> {
    project
        .track(track_id)
        .ok_or(TimelineError::Project(ProjectError::TrackNotFound(
            track_id,
        )))
}

fn editable_clip(project: &Project, clip_id: ClipId) -> Result<(&Clip, TrackId), TimelineError> {
    let track_id = project
        .clip_track(clip_id)
        .ok_or(TimelineError::Project(ProjectError::ClipNotFound(clip_id)))?;
    let track = ensure_track(project, track_id)?;
    if track.locked {
        return Err(TimelineError::TrackLocked(track_id));
    }
    let clip = project
        .clip(clip_id)
        .ok_or(TimelineError::Project(ProjectError::ClipNotFound(clip_id)))?;
    Ok((clip, track_id))
}

/// One grouped move represented by one model `Batch` command.  Calling
/// `ProjectHistory::execute(transaction.command())` therefore creates one undo
/// entry for the entire drag.
#[derive(Clone, Debug, PartialEq)]
pub struct DragTransaction {
    commands: Vec<EditCommand>,
}

impl DragTransaction {
    fn from_commands(commands: Vec<EditCommand>) -> Self {
        Self { commands }
    }

    pub fn commands(&self) -> &[EditCommand] {
        &self.commands
    }

    pub fn is_empty(&self) -> bool {
        self.commands.is_empty()
    }

    pub fn command(&self) -> EditCommand {
        EditCommand::Batch {
            commands: self.commands.clone(),
        }
    }

    pub fn into_command(self) -> EditCommand {
        EditCommand::Batch {
            commands: self.commands,
        }
    }

    pub fn apply(&self, project: &mut Project) -> Result<CommandReceipt, TimelineError> {
        Ok(self.command().apply(project)?)
    }
}

/// Build one grouped move transaction.  Every selected clip receives the
/// same exact delta, and all validation happens before the batch is returned
/// so a locked/invalid member cannot produce a partial drag.
pub fn build_grouped_drag_transaction(
    project: &Project,
    clip_ids: &[ClipId],
    delta: Time,
) -> Result<DragTransaction, TimelineError> {
    if clip_ids.is_empty() {
        return Err(TimelineError::EmptySelection);
    }
    let mut ids = clip_ids.to_vec();
    ids.sort_unstable();
    let mut unique = BTreeSet::new();
    for clip_id in ids {
        if !unique.insert(clip_id) {
            return Err(TimelineError::DuplicateSelection(clip_id));
        }
    }

    let mut commands = Vec::with_capacity(unique.len());
    for clip_id in unique {
        let (clip, _) = editable_clip(project, clip_id)?;
        let new_start = clip.range.start.checked_add(delta)?;
        if new_start < Time::ZERO {
            return Err(TimelineError::InvalidArgument(
                "grouped move cannot move a clip before time zero",
            ));
        }
        let _ = new_start.checked_add(clip.duration()?)?;
        commands.push(EditCommand::MoveClip { clip_id, new_start });
    }
    Ok(DragTransaction::from_commands(commands))
}

pub fn build_grouped_move_command(
    project: &Project,
    clip_ids: &[ClipId],
    delta: Time,
) -> Result<EditCommand, TimelineError> {
    Ok(build_grouped_drag_transaction(project, clip_ids, delta)?.into_command())
}

/// Name suited to pointer handlers that already have a drag transaction.
pub fn build_grouped_drag_command(
    project: &Project,
    clip_ids: &[ClipId],
    delta: Time,
) -> Result<EditCommand, TimelineError> {
    build_grouped_move_command(project, clip_ids, delta)
}

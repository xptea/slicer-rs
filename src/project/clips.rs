//! Timeline tracks, clip payloads, and canonical visual properties.

use super::assets::{Asset, AssetId, AssetKind};
use super::time::{RationalError, Time, TimeRange};
use serde::{Deserialize, Serialize};
use std::error::Error;
use std::f64::consts::PI;
use std::fmt;

/// Output canvas dimensions in pixels.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Canvas {
    pub width: u32,
    pub height: u32,
}

impl Canvas {
    pub fn new(width: u32, height: u32) -> Result<Self, ClipError> {
        if width == 0 || height == 0 {
            return Err(ClipError::InvalidCanvas);
        }
        Ok(Self { width, height })
    }

    pub fn aspect_ratio(self) -> Result<f64, ClipError> {
        if self.height == 0 {
            return Err(ClipError::InvalidCanvas);
        }
        Ok(f64::from(self.width) / f64::from(self.height))
    }
}

/// A finite two-dimensional point in canonical canvas or source-pixel space.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Point {
    pub x: f64,
    pub y: f64,
}

impl Point {
    pub const ZERO: Self = Self { x: 0.0, y: 0.0 };

    pub const fn new(x: f64, y: f64) -> Self {
        Self { x, y }
    }

    fn validate(self) -> Result<(), ClipError> {
        if !self.x.is_finite() || !self.y.is_finite() {
            return Err(ClipError::NonFiniteGeometry);
        }
        Ok(())
    }
}

/// A source-pixel crop rectangle.  Its right and bottom edges are exclusive.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CropRect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

impl CropRect {
    pub fn new(x: u32, y: u32, width: u32, height: u32) -> Result<Self, ClipError> {
        let crop = Self {
            x,
            y,
            width,
            height,
        };
        crop.validate()?;
        Ok(crop)
    }

    pub fn validate(self) -> Result<(), ClipError> {
        if self.width == 0 || self.height == 0 {
            return Err(ClipError::InvalidCrop);
        }
        self.x
            .checked_add(self.width)
            .ok_or(ClipError::CropOverflow)?;
        self.y
            .checked_add(self.height)
            .ok_or(ClipError::CropOverflow)?;
        Ok(())
    }

    pub fn right(self) -> Result<u32, ClipError> {
        self.x
            .checked_add(self.width)
            .ok_or(ClipError::CropOverflow)
    }

    pub fn bottom(self) -> Result<u32, ClipError> {
        self.y
            .checked_add(self.height)
            .ok_or(ClipError::CropOverflow)
    }

    pub fn fits_within(self, width: u32, height: u32) -> Result<bool, ClipError> {
        self.validate()?;
        Ok(self.right()? <= width && self.bottom()? <= height)
    }
}

/// RGBA color in the working color space, with channels in `[0, 1]`.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Color {
    pub r: f64,
    pub g: f64,
    pub b: f64,
    pub a: f64,
}

impl Color {
    pub const BLACK: Self = Self {
        r: 0.0,
        g: 0.0,
        b: 0.0,
        a: 1.0,
    };

    pub const TRANSPARENT: Self = Self {
        r: 0.0,
        g: 0.0,
        b: 0.0,
        a: 0.0,
    };

    pub fn new(r: f64, g: f64, b: f64, a: f64) -> Result<Self, ClipError> {
        let color = Self { r, g, b, a };
        color.validate()?;
        Ok(color)
    }

    pub fn validate(self) -> Result<(), ClipError> {
        for channel in [self.r, self.g, self.b, self.a] {
            if !channel.is_finite() || !(0.0..=1.0).contains(&channel) {
                return Err(ClipError::InvalidColor);
            }
        }
        Ok(())
    }
}

/// The operation order is fixed for all preview/export consumers:
/// crop source pixels, subtract the normalized anchor, scale, rotate in the
/// canvas plane, then translate to `position`.  `position` is the transformed
/// anchor in canvas pixels.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Transform {
    pub position: Point,
    /// Normalized anchor in the cropped source extent, normally in `[0, 1]`.
    pub anchor: Point,
    pub scale: Point,
    pub rotation_degrees: f64,
    pub crop: Option<CropRect>,
    pub opacity: f64,
}

impl Default for Transform {
    fn default() -> Self {
        Self::identity()
    }
}

impl Transform {
    pub const fn identity() -> Self {
        Self {
            position: Point::ZERO,
            anchor: Point::ZERO,
            scale: Point { x: 1.0, y: 1.0 },
            rotation_degrees: 0.0,
            crop: None,
            opacity: 1.0,
        }
    }

    pub fn validate(&self) -> Result<(), ClipError> {
        self.position.validate()?;
        self.anchor.validate()?;
        self.scale.validate()?;
        if !(0.0..=1.0).contains(&self.anchor.x) || !(0.0..=1.0).contains(&self.anchor.y) {
            return Err(ClipError::InvalidAnchor);
        }
        if self.scale.x == 0.0 || self.scale.y == 0.0 {
            return Err(ClipError::ZeroScale);
        }
        if !self.rotation_degrees.is_finite() {
            return Err(ClipError::NonFiniteGeometry);
        }
        if !self.opacity.is_finite() || !(0.0..=1.0).contains(&self.opacity) {
            return Err(ClipError::InvalidOpacity);
        }
        if let Some(crop) = self.crop {
            crop.validate()?;
        }
        Ok(())
    }

    pub fn is_identity(&self) -> bool {
        *self == Self::identity()
    }

    pub fn local_extent(self, source_width: u32, source_height: u32) -> Result<Point, ClipError> {
        self.validate()?;
        if let Some(crop) = self.crop {
            if !crop.fits_within(source_width, source_height)? {
                return Err(ClipError::CropOutOfBounds);
            }
            Ok(Point::new(f64::from(crop.width), f64::from(crop.height)))
        } else {
            Ok(Point::new(
                f64::from(source_width),
                f64::from(source_height),
            ))
        }
    }

    /// Transform a point expressed relative to the cropped source's top-left.
    pub fn forward_point(
        self,
        source_width: u32,
        source_height: u32,
        local: Point,
    ) -> Result<Point, ClipError> {
        let extent = self.local_extent(source_width, source_height)?;
        local.validate()?;
        let centered = Point::new(
            (local.x - extent.x * self.anchor.x) * self.scale.x,
            (local.y - extent.y * self.anchor.y) * self.scale.y,
        );
        let angle = self.rotation_degrees.to_radians();
        let (sin, cos) = angle.sin_cos();
        Ok(Point::new(
            self.position.x + centered.x * cos - centered.y * sin,
            self.position.y + centered.x * sin + centered.y * cos,
        ))
    }

    /// Inverse of [`Transform::forward_point`], used by hit testing.
    pub fn inverse_point(
        self,
        source_width: u32,
        source_height: u32,
        canvas_point: Point,
    ) -> Result<Point, ClipError> {
        let extent = self.local_extent(source_width, source_height)?;
        canvas_point.validate()?;
        let translated = Point::new(
            canvas_point.x - self.position.x,
            canvas_point.y - self.position.y,
        );
        let angle = -self.rotation_degrees.to_radians();
        let (sin, cos) = angle.sin_cos();
        let rotated = Point::new(
            translated.x * cos - translated.y * sin,
            translated.x * sin + translated.y * cos,
        );
        Ok(Point::new(
            rotated.x / self.scale.x + extent.x * self.anchor.x,
            rotated.y / self.scale.y + extent.y * self.anchor.y,
        ))
    }

    pub fn contains_canvas_point(
        self,
        source_width: u32,
        source_height: u32,
        canvas_point: Point,
    ) -> Result<bool, ClipError> {
        let local = self.inverse_point(source_width, source_height, canvas_point)?;
        let extent = self.local_extent(source_width, source_height)?;
        Ok(local.x >= 0.0 && local.y >= 0.0 && local.x < extent.x && local.y < extent.y)
    }

    /// Row-major affine matrix `[a b tx; c d ty; 0 0 1]` for GPU/export code.
    pub fn matrix(self, source_width: u32, source_height: u32) -> Result<[f64; 9], ClipError> {
        let extent = self.local_extent(source_width, source_height)?;
        let angle = self.rotation_degrees.to_radians();
        let (sin, cos) = angle.sin_cos();
        let a = cos * self.scale.x;
        let b = -sin * self.scale.y;
        let c = sin * self.scale.x;
        let d = cos * self.scale.y;
        let tx = self.position.x - a * extent.x * self.anchor.x - b * extent.y * self.anchor.y;
        let ty = self.position.y - c * extent.x * self.anchor.x - d * extent.y * self.anchor.y;
        Ok([a, b, tx, c, d, ty, 0.0, 0.0, 1.0])
    }
}

/// How text is aligned inside its wrapping box.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum TextAlignment {
    Left,
    Center,
    Right,
    Justify,
}

/// Text layout inputs shared by preview and offline export.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TextStyle {
    pub font_family: String,
    pub font_size: f64,
    pub alignment: TextAlignment,
    pub wrapping_width: Option<f64>,
    pub line_height: f64,
    pub color: Color,
}

impl Default for TextStyle {
    fn default() -> Self {
        Self {
            font_family: "sans-serif".to_owned(),
            font_size: 48.0,
            alignment: TextAlignment::Left,
            wrapping_width: None,
            line_height: 1.2,
            color: Color::new(1.0, 1.0, 1.0, 1.0).expect("valid default text color"),
        }
    }
}

impl TextStyle {
    pub fn validate(&self) -> Result<(), ClipError> {
        if self.font_family.trim().is_empty() {
            return Err(ClipError::EmptyFontFamily);
        }
        if !self.font_size.is_finite() || self.font_size <= 0.0 {
            return Err(ClipError::InvalidTextSize);
        }
        if let Some(width) = self.wrapping_width
            && (!width.is_finite() || width <= 0.0)
        {
            return Err(ClipError::InvalidWrapWidth);
        }
        if !self.line_height.is_finite() || self.line_height <= 0.0 {
            return Err(ClipError::InvalidLineHeight);
        }
        self.color.validate()
    }
}

/// Linear audio gain and per-instance mute state.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AudioSettings {
    pub gain: f64,
    pub muted: bool,
}

impl Default for AudioSettings {
    fn default() -> Self {
        Self {
            gain: 1.0,
            muted: false,
        }
    }
}

impl AudioSettings {
    pub fn validate(self) -> Result<(), ClipError> {
        if !self.gain.is_finite() || self.gain < 0.0 {
            return Err(ClipError::InvalidGain);
        }
        Ok(())
    }
}

/// Embedded audio behavior for a video instance.  An explicit policy avoids
/// playing a video's stream twice when an editor also creates an audio clip.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub enum EmbeddedAudio {
    Disabled,
    Enabled(AudioSettings),
}

impl Default for EmbeddedAudio {
    fn default() -> Self {
        Self::Enabled(AudioSettings::default())
    }
}

impl EmbeddedAudio {
    pub fn validate(self) -> Result<(), ClipError> {
        match self {
            Self::Disabled => Ok(()),
            Self::Enabled(settings) => settings.validate(),
        }
    }

    pub fn settings(self) -> Option<AudioSettings> {
        match self {
            Self::Disabled => None,
            Self::Enabled(settings) => Some(settings),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VideoClip {
    pub asset_id: AssetId,
    pub source_range: TimeRange,
    pub audio: EmbeddedAudio,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImageClip {
    pub asset_id: AssetId,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TextClip {
    pub text: String,
    pub style: TextStyle,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum ShapeKind {
    Rectangle,
    Ellipse,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Stroke {
    pub color: Color,
    pub width: f64,
}

impl Stroke {
    pub fn validate(self) -> Result<(), ClipError> {
        if !self.width.is_finite() || self.width < 0.0 {
            return Err(ClipError::InvalidStrokeWidth);
        }
        self.color.validate()
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ShapeClip {
    pub shape: ShapeKind,
    pub fill: Color,
    pub stroke: Option<Stroke>,
    pub corner_radius: f64,
    /// Shape bounds in canonical canvas pixels before the clip transform.
    #[serde(default = "default_shape_width")]
    pub width: f64,
    #[serde(default = "default_shape_height")]
    pub height: f64,
}

impl Default for ShapeClip {
    fn default() -> Self {
        Self {
            shape: ShapeKind::Rectangle,
            fill: Color::new(1.0, 1.0, 1.0, 1.0).expect("valid default shape color"),
            stroke: None,
            corner_radius: 0.0,
            width: default_shape_width(),
            height: default_shape_height(),
        }
    }
}

fn default_shape_width() -> f64 {
    100.0
}

fn default_shape_height() -> f64 {
    100.0
}

impl ShapeClip {
    pub fn validate(&self) -> Result<(), ClipError> {
        self.fill.validate()?;
        if !self.corner_radius.is_finite() || self.corner_radius < 0.0 {
            return Err(ClipError::InvalidCornerRadius);
        }
        if !self.width.is_finite()
            || !self.height.is_finite()
            || self.width <= 0.0
            || self.height <= 0.0
        {
            return Err(ClipError::InvalidShapeSize);
        }
        if let Some(stroke) = self.stroke {
            stroke.validate()?;
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AudioClip {
    pub asset_id: AssetId,
    pub source_range: TimeRange,
    pub settings: AudioSettings,
}

/// Exactly one content kind is stored for each clip.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum ClipKind {
    Video(VideoClip),
    Image(ImageClip),
    Text(TextClip),
    Shape(ShapeClip),
    Audio(AudioClip),
}

impl ClipKind {
    pub fn is_visual(&self) -> bool {
        !matches!(self, Self::Audio(_))
    }

    pub fn is_audio(&self) -> bool {
        matches!(self, Self::Video(_) | Self::Audio(_))
    }

    pub fn asset_id(&self) -> Option<AssetId> {
        match self {
            Self::Video(clip) => Some(clip.asset_id),
            Self::Image(clip) => Some(clip.asset_id),
            Self::Text(_) | Self::Shape(_) => None,
            Self::Audio(clip) => Some(clip.asset_id),
        }
    }

    pub fn source_range(&self) -> Option<TimeRange> {
        match self {
            Self::Video(clip) => Some(clip.source_range),
            Self::Audio(clip) => Some(clip.source_range),
            Self::Image(_) | Self::Text(_) | Self::Shape(_) => None,
        }
    }

    pub fn audio_settings(&self) -> Option<AudioSettings> {
        match self {
            Self::Video(clip) => clip.audio.settings(),
            Self::Audio(clip) => Some(clip.settings),
            Self::Image(_) | Self::Text(_) | Self::Shape(_) => None,
        }
    }
}

/// A timeline instance.  Multiple clips may reference one asset; clip IDs
/// remain distinct so each instance can have its own range and transform.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Clip {
    pub id: super::assets::ClipId,
    pub range: TimeRange,
    /// Lower order is behind higher order.  Clip ID is the final tie breaker.
    pub order: i64,
    pub visible: bool,
    pub transform: Transform,
    pub kind: ClipKind,
}

impl Clip {
    pub fn new(id: super::assets::ClipId, range: TimeRange, kind: ClipKind) -> Self {
        Self {
            id,
            range,
            order: 0,
            visible: true,
            transform: Transform::identity(),
            kind,
        }
    }

    pub fn video(
        id: super::assets::ClipId,
        range: TimeRange,
        asset_id: AssetId,
        source_range: TimeRange,
    ) -> Self {
        Self::new(
            id,
            range,
            ClipKind::Video(VideoClip {
                asset_id,
                source_range,
                audio: EmbeddedAudio::default(),
            }),
        )
    }

    pub fn image(id: super::assets::ClipId, range: TimeRange, asset_id: AssetId) -> Self {
        Self::new(id, range, ClipKind::Image(ImageClip { asset_id }))
    }

    pub fn text(id: super::assets::ClipId, range: TimeRange, text: impl Into<String>) -> Self {
        Self::new(
            id,
            range,
            ClipKind::Text(TextClip {
                text: text.into(),
                style: TextStyle::default(),
            }),
        )
    }

    pub fn shape(id: super::assets::ClipId, range: TimeRange, shape: ShapeClip) -> Self {
        Self::new(id, range, ClipKind::Shape(shape))
    }

    pub fn audio(
        id: super::assets::ClipId,
        range: TimeRange,
        asset_id: AssetId,
        source_range: TimeRange,
    ) -> Self {
        Self::new(
            id,
            range,
            ClipKind::Audio(AudioClip {
                asset_id,
                source_range,
                settings: AudioSettings::default(),
            }),
        )
    }

    pub fn duration(&self) -> Result<Time, RationalError> {
        self.range.duration()
    }

    pub fn is_active(&self, time: Time) -> bool {
        self.visible && self.range.contains(time)
    }

    /// Map an active project time to source time at speed 1.  The end of the
    /// clip is exclusive and therefore never maps to the source end.
    pub fn source_time_at(&self, project_time: Time) -> Result<Option<Time>, RationalError> {
        if !self.range.contains(project_time) {
            return Ok(None);
        }
        let Some(source_range) = self.kind.source_range() else {
            return Ok(None);
        };
        let elapsed = project_time.checked_sub(self.range.start)?;
        Ok(Some(source_range.start.checked_add(elapsed)?))
    }

    pub fn source_time(&self, project_time: Time) -> Result<Option<Time>, RationalError> {
        self.source_time_at(project_time)
    }

    pub fn validate(&self) -> Result<(), ClipError> {
        if self.id.is_zero() {
            return Err(ClipError::ZeroClipId);
        }
        if self.range.start < Time::ZERO {
            return Err(ClipError::NegativeTimelineTime);
        }
        self.transform.validate()?;
        match &self.kind {
            ClipKind::Video(video) => {
                video.source_range.duration().map_err(ClipError::Time)?;
                video.audio.validate()?;
            }
            ClipKind::Image(_) => {}
            ClipKind::Text(text) => text.style.validate()?,
            ClipKind::Shape(shape) => shape.validate()?,
            ClipKind::Audio(audio) => {
                audio.source_range.duration().map_err(ClipError::Time)?;
                audio.settings.validate()?
            }
        }
        Ok(())
    }

    pub fn source_range_mut(&mut self) -> Option<&mut TimeRange> {
        match &mut self.kind {
            ClipKind::Video(video) => Some(&mut video.source_range),
            ClipKind::Audio(audio) => Some(&mut audio.source_range),
            ClipKind::Image(_) | ClipKind::Text(_) | ClipKind::Shape(_) => None,
        }
    }

    pub fn source_range_ref(&self) -> Option<&TimeRange> {
        match &self.kind {
            ClipKind::Video(video) => Some(&video.source_range),
            ClipKind::Audio(audio) => Some(&audio.source_range),
            ClipKind::Image(_) | ClipKind::Text(_) | ClipKind::Shape(_) => None,
        }
    }

    pub fn asset_kind_compatible(&self, asset: &Asset) -> bool {
        match &self.kind {
            ClipKind::Video(_) => asset.kind == AssetKind::Video,
            ClipKind::Image(_) => asset.kind == AssetKind::Image,
            ClipKind::Audio(_) => matches!(asset.kind, AssetKind::Audio | AssetKind::Video),
            ClipKind::Text(_) | ClipKind::Shape(_) => false,
        }
    }
}

/// Track order is part of the total stacking key.  Lower track order is
/// behind higher track order; clip order and then IDs break all ties.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum TrackKind {
    Visual,
    Audio,
    Mixed,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Track {
    pub id: super::assets::TrackId,
    pub name: String,
    pub order: i64,
    pub kind: TrackKind,
    pub visible: bool,
    pub locked: bool,
    pub clips: Vec<Clip>,
}

impl Track {
    pub fn new(id: super::assets::TrackId, name: impl Into<String>, order: i64) -> Self {
        Self {
            id,
            name: name.into(),
            order,
            kind: TrackKind::Mixed,
            visible: true,
            locked: false,
            clips: Vec::new(),
        }
    }

    pub fn add_clip(&mut self, clip: Clip) -> Result<(), ClipError> {
        clip.validate()?;
        if self.clips.iter().any(|existing| existing.id == clip.id) {
            return Err(ClipError::DuplicateClipId);
        }
        self.clips.push(clip);
        Ok(())
    }

    pub fn remove_clip(&mut self, clip_id: super::assets::ClipId) -> Option<Clip> {
        let position = self.clips.iter().position(|clip| clip.id == clip_id)?;
        Some(self.clips.remove(position))
    }

    pub fn clip(&self, clip_id: super::assets::ClipId) -> Option<&Clip> {
        self.clips.iter().find(|clip| clip.id == clip_id)
    }

    pub fn clip_mut(&mut self, clip_id: super::assets::ClipId) -> Option<&mut Clip> {
        self.clips.iter_mut().find(|clip| clip.id == clip_id)
    }

    pub fn validate(&self) -> Result<(), ClipError> {
        if self.id.is_zero() {
            return Err(ClipError::ZeroTrackId);
        }
        let mut ids = std::collections::BTreeSet::new();
        for clip in &self.clips {
            clip.validate()?;
            if !ids.insert(clip.id) {
                return Err(ClipError::DuplicateClipId);
            }
            if self.kind == TrackKind::Visual && !clip.kind.is_visual() {
                return Err(ClipError::TrackKindMismatch);
            }
            if self.kind == TrackKind::Audio && !clip.kind.is_audio() {
                return Err(ClipError::TrackKindMismatch);
            }
        }
        Ok(())
    }
}

/// Clip and geometry validation failures.
#[derive(Clone, Debug, PartialEq)]
pub enum ClipError {
    ZeroTrackId,
    ZeroClipId,
    DuplicateClipId,
    InvalidCanvas,
    InvalidCrop,
    CropOverflow,
    CropOutOfBounds,
    NonFiniteGeometry,
    InvalidAnchor,
    ZeroScale,
    InvalidOpacity,
    InvalidColor,
    EmptyFontFamily,
    InvalidTextSize,
    InvalidWrapWidth,
    InvalidLineHeight,
    InvalidGain,
    InvalidStrokeWidth,
    InvalidCornerRadius,
    InvalidShapeSize,
    NegativeTimelineTime,
    TrackKindMismatch,
    Time(RationalError),
}

impl fmt::Display for ClipError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ZeroTrackId => formatter.write_str("track id must be non-zero"),
            Self::ZeroClipId => formatter.write_str("clip id must be non-zero"),
            Self::DuplicateClipId => formatter.write_str("clip id is already present in the track"),
            Self::InvalidCanvas => formatter.write_str("canvas dimensions must be positive"),
            Self::InvalidCrop => formatter.write_str("crop dimensions must be positive"),
            Self::CropOverflow => formatter.write_str("crop rectangle overflows source dimensions"),
            Self::CropOutOfBounds => {
                formatter.write_str("crop rectangle is outside source dimensions")
            }
            Self::NonFiniteGeometry => formatter.write_str("geometry contains a non-finite value"),
            Self::InvalidAnchor => formatter.write_str("anchor must be normalized to [0, 1]"),
            Self::ZeroScale => formatter.write_str("scale components must be non-zero"),
            Self::InvalidOpacity => formatter.write_str("opacity must be finite and in [0, 1]"),
            Self::InvalidColor => {
                formatter.write_str("color channels must be finite and in [0, 1]")
            }
            Self::EmptyFontFamily => formatter.write_str("font family must not be empty"),
            Self::InvalidTextSize => formatter.write_str("font size must be positive and finite"),
            Self::InvalidWrapWidth => {
                formatter.write_str("wrapping width must be positive and finite")
            }
            Self::InvalidLineHeight => {
                formatter.write_str("line height must be positive and finite")
            }
            Self::InvalidGain => formatter.write_str("audio gain must be finite and non-negative"),
            Self::InvalidStrokeWidth => {
                formatter.write_str("stroke width must be finite and non-negative")
            }
            Self::InvalidCornerRadius => {
                formatter.write_str("corner radius must be finite and non-negative")
            }
            Self::InvalidShapeSize => {
                formatter.write_str("shape width and height must be positive")
            }
            Self::NegativeTimelineTime => formatter.write_str("timeline time must be non-negative"),
            Self::TrackKindMismatch => {
                formatter.write_str("clip kind is incompatible with track kind")
            }
            Self::Time(error) => error.fmt(formatter),
        }
    }
}

impl Error for ClipError {}

impl From<RationalError> for ClipError {
    fn from(error: RationalError) -> Self {
        Self::Time(error)
    }
}

/// Utility used by callers implementing a rotation-aware bounding box.
pub fn rotated_extent(width: f64, height: f64, rotation_degrees: f64) -> Result<Point, ClipError> {
    if !width.is_finite() || !height.is_finite() || width < 0.0 || height < 0.0 {
        return Err(ClipError::NonFiniteGeometry);
    }
    if !rotation_degrees.is_finite() {
        return Err(ClipError::NonFiniteGeometry);
    }
    let angle = rotation_degrees * PI / 180.0;
    let (sin, cos) = angle.sin_cos();
    Ok(Point::new(
        width * cos.abs() + height * sin.abs(),
        width * sin.abs() + height * cos.abs(),
    ))
}

//! Authoritative project assets and source metadata.

use super::time::{FrameRate, Rational, RationalError, Time};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::error::Error;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_STABLE_ID: AtomicU64 = AtomicU64::new(1);

macro_rules! stable_id {
    ($name:ident) => {
        /// A stable identifier persisted in the project file.
        #[derive(
            Clone,
            Copy,
            Debug,
            Default,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd,
            Serialize,
            Deserialize,
        )]
        #[serde(transparent)]
        pub struct $name(pub u64);

        impl $name {
            pub const fn new(value: u64) -> Self {
                Self(value)
            }

            pub fn fresh() -> Self {
                loop {
                    let value = NEXT_STABLE_ID.fetch_add(1, Ordering::Relaxed);
                    if value != 0 {
                        return Self(value);
                    }
                }
            }

            pub const fn value(self) -> u64 {
                self.0
            }

            pub const fn is_zero(self) -> bool {
                self.0 == 0
            }
        }

        impl From<u64> for $name {
            fn from(value: u64) -> Self {
                Self::new(value)
            }
        }

        impl From<$name> for u64 {
            fn from(value: $name) -> Self {
                value.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.0.fmt(formatter)
            }
        }
    };
}

stable_id!(ProjectId);
stable_id!(AssetId);
stable_id!(TrackId);
stable_id!(ClipId);

/// The media kind represented by an asset entry.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum AssetKind {
    Video,
    Image,
    Audio,
}

/// Orientation metadata from a source stream.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub enum Orientation {
    #[default]
    Normal,
    Rotate90,
    Rotate180,
    Rotate270,
}

impl Orientation {
    pub const fn degrees(self) -> u16 {
        match self {
            Self::Normal => 0,
            Self::Rotate90 => 90,
            Self::Rotate180 => 180,
            Self::Rotate270 => 270,
        }
    }
}

/// A presentation timestamp retained in the source stream's integer time
/// base.  Keeping this alongside converted project time avoids throwing away
/// VFR/B-frame timestamp information at the model boundary.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceTimestamp {
    pub value: i64,
    pub time_base: Rational,
}

impl SourceTimestamp {
    pub fn new(value: i64, time_base: Rational) -> Self {
        Self { value, time_base }
    }

    pub fn as_time(self) -> Result<Time, RationalError> {
        self.time_base.checked_mul_integer(self.value)
    }
}

/// One decoded source frame.  `pts` and `duration` are expressed in the
/// containing video's `time_base` ticks.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceFrame {
    pub index: u64,
    pub pts: i64,
    pub duration: Option<u64>,
}

impl SourceFrame {
    pub const fn new(index: u64, pts: i64, duration: Option<u64>) -> Self {
        Self {
            index,
            pts,
            duration,
        }
    }
}

/// The source frame selected by an exact project-time lookup.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceFrameSelection {
    pub index: u64,
    pub pts: SourceTimestamp,
    pub duration: Option<Time>,
}

/// Video stream metadata needed for source-time mapping and composition.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VideoMetadata {
    pub duration: Time,
    pub width: u32,
    pub height: u32,
    pub time_base: Rational,
    pub frame_rate: Option<FrameRate>,
    /// Optional presentation timestamps in decode/presentation order.  An
    /// empty list is valid when the inspector has not enumerated every frame.
    pub frames: Vec<SourceFrame>,
    pub orientation: Orientation,
    pub pixel_aspect: Rational,
    pub has_audio: bool,
}

impl VideoMetadata {
    pub fn new(width: u32, height: u32, duration: Time) -> Result<Self, AssetError> {
        validate_dimensions(width, height)?;
        if duration <= Time::ZERO {
            return Err(AssetError::NonPositiveDuration);
        }
        Ok(Self {
            duration,
            width,
            height,
            time_base: Rational::ONE,
            frame_rate: None,
            frames: Vec::new(),
            orientation: Orientation::Normal,
            pixel_aspect: Rational::ONE,
            has_audio: false,
        })
    }

    pub fn validate(&self) -> Result<(), AssetError> {
        validate_dimensions(self.width, self.height)?;
        if self.duration <= Time::ZERO {
            return Err(AssetError::NonPositiveDuration);
        }
        if self.time_base <= Time::ZERO {
            return Err(AssetError::InvalidTimeBase);
        }
        if self.pixel_aspect <= Time::ZERO {
            return Err(AssetError::InvalidPixelAspect);
        }

        let mut previous_pts = None;
        for frame in &self.frames {
            if let Some(previous) = previous_pts
                && frame.pts <= previous
            {
                return Err(AssetError::FramesNotSorted);
            }
            if frame.duration == Some(0) {
                return Err(AssetError::ZeroFrameDuration);
            }
            previous_pts = Some(frame.pts);
        }
        Ok(())
    }

    /// Find the VFR frame whose presentation interval contains `source_time`.
    /// Intervals are half-open, so a timestamp exactly at a frame boundary
    /// selects the new frame.
    pub fn frame_at(&self, source_time: Time) -> Result<Option<SourceFrameSelection>, AssetError> {
        self.validate()?;
        if source_time < Time::ZERO || source_time >= self.duration {
            return Ok(None);
        }
        if self.frames.is_empty() {
            return Ok(None);
        }

        let source_tick = source_time
            .checked_div(self.time_base)
            .map_err(AssetError::Time)?;
        for (position, frame) in self.frames.iter().enumerate() {
            let start = Time::from_integer(frame.pts);
            if source_tick < start {
                break;
            }
            let end = if let Some(duration) = frame.duration {
                start
                    .checked_add(Time::from_integer(
                        i64::try_from(duration)
                            .map_err(|_| AssetError::Time(RationalError::Overflow))?,
                    ))
                    .map_err(AssetError::Time)?
            } else if let Some(next) = self.frames.get(position + 1) {
                Time::from_integer(next.pts)
            } else {
                self.duration
                    .checked_div(self.time_base)
                    .map_err(AssetError::Time)?
            };
            if source_tick < end {
                let duration = end
                    .checked_sub(start)
                    .map_err(AssetError::Time)?
                    .checked_mul(self.time_base)
                    .map_err(AssetError::Time)?;
                return Ok(Some(SourceFrameSelection {
                    index: frame.index,
                    pts: SourceTimestamp::new(frame.pts, self.time_base),
                    duration: Some(duration),
                }));
            }
        }
        Ok(None)
    }
}

/// Still-image metadata.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImageMetadata {
    pub width: u32,
    pub height: u32,
    pub orientation: Orientation,
    pub pixel_aspect: Rational,
}

impl ImageMetadata {
    pub fn new(width: u32, height: u32) -> Result<Self, AssetError> {
        validate_dimensions(width, height)?;
        Ok(Self {
            width,
            height,
            orientation: Orientation::Normal,
            pixel_aspect: Rational::ONE,
        })
    }

    pub fn validate(&self) -> Result<(), AssetError> {
        validate_dimensions(self.width, self.height)?;
        if self.pixel_aspect <= Time::ZERO {
            return Err(AssetError::InvalidPixelAspect);
        }
        Ok(())
    }
}

/// Audio stream metadata.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AudioMetadata {
    pub duration: Time,
    pub time_base: Rational,
    pub sample_rate: u32,
    pub channels: u16,
}

impl AudioMetadata {
    pub fn new(duration: Time, sample_rate: u32, channels: u16) -> Result<Self, AssetError> {
        let metadata = Self {
            duration,
            time_base: Rational::ONE,
            sample_rate,
            channels,
        };
        metadata.validate()?;
        Ok(metadata)
    }

    pub fn validate(&self) -> Result<(), AssetError> {
        if self.duration <= Time::ZERO {
            return Err(AssetError::NonPositiveDuration);
        }
        if self.time_base <= Time::ZERO {
            return Err(AssetError::InvalidTimeBase);
        }
        if self.sample_rate == 0 || self.channels == 0 {
            return Err(AssetError::InvalidAudioLayout);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum AssetMetadata {
    Video(VideoMetadata),
    Image(ImageMetadata),
    Audio(AudioMetadata),
}

impl AssetMetadata {
    pub fn kind(&self) -> AssetKind {
        match self {
            Self::Video(_) => AssetKind::Video,
            Self::Image(_) => AssetKind::Image,
            Self::Audio(_) => AssetKind::Audio,
        }
    }

    pub fn duration(&self) -> Option<Time> {
        match self {
            Self::Video(metadata) => Some(metadata.duration),
            Self::Image(_) => None,
            Self::Audio(metadata) => Some(metadata.duration),
        }
    }

    pub fn validate(&self) -> Result<(), AssetError> {
        match self {
            Self::Video(metadata) => metadata.validate(),
            Self::Image(metadata) => metadata.validate(),
            Self::Audio(metadata) => metadata.validate(),
        }
    }
}

/// Optional derived proxy information.  The original asset path remains the
/// only authoritative media reference.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProxyMetadata {
    pub path: PathBuf,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub source_revision: Option<u64>,
}

/// Optional derived cache information.  Cache files may be deleted and are
/// never required to open a project.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CacheMetadata {
    pub path: PathBuf,
    pub source_revision: Option<u64>,
}

/// An original media reference plus inspectable metadata and non-authoritative
/// derived-file hints.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Asset {
    pub id: AssetId,
    pub path: PathBuf,
    /// An absolute location remembered for relinking after a project moves.
    pub path_hint: Option<PathBuf>,
    pub kind: AssetKind,
    pub metadata: AssetMetadata,
    pub proxy: Option<ProxyMetadata>,
    pub cache: Option<CacheMetadata>,
}

impl Asset {
    pub fn video(
        id: AssetId,
        path: impl Into<PathBuf>,
        duration: Time,
        width: u32,
        height: u32,
    ) -> Result<Self, AssetError> {
        Ok(Self {
            id,
            path: checked_path(path.into())?,
            path_hint: None,
            kind: AssetKind::Video,
            metadata: AssetMetadata::Video(VideoMetadata::new(width, height, duration)?),
            proxy: None,
            cache: None,
        })
    }

    pub fn image(
        id: AssetId,
        path: impl Into<PathBuf>,
        width: u32,
        height: u32,
    ) -> Result<Self, AssetError> {
        Ok(Self {
            id,
            path: checked_path(path.into())?,
            path_hint: None,
            kind: AssetKind::Image,
            metadata: AssetMetadata::Image(ImageMetadata::new(width, height)?),
            proxy: None,
            cache: None,
        })
    }

    pub fn audio(
        id: AssetId,
        path: impl Into<PathBuf>,
        duration: Time,
        sample_rate: u32,
        channels: u16,
    ) -> Result<Self, AssetError> {
        Ok(Self {
            id,
            path: checked_path(path.into())?,
            path_hint: None,
            kind: AssetKind::Audio,
            metadata: AssetMetadata::Audio(AudioMetadata::new(duration, sample_rate, channels)?),
            proxy: None,
            cache: None,
        })
    }

    pub fn validate(&self) -> Result<(), AssetError> {
        if self.id.is_zero() {
            return Err(AssetError::ZeroId);
        }
        if self.path.as_os_str().is_empty() {
            return Err(AssetError::EmptyPath);
        }
        if self.kind != self.metadata.kind() {
            return Err(AssetError::KindMismatch);
        }
        self.metadata.validate()?;
        if let Some(proxy) = &self.proxy
            && proxy.path.as_os_str().is_empty()
        {
            return Err(AssetError::EmptyDerivedPath);
        }
        if let Some(cache) = &self.cache
            && cache.path.as_os_str().is_empty()
        {
            return Err(AssetError::EmptyDerivedPath);
        }
        Ok(())
    }

    pub fn video_metadata(&self) -> Option<&VideoMetadata> {
        match &self.metadata {
            AssetMetadata::Video(metadata) => Some(metadata),
            _ => None,
        }
    }

    pub fn image_metadata(&self) -> Option<&ImageMetadata> {
        match &self.metadata {
            AssetMetadata::Image(metadata) => Some(metadata),
            _ => None,
        }
    }

    pub fn audio_metadata(&self) -> Option<&AudioMetadata> {
        match &self.metadata {
            AssetMetadata::Audio(metadata) => Some(metadata),
            _ => None,
        }
    }

    pub fn duration(&self) -> Option<Time> {
        self.metadata.duration()
    }

    pub fn set_path(&mut self, path: impl Into<PathBuf>) -> Result<(), AssetError> {
        self.path = checked_path(path.into())?;
        Ok(())
    }

    pub fn path_is_absolute(&self) -> bool {
        self.path.is_absolute()
    }
}

fn checked_path(path: PathBuf) -> Result<PathBuf, AssetError> {
    if path.as_os_str().is_empty() {
        return Err(AssetError::EmptyPath);
    }
    Ok(path)
}

fn validate_dimensions(width: u32, height: u32) -> Result<(), AssetError> {
    if width == 0 || height == 0 {
        return Err(AssetError::InvalidDimensions);
    }
    Ok(())
}

/// An ID-keyed asset registry.  BTreeMap gives deterministic JSON and
/// deterministic iteration for scene/export consumers.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct AssetRegistry(BTreeMap<AssetId, Asset>);

impl AssetRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&mut self, asset: Asset) -> Result<(), AssetError> {
        asset.validate()?;
        if self.0.contains_key(&asset.id) {
            return Err(AssetError::DuplicateId);
        }
        self.0.insert(asset.id, asset);
        Ok(())
    }

    pub fn get(&self, id: AssetId) -> Option<&Asset> {
        self.0.get(&id)
    }

    pub fn get_mut(&mut self, id: AssetId) -> Option<&mut Asset> {
        self.0.get_mut(&id)
    }

    pub fn remove(&mut self, id: AssetId) -> Option<Asset> {
        self.0.remove(&id)
    }

    pub fn contains(&self, id: AssetId) -> bool {
        self.0.contains_key(&id)
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = (&AssetId, &Asset)> {
        self.0.iter()
    }

    pub fn values(&self) -> impl Iterator<Item = &Asset> {
        self.0.values()
    }

    pub fn validate(&self) -> Result<(), AssetError> {
        for (id, asset) in &self.0 {
            asset.validate()?;
            if id != &asset.id {
                return Err(AssetError::RegistryKeyMismatch);
            }
        }
        Ok(())
    }
}

impl<'a> IntoIterator for &'a AssetRegistry {
    type Item = (&'a AssetId, &'a Asset);
    type IntoIter = std::collections::btree_map::Iter<'a, AssetId, Asset>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.iter()
    }
}

/// Asset-model validation failures.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AssetError {
    ZeroId,
    DuplicateId,
    RegistryKeyMismatch,
    EmptyPath,
    EmptyDerivedPath,
    KindMismatch,
    InvalidDimensions,
    NonPositiveDuration,
    InvalidTimeBase,
    InvalidPixelAspect,
    InvalidAudioLayout,
    FramesNotSorted,
    ZeroFrameDuration,
    Time(RationalError),
}

impl fmt::Display for AssetError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ZeroId => formatter.write_str("asset id must be non-zero"),
            Self::DuplicateId => formatter.write_str("asset id is already registered"),
            Self::RegistryKeyMismatch => {
                formatter.write_str("asset registry key does not match asset id")
            }
            Self::EmptyPath => formatter.write_str("asset path must not be empty"),
            Self::EmptyDerivedPath => formatter.write_str("derived asset path must not be empty"),
            Self::KindMismatch => formatter.write_str("asset kind does not match its metadata"),
            Self::InvalidDimensions => formatter.write_str("asset dimensions must be positive"),
            Self::NonPositiveDuration => formatter.write_str("asset duration must be positive"),
            Self::InvalidTimeBase => formatter.write_str("asset time base must be positive"),
            Self::InvalidPixelAspect => formatter.write_str("pixel aspect ratio must be positive"),
            Self::InvalidAudioLayout => {
                formatter.write_str("audio sample rate and channels must be positive")
            }
            Self::FramesNotSorted => {
                formatter.write_str("source frame timestamps must be strictly increasing")
            }
            Self::ZeroFrameDuration => {
                formatter.write_str("source frame duration must be positive")
            }
            Self::Time(error) => error.fmt(formatter),
        }
    }
}

impl Error for AssetError {}

impl From<RationalError> for AssetError {
    fn from(error: RationalError) -> Self {
        Self::Time(error)
    }
}

impl AsRef<Path> for Asset {
    fn as_ref(&self) -> &Path {
        &self.path
    }
}

//! GPUI-independent layer rows and command builders for the layered editor.
//!
//! The project model intentionally keeps mutation in [`EditCommand`].  This
//! module only validates inspector input and constructs commands, making it
//! safe to use from a GPUI view, a headless test, or a future alternate UI.

use slicer::project::{
    Clip, ClipError, ClipId, ClipKind, Color, CropRect, EditCommand, Project, TextAlignment,
    TextStyle, TrackId, Transform,
};
use std::error::Error;
use std::fmt;

#[derive(Clone, Debug, PartialEq)]
pub enum InspectorError {
    InvalidGeometry(String),
    InvalidText(String),
    Transform(ClipError),
    WrongClipKind(ClipId),
    MissingClip(ClipId),
    MissingTrack(TrackId),
    MissingSourceDimensions(ClipId),
}

impl fmt::Display for InspectorError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidGeometry(error) => formatter.write_str(error),
            Self::InvalidText(error) => formatter.write_str(error),
            Self::Transform(error) => write!(formatter, "invalid transform geometry: {error}"),
            Self::WrongClipKind(id) => write!(formatter, "clip {id} is not a text clip"),
            Self::MissingClip(id) => write!(formatter, "clip {id} was not found"),
            Self::MissingTrack(id) => write!(formatter, "track {id} was not found"),
            Self::MissingSourceDimensions(id) => {
                write!(
                    formatter,
                    "crop command for clip {id} requires source dimensions"
                )
            }
        }
    }
}

impl Error for InspectorError {}

impl From<ClipError> for InspectorError {
    fn from(error: ClipError) -> Self {
        Self::Transform(error)
    }
}

pub type LayerInspectorError = InspectorError;

fn valid_clip_id(clip_id: ClipId) -> Result<(), InspectorError> {
    if clip_id.is_zero() {
        return Err(InspectorError::InvalidGeometry(
            "clip id must be non-zero".to_owned(),
        ));
    }
    Ok(())
}

fn valid_track_id(track_id: TrackId) -> Result<(), InspectorError> {
    if track_id.is_zero() {
        return Err(InspectorError::InvalidGeometry(
            "track id must be non-zero".to_owned(),
        ));
    }
    Ok(())
}

/// Build a validated transform edit.
pub fn transform_command(
    clip_id: ClipId,
    transform: Transform,
) -> Result<EditCommand, InspectorError> {
    valid_clip_id(clip_id)?;
    transform.validate()?;
    Ok(EditCommand::SetClipTransform { clip_id, transform })
}

pub fn set_transform_command(
    clip_id: ClipId,
    transform: Transform,
) -> Result<EditCommand, InspectorError> {
    transform_command(clip_id, transform)
}

/// Build a crop edit while validating the crop against the source dimensions.
/// Passing `None` clears the crop and does not require source dimensions.
pub fn crop_command(
    clip_id: ClipId,
    mut transform: Transform,
    crop: Option<CropRect>,
    source_dimensions: Option<(u32, u32)>,
) -> Result<EditCommand, InspectorError> {
    valid_clip_id(clip_id)?;
    if let Some(crop) = crop {
        crop.validate()?;
        let (width, height) =
            source_dimensions.ok_or(InspectorError::MissingSourceDimensions(clip_id))?;
        if width == 0 || height == 0 || !crop.fits_within(width, height)? {
            return Err(InspectorError::InvalidGeometry(
                "crop rectangle is outside source dimensions".to_owned(),
            ));
        }
    }
    transform.crop = crop;
    transform.validate()?;
    Ok(EditCommand::SetClipTransform { clip_id, transform })
}

pub fn set_crop_command(
    clip_id: ClipId,
    transform: Transform,
    crop: Option<CropRect>,
    source_dimensions: Option<(u32, u32)>,
) -> Result<EditCommand, InspectorError> {
    crop_command(clip_id, transform, crop, source_dimensions)
}

pub fn crop_command_with_source(
    clip_id: ClipId,
    transform: Transform,
    crop: Option<CropRect>,
    source_width: u32,
    source_height: u32,
) -> Result<EditCommand, InspectorError> {
    crop_command(
        clip_id,
        transform,
        crop,
        Some((source_width, source_height)),
    )
}

pub fn clear_crop_command(
    clip_id: ClipId,
    transform: Transform,
) -> Result<EditCommand, InspectorError> {
    crop_command(clip_id, transform, None, None)
}

pub fn z_order_command(clip_id: ClipId, order: i64) -> Result<EditCommand, InspectorError> {
    valid_clip_id(clip_id)?;
    Ok(EditCommand::SetClipOrder { clip_id, order })
}

pub fn set_z_order_command(clip_id: ClipId, order: i64) -> Result<EditCommand, InspectorError> {
    z_order_command(clip_id, order)
}

pub fn track_z_order_command(track_id: TrackId, order: i64) -> Result<EditCommand, InspectorError> {
    valid_track_id(track_id)?;
    Ok(EditCommand::SetTrackOrder { track_id, order })
}

pub fn set_track_z_order_command(
    track_id: TrackId,
    order: i64,
) -> Result<EditCommand, InspectorError> {
    track_z_order_command(track_id, order)
}

pub fn visibility_command(clip_id: ClipId, visible: bool) -> Result<EditCommand, InspectorError> {
    valid_clip_id(clip_id)?;
    Ok(EditCommand::SetClipVisibility { clip_id, visible })
}

pub fn set_visibility_command(
    clip_id: ClipId,
    visible: bool,
) -> Result<EditCommand, InspectorError> {
    visibility_command(clip_id, visible)
}

pub fn track_visibility_command(
    track_id: TrackId,
    visible: bool,
) -> Result<EditCommand, InspectorError> {
    valid_track_id(track_id)?;
    Ok(EditCommand::SetTrackVisibility { track_id, visible })
}

pub fn lock_command(track_id: TrackId, locked: bool) -> Result<EditCommand, InspectorError> {
    valid_track_id(track_id)?;
    Ok(EditCommand::SetTrackLocked { track_id, locked })
}

pub fn set_lock_command(track_id: TrackId, locked: bool) -> Result<EditCommand, InspectorError> {
    lock_command(track_id, locked)
}

pub fn set_track_locked_command(
    track_id: TrackId,
    locked: bool,
) -> Result<EditCommand, InspectorError> {
    lock_command(track_id, locked)
}

/// Text values are optional so a single inspector transaction can update any
/// subset of the serialized text properties.  `wrapping_width: Some(None)`
/// explicitly clears wrapping; `None` leaves it unchanged.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TextProperties {
    pub text: Option<String>,
    pub font_family: Option<String>,
    pub font_size: Option<f64>,
    pub alignment: Option<TextAlignment>,
    pub wrapping_width: Option<Option<f64>>,
    pub line_height: Option<f64>,
    pub color: Option<Color>,
}

impl TextProperties {
    pub fn with_text(text: impl Into<String>) -> Self {
        Self {
            text: Some(text.into()),
            ..Self::default()
        }
    }

    pub fn with_font_family(font_family: impl Into<String>) -> Self {
        Self {
            font_family: Some(font_family.into()),
            ..Self::default()
        }
    }

    pub fn with_font_size(font_size: f64) -> Self {
        Self {
            font_size: Some(font_size),
            ..Self::default()
        }
    }

    pub fn with_alignment(alignment: TextAlignment) -> Self {
        Self {
            alignment: Some(alignment),
            ..Self::default()
        }
    }

    pub fn with_wrapping_width(wrapping_width: Option<f64>) -> Self {
        Self {
            wrapping_width: Some(wrapping_width),
            ..Self::default()
        }
    }

    pub fn with_line_height(line_height: f64) -> Self {
        Self {
            line_height: Some(line_height),
            ..Self::default()
        }
    }

    pub fn with_color(color: Color) -> Self {
        Self {
            color: Some(color),
            ..Self::default()
        }
    }

    pub fn is_empty(&self) -> bool {
        self.text.is_none()
            && self.font_family.is_none()
            && self.font_size.is_none()
            && self.alignment.is_none()
            && self.wrapping_width.is_none()
            && self.line_height.is_none()
            && self.color.is_none()
    }
}

/// Replace text properties using only the existing project command API.  A
/// `DeleteClip` followed by an `AddClip` in one batch preserves the clip ID,
/// range, order, visibility, transform, and kind while making the complete
/// text edit one undoable command.
pub fn text_properties_command(
    track_id: TrackId,
    clip: &Clip,
    properties: TextProperties,
) -> Result<EditCommand, InspectorError> {
    valid_track_id(track_id)?;
    valid_clip_id(clip.id)?;
    if properties.is_empty() {
        return Err(InspectorError::InvalidText(
            "text edit contains no property changes".to_owned(),
        ));
    }
    let ClipKind::Text(text_clip) = &clip.kind else {
        return Err(InspectorError::WrongClipKind(clip.id));
    };
    let mut updated_text = text_clip.clone();
    if let Some(text) = properties.text {
        updated_text.text = text;
    }
    if let Some(font_family) = properties.font_family {
        updated_text.style.font_family = font_family;
    }
    if let Some(font_size) = properties.font_size {
        updated_text.style.font_size = font_size;
    }
    if let Some(alignment) = properties.alignment {
        updated_text.style.alignment = alignment;
    }
    if let Some(wrapping_width) = properties.wrapping_width {
        updated_text.style.wrapping_width = wrapping_width;
    }
    if let Some(line_height) = properties.line_height {
        updated_text.style.line_height = line_height;
    }
    if let Some(color) = properties.color {
        updated_text.style.color = color;
    }
    updated_text.style.validate()?;
    let mut updated = clip.clone();
    updated.kind = ClipKind::Text(updated_text);
    updated.validate()?;
    Ok(EditCommand::Batch {
        commands: vec![
            EditCommand::DeleteClip { clip_id: clip.id },
            EditCommand::AddClip {
                track_id,
                clip: updated,
            },
        ],
    })
}

pub fn set_text_command(
    track_id: TrackId,
    clip: &Clip,
    text: impl Into<String>,
) -> Result<EditCommand, InspectorError> {
    text_properties_command(track_id, clip, TextProperties::with_text(text))
}

pub fn set_text_style_command(
    track_id: TrackId,
    clip: &Clip,
    style: TextStyle,
) -> Result<EditCommand, InspectorError> {
    style.validate()?;
    let properties = TextProperties {
        font_family: Some(style.font_family),
        font_size: Some(style.font_size),
        alignment: Some(style.alignment),
        wrapping_width: Some(style.wrapping_width),
        line_height: Some(style.line_height),
        color: Some(style.color),
        ..TextProperties::default()
    };
    text_properties_command(track_id, clip, properties)
}

pub fn text_properties_command_for_project(
    project: &Project,
    clip_id: ClipId,
    properties: TextProperties,
) -> Result<EditCommand, InspectorError> {
    let clip = project
        .clip(clip_id)
        .ok_or(InspectorError::MissingClip(clip_id))?;
    let track_id = project
        .clip_track(clip_id)
        .ok_or(InspectorError::MissingTrack(TrackId::new(0)))?;
    text_properties_command(track_id, clip, properties)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LayerKind {
    Video,
    Image,
    Text,
    Shape,
    Audio,
}

impl From<&ClipKind> for LayerKind {
    fn from(kind: &ClipKind) -> Self {
        match kind {
            ClipKind::Video(_) => Self::Video,
            ClipKind::Image(_) => Self::Image,
            ClipKind::Text(_) => Self::Text,
            ClipKind::Shape(_) => Self::Shape,
            ClipKind::Audio(_) => Self::Audio,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LayerRow {
    pub track_id: TrackId,
    pub clip_id: ClipId,
    pub track_name: String,
    pub track_order: i64,
    pub clip_order: i64,
    pub visible: bool,
    pub locked: bool,
    pub kind: LayerKind,
}

/// Return layer rows in top-to-bottom inspector order.  Track and clip order
/// are the same total-order inputs used by scene evaluation; IDs break ties.
pub fn layer_rows(project: &Project) -> Vec<LayerRow> {
    let mut rows = project
        .tracks
        .iter()
        .flat_map(|track| {
            track.clips.iter().map(move |clip| LayerRow {
                track_id: track.id,
                clip_id: clip.id,
                track_name: track.name.clone(),
                track_order: track.order,
                clip_order: clip.order,
                visible: track.visible && clip.visible,
                locked: track.locked,
                kind: LayerKind::from(&clip.kind),
            })
        })
        .collect::<Vec<_>>();
    rows.sort_by(|left, right| {
        (
            right.track_order,
            right.clip_order,
            right.track_id,
            right.clip_id,
        )
            .cmp(&(
                left.track_order,
                left.clip_order,
                left.track_id,
                left.clip_id,
            ))
    });
    rows
}

/// Stateless facade useful to UI code that wants a named inspector service.
#[derive(Clone, Copy, Debug, Default)]
pub struct LayerInspector;

impl LayerInspector {
    pub fn rows(project: &Project) -> Vec<LayerRow> {
        layer_rows(project)
    }

    pub fn commands() -> InspectorCommandBuilder {
        InspectorCommandBuilder
    }
}

/// Named facade for command construction.  The free functions remain handy
/// for small adapters and tests.
#[derive(Clone, Copy, Debug, Default)]
pub struct InspectorCommandBuilder;

impl InspectorCommandBuilder {
    pub fn transform(
        &self,
        clip_id: ClipId,
        transform: Transform,
    ) -> Result<EditCommand, InspectorError> {
        transform_command(clip_id, transform)
    }

    pub fn crop(
        &self,
        clip_id: ClipId,
        transform: Transform,
        crop: Option<CropRect>,
        source_dimensions: Option<(u32, u32)>,
    ) -> Result<EditCommand, InspectorError> {
        crop_command(clip_id, transform, crop, source_dimensions)
    }

    pub fn z_order(&self, clip_id: ClipId, order: i64) -> Result<EditCommand, InspectorError> {
        z_order_command(clip_id, order)
    }

    pub fn track_z_order(
        &self,
        track_id: TrackId,
        order: i64,
    ) -> Result<EditCommand, InspectorError> {
        track_z_order_command(track_id, order)
    }

    pub fn visibility(
        &self,
        clip_id: ClipId,
        visible: bool,
    ) -> Result<EditCommand, InspectorError> {
        visibility_command(clip_id, visible)
    }

    pub fn track_visibility(
        &self,
        track_id: TrackId,
        visible: bool,
    ) -> Result<EditCommand, InspectorError> {
        track_visibility_command(track_id, visible)
    }

    pub fn lock(&self, track_id: TrackId, locked: bool) -> Result<EditCommand, InspectorError> {
        lock_command(track_id, locked)
    }

    pub fn text(
        &self,
        track_id: TrackId,
        clip: &Clip,
        properties: TextProperties,
    ) -> Result<EditCommand, InspectorError> {
        text_properties_command(track_id, clip, properties)
    }
}

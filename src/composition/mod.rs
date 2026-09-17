//! CPU reference compositor shared by preview tests and offline export.
//!
//! The renderer is intentionally independent of GPUI and Vulkan.  It defines
//! the pixel semantics that a future GPU route must match: back-to-front
//! stacking, straight-alpha project colors converted to premultiplied
//! working values, source crop/orientation, affine transforms, and analytic
//! vector/text coverage.

pub mod color;
pub mod frame;
pub mod shapes;
pub mod text;
pub mod transforms;

use crate::project::{
    AssetId, AssetMetadata, Orientation, Point, Project, SceneError, SceneKind, SceneSnapshot,
    TextClip, Transform,
};
use color::PremultipliedRgba;
use frame::{FrameError, FrameLimits, RgbaFrame};
use std::collections::BTreeMap;
use std::error::Error;
use std::fmt;
use std::sync::Arc;

pub use frame::{DEFAULT_MAX_PIXELS, FrameLimits as OutputLimits};

/// A frame provider abstracts software decoding, imported stills, and future
/// GPU-backed media without making the project model depend on either.
pub trait MediaSource: Send + Sync {
    fn frame(
        &self,
        asset_id: AssetId,
        source_time: crate::project::Time,
    ) -> Result<Option<RgbaFrame>, CompositionError>;
}

/// Simple deterministic frame provider useful for tests and generated assets.
#[derive(Clone, Default)]
pub struct InMemoryMedia {
    frames: Arc<BTreeMap<AssetId, RgbaFrame>>,
}

impl InMemoryMedia {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&mut self, asset_id: AssetId, frame: RgbaFrame) {
        Arc::make_mut(&mut self.frames).insert(asset_id, frame);
    }

    pub fn with_frame(mut self, asset_id: AssetId, frame: RgbaFrame) -> Self {
        self.insert(asset_id, frame);
        self
    }
}

impl MediaSource for InMemoryMedia {
    fn frame(
        &self,
        asset_id: AssetId,
        _source_time: crate::project::Time,
    ) -> Result<Option<RgbaFrame>, CompositionError> {
        Ok(self.frames.get(&asset_id).cloned())
    }
}

/// Bounded reference-render settings.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RenderOptions {
    pub limits: FrameLimits,
}

#[derive(Debug)]
pub enum CompositionError {
    Project(crate::project::ProjectError),
    Scene(SceneError),
    Frame(FrameError),
    MissingMedia(AssetId),
    MediaDimensionsMismatch {
        asset_id: AssetId,
        expected: (u32, u32),
        actual: (u32, u32),
    },
    InvalidGeometry(String),
}

impl fmt::Display for CompositionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Project(error) => write!(formatter, "project error: {error}"),
            Self::Scene(error) => write!(formatter, "scene error: {error}"),
            Self::Frame(error) => write!(formatter, "frame error: {error}"),
            Self::MissingMedia(id) => write!(formatter, "media for asset {id} is unavailable"),
            Self::MediaDimensionsMismatch {
                asset_id,
                expected,
                actual,
            } => write!(
                formatter,
                "asset {asset_id} media is {}x{}, expected {}x{}",
                actual.0, actual.1, expected.0, expected.1
            ),
            Self::InvalidGeometry(error) => formatter.write_str(error),
        }
    }
}

impl Error for CompositionError {}

impl From<crate::project::ProjectError> for CompositionError {
    fn from(error: crate::project::ProjectError) -> Self {
        Self::Project(error)
    }
}

impl From<SceneError> for CompositionError {
    fn from(error: SceneError) -> Self {
        Self::Scene(error)
    }
}

impl From<FrameError> for CompositionError {
    fn from(error: FrameError) -> Self {
        Self::Frame(error)
    }
}

/// Evaluate and render a project at an exact rational time.
pub fn render<M: MediaSource>(
    project: &Project,
    time: crate::project::Time,
    media: &M,
) -> Result<RgbaFrame, CompositionError> {
    render_with_options(project, time, media, RenderOptions::default())
}

pub fn render_with_options<M: MediaSource>(
    project: &Project,
    time: crate::project::Time,
    media: &M,
    options: RenderOptions,
) -> Result<RgbaFrame, CompositionError> {
    let scene = project.evaluate_scene(time)?;
    render_scene(&scene, media, options)
}

/// Render an already evaluated immutable scene snapshot.
pub fn render_scene<M: MediaSource>(
    scene: &SceneSnapshot,
    media: &M,
    options: RenderOptions,
) -> Result<RgbaFrame, CompositionError> {
    // Resolve each source frame once per scene. Calling a provider once per
    // output pixel would make a 1080p frame perform millions of cache lookups
    // and clone the same decoded buffer repeatedly. The prepared list keeps
    // the media lifetime tied to this immutable scene render while the inner
    // raster loop only performs sampling and compositing.
    let prepared = scene
        .draw_items
        .iter()
        .map(|item| {
            let frame = match &item.kind {
                SceneKind::Video {
                    asset_id,
                    source_time,
                    ..
                } => media.frame(*asset_id, *source_time)?,
                SceneKind::Image { asset_id, .. } => {
                    media.frame(*asset_id, crate::project::Time::ZERO)?
                }
                SceneKind::Text(_) | SceneKind::Shape(_) => None,
            };
            Ok((item, frame))
        })
        .collect::<Result<Vec<_>, CompositionError>>()?;
    let mut output =
        RgbaFrame::with_limits(scene.canvas.width, scene.canvas.height, options.limits)?;
    let background = PremultipliedRgba::from_color(scene.background);
    for y in 0..scene.canvas.height {
        for x in 0..scene.canvas.width {
            let index = ((y as usize) * (scene.canvas.width as usize) + x as usize) * 4;
            let mut pixel = background;
            let canvas_point = Point::new(f64::from(x) + 0.5, f64::from(y) + 0.5);
            for (item, frame) in &prepared {
                composite_item(&mut pixel, item, canvas_point, frame.as_ref())?;
            }
            output.pixels_mut()[index..index + 4].copy_from_slice(&pixel.to_rgba8().as_array());
        }
    }
    Ok(output)
}

fn composite_item(
    destination: &mut PremultipliedRgba,
    item: &crate::project::DrawItem,
    canvas_point: Point,
    media_frame: Option<&RgbaFrame>,
) -> Result<(), CompositionError> {
    match &item.kind {
        SceneKind::Video {
            orientation,
            pixel_aspect,
            ..
        } => {
            if let Some(frame) = media_frame {
                let (width, height) = frame.dimensions();
                let local = inverse_visual_point(
                    item.transform,
                    width,
                    height,
                    *pixel_aspect,
                    *orientation,
                    canvas_point,
                )?;
                if let Some(sample) =
                    sample_oriented(frame, local, item.transform.crop, *orientation)
                {
                    *destination = sample.scale(item.transform.opacity).over(*destination);
                }
            }
        }
        SceneKind::Image {
            orientation,
            pixel_aspect,
            ..
        } => {
            if let Some(frame) = media_frame {
                let (width, height) = frame.dimensions();
                let local = inverse_visual_point(
                    item.transform,
                    width,
                    height,
                    *pixel_aspect,
                    *orientation,
                    canvas_point,
                )?;
                if let Some(sample) =
                    sample_oriented(frame, local, item.transform.crop, *orientation)
                {
                    *destination = sample.scale(item.transform.opacity).over(*destination);
                }
            }
        }
        SceneKind::Shape(shape) => {
            let local =
                inverse_extent_point(item.transform, shape.width, shape.height, canvas_point)?;
            if local.x >= 0.0 && local.y >= 0.0 && local.x < shape.width && local.y < shape.height {
                let coverage = shapes::coverage(shape, local);
                if coverage.fill > 0.0 {
                    *destination = PremultipliedRgba::from_color(shape.fill)
                        .scale(coverage.fill * item.transform.opacity)
                        .over(*destination);
                }
                if let Some(stroke) = shape.stroke
                    && coverage.stroke > 0.0
                {
                    *destination = PremultipliedRgba::from_color(stroke.color)
                        .scale(coverage.stroke * item.transform.opacity)
                        .over(*destination);
                }
            }
        }
        SceneKind::Text(text) => composite_text(destination, item.transform, text, canvas_point),
    }
    Ok(())
}

fn composite_text(
    destination: &mut PremultipliedRgba,
    transform: Transform,
    text: &TextClip,
    canvas_point: Point,
) {
    let layout = text::TextLayout::new(&text.text, &text.style);
    let Ok(local) = inverse_extent_point(transform, layout.width, layout.height, canvas_point)
    else {
        return;
    };
    if local.x < 0.0 || local.y < 0.0 || local.x >= layout.width || local.y >= layout.height {
        return;
    }
    for glyph in &layout.glyphs {
        if local.x < glyph.x
            || local.x >= glyph.x + glyph.width
            || local.y < glyph.y
            || local.y >= glyph.y + glyph.height
        {
            continue;
        }
        let coverage = layout.coverage(
            glyph.character,
            (local.x - glyph.x) / glyph.width,
            (local.y - glyph.y) / glyph.height,
        );
        if coverage > 0.0 {
            *destination = PremultipliedRgba::from_color(text.style.color)
                .scale(coverage * transform.opacity)
                .over(*destination);
        }
    }
}

fn inverse_visual_point(
    transform: Transform,
    source_width: u32,
    source_height: u32,
    pixel_aspect: crate::project::Rational,
    orientation: Orientation,
    canvas_point: Point,
) -> Result<Point, CompositionError> {
    let crop = transform.crop;
    let raw_width = crop.map_or(source_width, |crop| crop.width);
    let raw_height = crop.map_or(source_height, |crop| crop.height);
    let (oriented_width, oriented_height) = match orientation {
        Orientation::Normal | Orientation::Rotate180 => (raw_width, raw_height),
        Orientation::Rotate90 | Orientation::Rotate270 => (raw_height, raw_width),
    };
    let aspect = pixel_aspect.to_f64();
    let point = inverse_extent_point(
        transform,
        f64::from(oriented_width) * aspect,
        f64::from(oriented_height),
        canvas_point,
    )?;
    Ok(Point::new(point.x / aspect, point.y))
}

fn inverse_extent_point(
    transform: Transform,
    width: f64,
    height: f64,
    canvas_point: Point,
) -> Result<Point, CompositionError> {
    transform
        .validate()
        .map_err(|error| CompositionError::InvalidGeometry(error.to_string()))?;
    let translated = Point::new(
        canvas_point.x - transform.position.x,
        canvas_point.y - transform.position.y,
    );
    let angle = -transform.rotation_degrees.to_radians();
    let (sin, cos) = angle.sin_cos();
    let rotated = Point::new(
        translated.x * cos - translated.y * sin,
        translated.x * sin + translated.y * cos,
    );
    if transform.scale.x == 0.0 || transform.scale.y == 0.0 {
        return Err(CompositionError::InvalidGeometry(
            "zero transform scale".to_owned(),
        ));
    }
    Ok(Point::new(
        rotated.x / transform.scale.x + width * transform.anchor.x,
        rotated.y / transform.scale.y + height * transform.anchor.y,
    ))
}

fn sample_oriented(
    frame: &RgbaFrame,
    local: Point,
    crop: Option<crate::project::CropRect>,
    orientation: Orientation,
) -> Option<PremultipliedRgba> {
    if !local.x.is_finite() || !local.y.is_finite() || local.x < 0.0 || local.y < 0.0 {
        return None;
    }
    let (width, height) = match orientation {
        Orientation::Normal | Orientation::Rotate180 => (
            crop.map_or(frame.width(), |c| c.width),
            crop.map_or(frame.height(), |c| c.height),
        ),
        Orientation::Rotate90 | Orientation::Rotate270 => (
            crop.map_or(frame.height(), |c| c.height),
            crop.map_or(frame.width(), |c| c.width),
        ),
    };
    if local.x >= f64::from(width) || local.y >= f64::from(height) {
        return None;
    }
    // `local` is already evaluated at the output pixel centre.  Keeping that
    // centre through the orientation map and flooring only at the final
    // source lookup gives nearest-neighbour sampling without a half-pixel
    // shift.
    let u = local.x;
    let v = local.y;
    let (raw_x, raw_y) = match orientation {
        Orientation::Normal => (u, v),
        Orientation::Rotate90 => (v, f64::from(width) - u),
        Orientation::Rotate180 => (f64::from(width) - u, f64::from(height) - v),
        Orientation::Rotate270 => (f64::from(height) - v, u),
    };
    let crop = crop.unwrap_or(crate::project::CropRect {
        x: 0,
        y: 0,
        width: frame.width(),
        height: frame.height(),
    });
    let x = crop.x as f64 + raw_x.floor();
    let y = crop.y as f64 + raw_y.floor();
    if x < 0.0 || y < 0.0 || x >= f64::from(frame.width()) || y >= f64::from(frame.height()) {
        return None;
    }
    frame
        .pixel(x as u32, y as u32)
        .map(PremultipliedRgba::from_rgba8)
}

/// Return source dimensions and orientation-independent metadata for a media asset.
pub fn media_dimensions(project: &Project, asset_id: AssetId) -> Option<(u32, u32)> {
    let asset = project.asset(asset_id)?;
    match &asset.metadata {
        AssetMetadata::Video(metadata) => Some((metadata.width, metadata.height)),
        AssetMetadata::Image(metadata) => Some((metadata.width, metadata.height)),
        AssetMetadata::Audio(_) => None,
    }
}

pub fn text_layout(text: &str, style: &crate::project::TextStyle) -> text::TextLayout {
    text::TextLayout::new(text, style)
}

pub fn orientation_degrees(orientation: Orientation) -> u16 {
    orientation.degrees()
}

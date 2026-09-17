//! GPUI-independent geometry for the layered editor canvas.
//!
//! The editor stores positions in canonical canvas pixels.  This module keeps
//! all pointer/display conversion and affine interaction math in that same
//! coordinate system so a preview backend and an inspector cannot disagree
//! about a clip's position.  Display rectangles are expressed in physical
//! display coordinates; `display_scale` converts them to logical layout units
//! before the fitted canvas scale is applied.

use slicer::composition::transforms as composition_transforms;
use slicer::project::{Canvas, ClipError, ClipId, CropRect, Orientation, TextStyle, Transform};
use std::error::Error;
use std::fmt;

pub use slicer::project::Point;

/// A validated two-dimensional display/layout size.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Size {
    pub width: f64,
    pub height: f64,
}

impl Size {
    pub fn new(width: f64, height: f64) -> Result<Self, GeometryError> {
        if !width.is_finite() || !height.is_finite() || width <= 0.0 || height <= 0.0 {
            return Err(GeometryError::InvalidGeometry(
                "size must have finite, positive dimensions".to_owned(),
            ));
        }
        Ok(Self { width, height })
    }

    pub fn aspect_ratio(self) -> Result<f64, GeometryError> {
        if !self.height.is_finite() || self.height <= 0.0 {
            return Err(GeometryError::InvalidGeometry(
                "size height must be finite and positive".to_owned(),
            ));
        }
        let ratio = self.width / self.height;
        ratio.is_finite().then_some(ratio).ok_or_else(|| {
            GeometryError::InvalidGeometry("size aspect ratio is not finite".to_owned())
        })
    }
}

impl From<Canvas> for Size {
    fn from(canvas: Canvas) -> Self {
        Self {
            width: f64::from(canvas.width),
            height: f64::from(canvas.height),
        }
    }
}

/// A rectangle in physical display coordinates.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

impl Rect {
    pub fn new(x: f64, y: f64, width: f64, height: f64) -> Result<Self, GeometryError> {
        if !x.is_finite()
            || !y.is_finite()
            || !width.is_finite()
            || !height.is_finite()
            || width <= 0.0
            || height <= 0.0
        {
            return Err(GeometryError::InvalidGeometry(
                "rectangle must have finite origin and positive dimensions".to_owned(),
            ));
        }
        Ok(Self {
            x,
            y,
            width,
            height,
        })
    }

    pub fn right(self) -> f64 {
        self.x + self.width
    }

    pub fn bottom(self) -> f64 {
        self.y + self.height
    }

    pub fn center(self) -> Point {
        Point::new(self.x + self.width * 0.5, self.y + self.height * 0.5)
    }

    pub fn contains(self, point: Point) -> bool {
        point.x >= self.x
            && point.x <= self.right()
            && point.y >= self.y
            && point.y <= self.bottom()
    }
}

/// Errors returned by canvas conversion and interaction geometry.
#[derive(Clone, Debug, PartialEq)]
pub enum GeometryError {
    InvalidGeometry(String),
    Transform(ClipError),
    UnsupportedGeometry(String),
}

impl fmt::Display for GeometryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidGeometry(error) => formatter.write_str(error),
            Self::Transform(error) => write!(formatter, "invalid transform geometry: {error}"),
            Self::UnsupportedGeometry(error) => formatter.write_str(error),
        }
    }
}

impl Error for GeometryError {}

impl From<ClipError> for GeometryError {
    fn from(error: ClipError) -> Self {
        Self::Transform(error)
    }
}

/// Compatibility aliases for callers that prefer the module name in the
/// error type.
pub type CanvasToolError = GeometryError;
pub type CanvasToolsError = GeometryError;

fn validate_point(point: Point) -> Result<(), GeometryError> {
    if point.x.is_finite() && point.y.is_finite() {
        Ok(())
    } else {
        Err(GeometryError::InvalidGeometry(
            "point must have finite coordinates".to_owned(),
        ))
    }
}

/// Conversion between physical display coordinates and canonical canvas
/// pixels.  `canvas_scale` is the effective logical display-units per canvas
/// pixel.  Use [`Self::fit`] to derive it from a letterboxed viewport.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CanvasDisplayTransform {
    pub display_rect: Rect,
    pub canvas_size: Size,
    pub display_scale: f64,
    pub canvas_scale: f64,
}

impl CanvasDisplayTransform {
    /// Construct a mapping with an explicit effective canvas scale.
    pub fn new<C>(
        display_rect: Rect,
        canvas: C,
        display_scale: f64,
        canvas_scale: f64,
    ) -> Result<Self, GeometryError>
    where
        C: Into<Size>,
    {
        let canvas_size = canvas.into();
        display_rect_check(display_rect)?;
        canvas_size_check(canvas_size)?;
        validate_positive_scale(display_scale, "display scale")?;
        validate_positive_scale(canvas_scale, "canvas scale")?;
        Ok(Self {
            display_rect,
            canvas_size,
            display_scale,
            canvas_scale,
        })
    }

    /// Fit a canvas into the display rectangle, preserving aspect ratio.
    /// `zoom = 1` is the letterboxed fit; values above or below one zoom in or
    /// out around the display centre.
    pub fn fit<C>(
        display_rect: Rect,
        canvas: C,
        display_scale: f64,
        zoom: f64,
    ) -> Result<Self, GeometryError>
    where
        C: Into<Size>,
    {
        let canvas_size = canvas.into();
        display_rect_check(display_rect)?;
        canvas_size_check(canvas_size)?;
        validate_positive_scale(display_scale, "display scale")?;
        validate_positive_scale(zoom, "canvas zoom")?;
        let logical_width = display_rect.width / display_scale;
        let logical_height = display_rect.height / display_scale;
        let fit_scale =
            (logical_width / canvas_size.width).min(logical_height / canvas_size.height);
        if !fit_scale.is_finite() || fit_scale <= 0.0 {
            return Err(GeometryError::InvalidGeometry(
                "display rectangle cannot contain a canvas".to_owned(),
            ));
        }
        Self::new(display_rect, canvas_size, display_scale, fit_scale * zoom)
    }

    pub fn from_fit<C>(
        display_rect: Rect,
        canvas: C,
        display_scale: f64,
        zoom: f64,
    ) -> Result<Self, GeometryError>
    where
        C: Into<Size>,
    {
        Self::fit(display_rect, canvas, display_scale, zoom)
    }

    pub fn fit_scale(&self) -> Result<f64, GeometryError> {
        let logical_width = self.display_rect.width / self.display_scale;
        let logical_height = self.display_rect.height / self.display_scale;
        let scale =
            (logical_width / self.canvas_size.width).min(logical_height / self.canvas_size.height);
        scale
            .is_finite()
            .then_some(scale)
            .ok_or_else(|| GeometryError::InvalidGeometry("fit scale is not finite".to_owned()))
    }

    /// The physical rectangle occupied by the canvas after letterboxing and
    /// zooming.  At zoom 1 it is fully contained by `display_rect`.
    pub fn letterbox_rect(&self) -> Result<Rect, GeometryError> {
        let logical_width = self.canvas_size.width * self.canvas_scale;
        let logical_height = self.canvas_size.height * self.canvas_scale;
        Rect::new(
            self.display_rect.x
                + (self.display_rect.width - logical_width * self.display_scale) * 0.5,
            self.display_rect.y
                + (self.display_rect.height - logical_height * self.display_scale) * 0.5,
            logical_width * self.display_scale,
            logical_height * self.display_scale,
        )
    }

    pub fn canvas_rect(&self) -> Result<Rect, GeometryError> {
        self.letterbox_rect()
    }

    /// Convert a physical display point to canonical canvas pixels.  Points
    /// outside the letterboxed canvas are intentionally returned as negative
    /// or out-of-range canvas coordinates so callers can implement their own
    /// edge behavior.
    pub fn display_to_canvas(&self, display_point: Point) -> Result<Point, GeometryError> {
        validate_point(display_point)?;
        let layout_point = self.display_to_layout(display_point)?;
        let logical_width = self.canvas_size.width * self.canvas_scale;
        let logical_height = self.canvas_size.height * self.canvas_scale;
        let origin = Point::new(
            (self.display_rect.width / self.display_scale - logical_width) * 0.5,
            (self.display_rect.height / self.display_scale - logical_height) * 0.5,
        );
        Ok(Point::new(
            (layout_point.x - origin.x) / self.canvas_scale,
            (layout_point.y - origin.y) / self.canvas_scale,
        ))
    }

    pub fn display_to_canvas_checked(&self, display_point: Point) -> Result<Point, GeometryError> {
        let canvas_point = self.display_to_canvas(display_point)?;
        if canvas_point.x < 0.0
            || canvas_point.y < 0.0
            || canvas_point.x > self.canvas_size.width
            || canvas_point.y > self.canvas_size.height
        {
            return Err(GeometryError::InvalidGeometry(
                "display point is outside the letterboxed canvas".to_owned(),
            ));
        }
        Ok(canvas_point)
    }

    pub fn canvas_to_display(&self, canvas_point: Point) -> Result<Point, GeometryError> {
        validate_point(canvas_point)?;
        let layout_width = self.display_rect.width / self.display_scale;
        let layout_height = self.display_rect.height / self.display_scale;
        let content_width = self.canvas_size.width * self.canvas_scale;
        let content_height = self.canvas_size.height * self.canvas_scale;
        let origin = Point::new(
            (layout_width - content_width) * 0.5,
            (layout_height - content_height) * 0.5,
        );
        Ok(Point::new(
            self.display_rect.x
                + (origin.x + canvas_point.x * self.canvas_scale) * self.display_scale,
            self.display_rect.y
                + (origin.y + canvas_point.y * self.canvas_scale) * self.display_scale,
        ))
    }

    /// Convert a point relative to the display rectangle from layout units to
    /// physical display coordinates.
    pub fn layout_to_display(&self, layout_point: Point) -> Result<Point, GeometryError> {
        validate_point(layout_point)?;
        Ok(Point::new(
            self.display_rect.x + layout_point.x * self.display_scale,
            self.display_rect.y + layout_point.y * self.display_scale,
        ))
    }

    /// Convert a physical display point to layout units relative to the
    /// display rectangle.
    pub fn display_to_layout(&self, display_point: Point) -> Result<Point, GeometryError> {
        validate_point(display_point)?;
        Ok(Point::new(
            (display_point.x - self.display_rect.x) / self.display_scale,
            (display_point.y - self.display_rect.y) / self.display_scale,
        ))
    }

    pub fn is_on_canvas(&self, display_point: Point) -> Result<bool, GeometryError> {
        validate_point(display_point)?;
        Ok(self.letterbox_rect()?.contains(display_point))
    }
}

pub type CanvasDisplayMapping = CanvasDisplayTransform;
pub type CanvasViewport = CanvasDisplayTransform;
pub type CanvasView = CanvasDisplayTransform;

fn display_rect_check(rect: Rect) -> Result<(), GeometryError> {
    Rect::new(rect.x, rect.y, rect.width, rect.height).map(|_| ())
}

fn canvas_size_check(size: Size) -> Result<(), GeometryError> {
    Size::new(size.width, size.height).map(|_| ())
}

fn validate_positive_scale(value: f64, label: &str) -> Result<(), GeometryError> {
    if value.is_finite() && value > 0.0 {
        Ok(())
    } else {
        Err(GeometryError::InvalidGeometry(format!(
            "{label} must be finite and positive"
        )))
    }
}

/// Source metadata needed to make a media clip's visual extent agree with
/// the compositor, including pixel aspect and presentation orientation.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SourceGeometry {
    pub width: u32,
    pub height: u32,
    pub pixel_aspect: f64,
    pub orientation: Orientation,
}

impl SourceGeometry {
    pub fn new(width: u32, height: u32, pixel_aspect: f64) -> Result<Self, GeometryError> {
        Self::with_orientation(width, height, pixel_aspect, Orientation::Normal)
    }

    pub fn with_orientation(
        width: u32,
        height: u32,
        pixel_aspect: f64,
        orientation: Orientation,
    ) -> Result<Self, GeometryError> {
        if width == 0 || height == 0 {
            return Err(GeometryError::InvalidGeometry(
                "source dimensions must be positive".to_owned(),
            ));
        }
        validate_positive_scale(pixel_aspect, "pixel aspect")?;
        Ok(Self {
            width,
            height,
            pixel_aspect,
            orientation,
        })
    }
}

/// The visual content rectangle used for hit testing and handles.  Media
/// geometry stores source dimensions and uses `Transform::crop`; text and
/// shapes store their already-laid-out local dimensions.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct VisualGeometry {
    pub width: f64,
    pub height: f64,
    pub pixel_aspect: f64,
    pub orientation: Orientation,
    pub source_dimensions: Option<(u32, u32)>,
}

pub type ClipGeometry = VisualGeometry;

impl VisualGeometry {
    pub fn media(source: SourceGeometry) -> Self {
        Self {
            width: f64::from(source.width),
            height: f64::from(source.height),
            pixel_aspect: source.pixel_aspect,
            orientation: source.orientation,
            source_dimensions: Some((source.width, source.height)),
        }
    }

    pub fn new(width: f64, height: f64) -> Result<Self, GeometryError> {
        Self::content(width, height, 1.0)
    }

    pub fn content(width: f64, height: f64, pixel_aspect: f64) -> Result<Self, GeometryError> {
        if !width.is_finite() || !height.is_finite() || width <= 0.0 || height <= 0.0 {
            return Err(GeometryError::InvalidGeometry(
                "content dimensions must be finite and positive".to_owned(),
            ));
        }
        validate_positive_scale(pixel_aspect, "pixel aspect")?;
        Ok(Self {
            width,
            height,
            pixel_aspect,
            orientation: Orientation::Normal,
            source_dimensions: None,
        })
    }

    pub fn shape(width: f64, height: f64) -> Result<Self, GeometryError> {
        Self::content(width, height, 1.0)
    }

    /// Resolve text dimensions with the same deterministic layout used by the
    /// reference compositor.  In particular, wrapping and line-height are
    /// not reimplemented in the UI layer.
    pub fn text(text: &str, style: &TextStyle) -> Result<Self, GeometryError> {
        style.validate()?;
        let layout = slicer::composition::text_layout(text, style);
        Self::content(layout.width, layout.height, 1.0)
    }

    pub fn from_source(source: SourceGeometry) -> Self {
        Self::media(source)
    }

    pub fn source_geometry(self) -> Option<SourceGeometry> {
        self.source_dimensions
            .map(|(width, height)| SourceGeometry {
                width,
                height,
                pixel_aspect: self.pixel_aspect,
                orientation: self.orientation,
            })
    }

    pub fn raw_extent(self, transform: Transform) -> Result<Size, GeometryError> {
        transform.validate()?;
        if let Some((width, height)) = self.source_dimensions {
            let extent = transform.local_extent(width, height)?;
            return Size::new(extent.x, extent.y);
        }
        if transform.crop.is_some() {
            return Err(GeometryError::UnsupportedGeometry(
                "text and shape geometry cannot have a source crop".to_owned(),
            ));
        }
        if self.orientation != Orientation::Normal {
            return Err(GeometryError::UnsupportedGeometry(
                "non-media geometry cannot have source orientation".to_owned(),
            ));
        }
        Size::new(self.width, self.height)
    }

    pub fn oriented_raw_extent(self, transform: Transform) -> Result<Size, GeometryError> {
        let raw = self.raw_extent(transform)?;
        match self.orientation {
            Orientation::Normal | Orientation::Rotate180 => Ok(raw),
            Orientation::Rotate90 | Orientation::Rotate270 => Size::new(raw.height, raw.width),
        }
    }

    /// Visual size before the clip transform's scale, in canvas pixels.
    pub fn content_extent(self, transform: Transform) -> Result<Size, GeometryError> {
        let raw = self.oriented_raw_extent(transform)?;
        if self.source_dimensions.is_some() {
            let mut helper_transform = transform;
            helper_transform.crop = None;
            let oriented_width = raw.width.round() as u32;
            let oriented_height = raw.height.round() as u32;
            let extent = composition_transforms::content_extent(
                helper_transform,
                oriented_width,
                oriented_height,
                self.pixel_aspect,
            )?;
            // The source dimensions are used above to keep the helper on its
            // validated media path.  `raw` remains authoritative for crops.
            Size::new(extent.x, extent.y)
        } else {
            Size::new(raw.width * self.pixel_aspect, raw.height)
        }
    }

    /// Invert the compositor's affine transform into oriented, cropped local
    /// source coordinates.  This is the same operation order as composition:
    /// crop, anchor, scale, rotation, then position.
    pub fn inverse_point(
        self,
        transform: Transform,
        canvas_point: Point,
    ) -> Result<Point, GeometryError> {
        validate_point(canvas_point)?;
        let raw = self.oriented_raw_extent(transform)?;
        if self.source_dimensions.is_some() {
            let mut helper_transform = transform;
            helper_transform.crop = None;
            let width = u32_from_extent(raw.width, "oriented source width")?;
            let height = u32_from_extent(raw.height, "oriented source height")?;
            return composition_transforms::inverse_point(
                helper_transform,
                width,
                height,
                self.pixel_aspect,
                canvas_point,
            )
            .map_err(GeometryError::from);
        }
        inverse_extent_point(
            transform,
            raw.width * self.pixel_aspect,
            raw.height,
            canvas_point,
        )
    }

    pub fn forward_point(
        self,
        transform: Transform,
        local_point: Point,
    ) -> Result<Point, GeometryError> {
        validate_point(local_point)?;
        let raw = self.oriented_raw_extent(transform)?;
        if self.source_dimensions.is_some() {
            let mut helper_transform = transform;
            helper_transform.crop = None;
            let width = u32_from_extent(raw.width, "oriented source width")?;
            let height = u32_from_extent(raw.height, "oriented source height")?;
            return composition_transforms::local_to_source(
                helper_transform,
                width,
                height,
                self.pixel_aspect,
                local_point,
            )
            .map_err(GeometryError::from);
        }
        forward_extent_point(
            transform,
            raw.width * self.pixel_aspect,
            raw.height,
            local_point,
        )
    }

    pub fn contains_canvas_point(
        self,
        transform: Transform,
        canvas_point: Point,
    ) -> Result<bool, GeometryError> {
        let local = self.inverse_point(transform, canvas_point)?;
        let extent = self.oriented_raw_extent(transform)?;
        Ok(local.x >= 0.0 && local.y >= 0.0 && local.x < extent.width && local.y < extent.height)
    }

    pub fn corners_canvas(self, transform: Transform) -> Result<[Point; 4], GeometryError> {
        let extent = self.oriented_raw_extent(transform)?;
        Ok([
            self.forward_point(transform, Point::new(0.0, 0.0))?,
            self.forward_point(transform, Point::new(extent.width, 0.0))?,
            self.forward_point(transform, Point::new(extent.width, extent.height))?,
            self.forward_point(transform, Point::new(0.0, extent.height))?,
        ])
    }

    /// Convert a canvas point into full-source coordinates.  This is useful
    /// for crop drags; the returned point includes the current crop origin.
    pub fn inverse_source_point(
        self,
        transform: Transform,
        canvas_point: Point,
    ) -> Result<Point, GeometryError> {
        let Some((source_width, source_height)) = self.source_dimensions else {
            return Err(GeometryError::UnsupportedGeometry(
                "source coordinates require media geometry".to_owned(),
            ));
        };
        let crop = current_crop(transform, source_width, source_height)?;
        let local = self.inverse_point(transform, canvas_point)?;
        let raw = oriented_to_raw(
            local,
            f64::from(crop.width),
            f64::from(crop.height),
            self.orientation,
        );
        Ok(Point::new(
            f64::from(crop.x) + raw.x,
            f64::from(crop.y) + raw.y,
        ))
    }
}

fn u32_from_extent(value: f64, label: &str) -> Result<u32, GeometryError> {
    if !value.is_finite() || value <= 0.0 || value > f64::from(u32::MAX) || value.fract() != 0.0 {
        return Err(GeometryError::InvalidGeometry(format!(
            "{label} must be a positive whole-number source extent"
        )));
    }
    Ok(value as u32)
}

fn inverse_extent_point(
    transform: Transform,
    width: f64,
    height: f64,
    canvas_point: Point,
) -> Result<Point, GeometryError> {
    transform.validate()?;
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
        return Err(GeometryError::InvalidGeometry(
            "zero transform scale cannot be inverted".to_owned(),
        ));
    }
    Ok(Point::new(
        rotated.x / transform.scale.x + width * transform.anchor.x,
        rotated.y / transform.scale.y + height * transform.anchor.y,
    ))
}

fn forward_extent_point(
    transform: Transform,
    width: f64,
    height: f64,
    local_point: Point,
) -> Result<Point, GeometryError> {
    transform.validate()?;
    let centered = Point::new(
        (local_point.x - width * transform.anchor.x) * transform.scale.x,
        (local_point.y - height * transform.anchor.y) * transform.scale.y,
    );
    let angle = transform.rotation_degrees.to_radians();
    let (sin, cos) = angle.sin_cos();
    Ok(Point::new(
        transform.position.x + centered.x * cos - centered.y * sin,
        transform.position.y + centered.x * sin + centered.y * cos,
    ))
}

/// Hit-test a normal-orientation media clip using the shared composition
/// transform helper.
pub fn affine_hit_test(
    transform: Transform,
    source_width: u32,
    source_height: u32,
    pixel_aspect: f64,
    canvas_point: Point,
) -> Result<bool, GeometryError> {
    VisualGeometry::media(SourceGeometry::new(
        source_width,
        source_height,
        pixel_aspect,
    )?)
    .contains_canvas_point(transform, canvas_point)
}

pub fn hit_test(
    transform: Transform,
    source_width: u32,
    source_height: u32,
    pixel_aspect: f64,
    canvas_point: Point,
) -> Result<bool, GeometryError> {
    affine_hit_test(
        transform,
        source_width,
        source_height,
        pixel_aspect,
        canvas_point,
    )
}

pub fn affine_hit_test_oriented(
    transform: Transform,
    source: SourceGeometry,
    canvas_point: Point,
) -> Result<bool, GeometryError> {
    VisualGeometry::media(source).contains_canvas_point(transform, canvas_point)
}

/// One candidate layer for back-to-front affine hit testing.
#[derive(Clone, Debug, PartialEq)]
pub struct HitTestLayer {
    pub clip_id: ClipId,
    pub transform: Transform,
    pub geometry: VisualGeometry,
    pub track_order: i64,
    pub clip_order: i64,
    pub visible: bool,
    pub locked: bool,
}

pub fn hit_test_layers(
    layers: &[HitTestLayer],
    canvas_point: Point,
) -> Result<Option<HitTestLayer>, GeometryError> {
    let mut hit = None;
    for layer in layers {
        if !layer.visible
            || !layer
                .geometry
                .contains_canvas_point(layer.transform, canvas_point)?
        {
            continue;
        }
        if hit.as_ref().is_none_or(|current: &HitTestLayer| {
            (layer.track_order, layer.clip_order, layer.clip_id)
                > (current.track_order, current.clip_order, current.clip_id)
        }) {
            hit = Some(layer.clone());
        }
    }
    Ok(hit)
}

/// Handles are ordered clockwise from the top-left corner, followed by the
/// rotation handle.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum HandleKind {
    TopLeft,
    Top,
    TopRight,
    Right,
    BottomRight,
    Bottom,
    BottomLeft,
    Left,
    Rotate,
}

impl HandleKind {
    #[allow(non_upper_case_globals)]
    pub const Rotation: Self = Self::Rotate;

    fn is_left(self) -> bool {
        matches!(self, Self::TopLeft | Self::Left | Self::BottomLeft)
    }

    fn is_right(self) -> bool {
        matches!(self, Self::TopRight | Self::Right | Self::BottomRight)
    }

    fn is_top(self) -> bool {
        matches!(self, Self::TopLeft | Self::Top | Self::TopRight)
    }

    fn is_bottom(self) -> bool {
        matches!(self, Self::BottomLeft | Self::Bottom | Self::BottomRight)
    }

    fn is_resize(self) -> bool {
        !matches!(self, Self::Rotate)
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SelectionHandle {
    pub kind: HandleKind,
    pub center: Point,
    pub radius: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SelectionGeometry {
    /// Clockwise transformed corners: top-left, top-right, bottom-right,
    /// bottom-left in local content coordinates.
    pub corners: [Point; 4],
    /// Edge midpoints in top, right, bottom, left order.
    pub edges: [Point; 4],
    pub center: Point,
    pub rotation_handle: Point,
    pub bounds: Rect,
    pub handles: Vec<SelectionHandle>,
}

impl SelectionGeometry {
    pub const DEFAULT_HANDLE_RADIUS: f64 = 6.0;
    pub const DEFAULT_ROTATION_OFFSET: f64 = 28.0;

    pub fn new(
        transform: Transform,
        geometry: VisualGeometry,
        handle_radius: f64,
        rotation_offset: f64,
    ) -> Result<Self, GeometryError> {
        if !handle_radius.is_finite() || handle_radius <= 0.0 {
            return Err(GeometryError::InvalidGeometry(
                "selection handle radius must be finite and positive".to_owned(),
            ));
        }
        if !rotation_offset.is_finite() || rotation_offset <= 0.0 {
            return Err(GeometryError::InvalidGeometry(
                "rotation handle offset must be finite and positive".to_owned(),
            ));
        }
        let corners = geometry.corners_canvas(transform)?;
        let edges = [
            midpoint(corners[0], corners[1]),
            midpoint(corners[1], corners[2]),
            midpoint(corners[2], corners[3]),
            midpoint(corners[3], corners[0]),
        ];
        let center = midpoint(edges[0], edges[2]);
        let outward = normalized_difference(edges[0], center)?;
        let rotation_handle = Point::new(
            edges[0].x + outward.x * rotation_offset,
            edges[0].y + outward.y * rotation_offset,
        );
        let bounds = axis_aligned_bounds(corners)?;
        let handle_centers = [
            (HandleKind::TopLeft, corners[0]),
            (HandleKind::Top, edges[0]),
            (HandleKind::TopRight, corners[1]),
            (HandleKind::Right, edges[1]),
            (HandleKind::BottomRight, corners[2]),
            (HandleKind::Bottom, edges[2]),
            (HandleKind::BottomLeft, corners[3]),
            (HandleKind::Left, edges[3]),
            (HandleKind::Rotate, rotation_handle),
        ];
        let handles = handle_centers
            .into_iter()
            .map(|(kind, center)| SelectionHandle {
                kind,
                center,
                radius: handle_radius,
            })
            .collect();
        Ok(Self {
            corners,
            edges,
            center,
            rotation_handle,
            bounds,
            handles,
        })
    }

    pub fn from_transform(
        transform: Transform,
        geometry: VisualGeometry,
    ) -> Result<Self, GeometryError> {
        Self::new(
            transform,
            geometry,
            Self::DEFAULT_HANDLE_RADIUS,
            Self::DEFAULT_ROTATION_OFFSET,
        )
    }

    pub fn handle(&self, kind: HandleKind) -> Option<SelectionHandle> {
        self.handles
            .iter()
            .copied()
            .find(|handle| handle.kind == kind)
    }

    pub fn hit_handle(
        &self,
        point: Point,
        tolerance: f64,
    ) -> Result<Option<HandleKind>, GeometryError> {
        validate_point(point)?;
        if !tolerance.is_finite() || tolerance < 0.0 {
            return Err(GeometryError::InvalidGeometry(
                "handle tolerance must be finite and non-negative".to_owned(),
            ));
        }
        let mut best = None;
        let mut best_distance = f64::INFINITY;
        for handle in &self.handles {
            let distance = squared_distance(point, handle.center);
            let radius = handle.radius + tolerance;
            if distance <= radius * radius && distance < best_distance {
                best = Some(handle.kind);
                best_distance = distance;
            }
        }
        Ok(best)
    }

    pub fn contains(&self, point: Point) -> Result<bool, GeometryError> {
        validate_point(point)?;
        let mut signs = [0.0; 4];
        for index in 0..4 {
            let next = (index + 1) % 4;
            signs[index] = cross(
                subtract(self.corners[next], self.corners[index]),
                subtract(point, self.corners[index]),
            );
        }
        let positive = signs.iter().any(|value| *value > 1e-9);
        let negative = signs.iter().any(|value| *value < -1e-9);
        Ok(!(positive && negative))
    }
}

pub fn selection_geometry(
    transform: Transform,
    geometry: VisualGeometry,
    handle_radius: f64,
    rotation_offset: f64,
) -> Result<SelectionGeometry, GeometryError> {
    SelectionGeometry::new(transform, geometry, handle_radius, rotation_offset)
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DragGeometry {
    pub start: Point,
    pub current: Point,
    pub delta: Point,
}

impl DragGeometry {
    pub fn new(start: Point, current: Point) -> Result<Self, GeometryError> {
        validate_point(start)?;
        validate_point(current)?;
        Ok(Self {
            start,
            current,
            delta: subtract(current, start),
        })
    }
}

pub fn translate_transform(transform: Transform, delta: Point) -> Result<Transform, GeometryError> {
    transform.validate()?;
    validate_point(delta)?;
    let mut translated = transform;
    translated.position = Point::new(
        transform.position.x + delta.x,
        transform.position.y + delta.y,
    );
    translated.validate()?;
    Ok(translated)
}

pub fn drag_transform(
    transform: Transform,
    start: Point,
    current: Point,
) -> Result<Transform, GeometryError> {
    translate_transform(transform, DragGeometry::new(start, current)?.delta)
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ResizeOptions {
    pub lock_aspect: bool,
    /// Minimum local source/content span for an actively resized axis.
    pub min_size: f64,
}

impl Default for ResizeOptions {
    fn default() -> Self {
        Self {
            lock_aspect: false,
            min_size: 1e-6,
        }
    }
}

pub fn resize_transform(
    transform: Transform,
    geometry: VisualGeometry,
    handle: HandleKind,
    pointer: Point,
    options: ResizeOptions,
) -> Result<Transform, GeometryError> {
    if !handle.is_resize() {
        return Err(GeometryError::InvalidGeometry(
            "rotation handle is not a resize handle".to_owned(),
        ));
    }
    if !options.min_size.is_finite() || options.min_size <= 0.0 {
        return Err(GeometryError::InvalidGeometry(
            "resize minimum must be finite and positive".to_owned(),
        ));
    }
    let extent = geometry.oriented_raw_extent(transform)?;
    if extent.width <= options.min_size || extent.height <= options.min_size {
        return Err(GeometryError::InvalidGeometry(
            "clip content is smaller than the resize minimum".to_owned(),
        ));
    }
    let local = geometry.inverse_point(transform, pointer)?;
    let horizontal = handle.is_left() || handle.is_right();
    let vertical = handle.is_top() || handle.is_bottom();
    let mut span_x = if handle.is_left() {
        extent.width - local.x
    } else if handle.is_right() {
        local.x
    } else {
        extent.width
    };
    let mut span_y = if handle.is_top() {
        extent.height - local.y
    } else if handle.is_bottom() {
        local.y
    } else {
        extent.height
    };
    if horizontal && span_x <= options.min_size || vertical && span_y <= options.min_size {
        return Err(GeometryError::InvalidGeometry(
            "resize would produce a zero or negative content span".to_owned(),
        ));
    }

    let mut anchor_x = if handle.is_left() {
        1.0
    } else if handle.is_right() {
        0.0
    } else {
        transform.anchor.x
    };
    let mut anchor_y = if handle.is_top() {
        1.0
    } else if handle.is_bottom() {
        0.0
    } else {
        transform.anchor.y
    };
    let mut fixed_x = if handle.is_left() {
        extent.width
    } else if handle.is_right() {
        0.0
    } else {
        extent.width * transform.anchor.x
    };
    let mut fixed_y = if handle.is_top() {
        extent.height
    } else if handle.is_bottom() {
        0.0
    } else {
        extent.height * transform.anchor.y
    };

    if options.lock_aspect {
        if horizontal && vertical {
            let factor = (span_x / extent.width).max(span_y / extent.height);
            span_x = extent.width * factor;
            span_y = extent.height * factor;
        } else if horizontal {
            let factor = span_x / extent.width;
            span_y = extent.height * factor;
            anchor_y = 0.5;
            fixed_y = extent.height * 0.5;
        } else if vertical {
            let factor = span_y / extent.height;
            span_x = extent.width * factor;
            anchor_x = 0.5;
            fixed_x = extent.width * 0.5;
        }
    }
    if span_x <= options.min_size || span_y <= options.min_size {
        return Err(GeometryError::InvalidGeometry(
            "aspect-locked resize would produce a zero or negative span".to_owned(),
        ));
    }

    let fixed_canvas = geometry.forward_point(transform, Point::new(fixed_x, fixed_y))?;
    let mut resized = transform;
    resized.anchor = Point::new(anchor_x, anchor_y);
    resized.scale = Point::new(
        transform.scale.x * span_x / extent.width,
        transform.scale.y * span_y / extent.height,
    );
    resized.position = fixed_canvas;
    resized.validate()?;
    Ok(resized)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AspectAxis {
    Width,
    Height,
}

pub fn aspect_locked_size(
    size: Size,
    aspect_ratio: f64,
    driven_axis: AspectAxis,
) -> Result<Size, GeometryError> {
    let _ = size.aspect_ratio()?;
    validate_positive_scale(aspect_ratio, "aspect ratio")?;
    match driven_axis {
        AspectAxis::Width => Size::new(size.width, size.width / aspect_ratio),
        AspectAxis::Height => Size::new(size.height * aspect_ratio, size.height),
    }
}

pub fn lock_aspect(size: Size, aspect_ratio: f64) -> Result<Size, GeometryError> {
    aspect_locked_size(size, aspect_ratio, AspectAxis::Width)
}

pub fn rotate_transform(
    transform: Transform,
    geometry: VisualGeometry,
    start_pointer: Point,
    current_pointer: Point,
) -> Result<Transform, GeometryError> {
    validate_point(start_pointer)?;
    validate_point(current_pointer)?;
    let extent = geometry.oriented_raw_extent(transform)?;
    let center_local = Point::new(extent.width * 0.5, extent.height * 0.5);
    let center = geometry.forward_point(transform, center_local)?;
    let start_vector = subtract(start_pointer, center);
    let current_vector = subtract(current_pointer, center);
    let start_length = vector_length(start_vector);
    let current_length = vector_length(current_vector);
    if start_length <= f64::EPSILON || current_length <= f64::EPSILON {
        return Err(GeometryError::InvalidGeometry(
            "rotation pointer cannot be at the clip centre".to_owned(),
        ));
    }
    let delta = cross(start_vector, current_vector)
        .atan2(dot(start_vector, current_vector))
        .to_degrees();
    rotate_transform_by_degrees(transform, geometry, delta)
}

pub fn rotate_transform_by_degrees(
    transform: Transform,
    geometry: VisualGeometry,
    delta_degrees: f64,
) -> Result<Transform, GeometryError> {
    transform.validate()?;
    validate_positive_scale(delta_degrees.abs() + 1.0, "rotation delta")?;
    let extent = geometry.oriented_raw_extent(transform)?;
    let center_local = Point::new(extent.width * 0.5, extent.height * 0.5);
    let center = geometry.forward_point(transform, center_local)?;
    let mut rotated = transform;
    rotated.rotation_degrees += delta_degrees;
    rotated.position = Point::ZERO;
    let rotated_center_offset = geometry.forward_point(rotated, center_local)?;
    rotated.position = Point::new(
        center.x - rotated_center_offset.x,
        center.y - rotated_center_offset.y,
    );
    rotated.validate()?;
    Ok(rotated)
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CropOptions {
    pub lock_aspect: bool,
    pub min_width: u32,
    pub min_height: u32,
}

impl Default for CropOptions {
    fn default() -> Self {
        Self {
            lock_aspect: false,
            min_width: 1,
            min_height: 1,
        }
    }
}

/// Update the current crop from a pointer expressed in full-source pixels.
/// The crop handle is interpreted in the oriented preview plane and converted
/// back to a raw source rectangle before returning.
pub fn crop_rect_from_source_point(
    transform: Transform,
    source: SourceGeometry,
    source_point: Point,
    handle: HandleKind,
    options: CropOptions,
) -> Result<CropRect, GeometryError> {
    if !handle.is_resize() {
        return Err(GeometryError::InvalidGeometry(
            "rotation handle is not a crop handle".to_owned(),
        ));
    }
    if options.min_width == 0 || options.min_height == 0 {
        return Err(GeometryError::InvalidGeometry(
            "crop minimum dimensions must be positive".to_owned(),
        ));
    }
    validate_point(source_point)?;
    let current = current_crop(transform, source.width, source.height)?;
    let raw_width = f64::from(current.width);
    let raw_height = f64::from(current.height);
    let oriented_current_point = raw_to_oriented(
        Point::new(
            source_point.x - f64::from(current.x),
            source_point.y - f64::from(current.y),
        ),
        raw_width,
        raw_height,
        source.orientation,
    );
    let point = Point::new(
        oriented_current_point.x.clamp(
            0.0,
            raw_oriented_width(raw_width, raw_height, source.orientation),
        ),
        oriented_current_point.y.clamp(
            0.0,
            raw_oriented_height(raw_width, raw_height, source.orientation),
        ),
    );
    let current_width = raw_oriented_width(raw_width, raw_height, source.orientation);
    let current_height = raw_oriented_height(raw_width, raw_height, source.orientation);
    let mut left = 0.0;
    let mut top = 0.0;
    let mut right = current_width;
    let mut bottom = current_height;
    let minimum_width = f64::from(options.min_width).max(1.0);
    let minimum_height = f64::from(options.min_height).max(1.0);
    if handle.is_left() {
        left = point.x.clamp(0.0, right - minimum_width);
    } else if handle.is_right() {
        right = point.x.clamp(left + minimum_width, current_width);
    }
    if handle.is_top() {
        top = point.y.clamp(0.0, bottom - minimum_height);
    } else if handle.is_bottom() {
        bottom = point.y.clamp(top + minimum_height, current_height);
    }
    if options.lock_aspect {
        let corner =
            (handle.is_left() || handle.is_right()) && (handle.is_top() || handle.is_bottom());
        let aspect_width = current_width;
        let aspect_height = current_height;
        if corner {
            let factor = ((right - left) / aspect_width).max((bottom - top) / aspect_height);
            let mut width = aspect_width * factor;
            let mut height = aspect_height * factor;
            let max_width = if handle.is_left() {
                right
            } else {
                current_width - left
            };
            let max_height = if handle.is_top() {
                bottom
            } else {
                current_height - top
            };
            let cap = (max_width / aspect_width).min(max_height / aspect_height);
            width = width.min(aspect_width * cap);
            height = height.min(aspect_height * cap);
            if handle.is_left() {
                left = right - width;
            } else {
                right = left + width;
            }
            if handle.is_top() {
                top = bottom - height;
            } else {
                bottom = top + height;
            }
        } else if handle.is_left() || handle.is_right() {
            let factor = (right - left) / aspect_width;
            let height = (aspect_height * factor).min(current_height);
            top = (current_height - height) * 0.5;
            bottom = top + height;
        } else {
            let factor = (bottom - top) / aspect_height;
            let width = (aspect_width * factor).min(current_width);
            left = (current_width - width) * 0.5;
            right = left + width;
        }
    }
    let raw_rect = oriented_rect_to_raw(
        left,
        top,
        right,
        bottom,
        raw_width,
        raw_height,
        source.orientation,
    );
    let x0 = f64::from(current.x) + raw_rect.0.floor();
    let y0 = f64::from(current.y) + raw_rect.1.floor();
    let x1 = f64::from(current.x) + raw_rect.2.ceil();
    let y1 = f64::from(current.y) + raw_rect.3.ceil();
    let x = bounded_u32(x0, 0, source.width, "crop x")?;
    let y = bounded_u32(y0, 0, source.height, "crop y")?;
    let right = bounded_u32(x1, 0, source.width, "crop right")?;
    let bottom = bounded_u32(y1, 0, source.height, "crop bottom")?;
    if right <= x || bottom <= y || right - x < options.min_width || bottom - y < options.min_height
    {
        return Err(GeometryError::InvalidGeometry(
            "crop drag would produce an empty or undersized crop".to_owned(),
        ));
    }
    CropRect::new(x, y, right - x, bottom - y).map_err(GeometryError::from)
}

pub fn crop_transform_from_source_point(
    transform: Transform,
    source: SourceGeometry,
    source_point: Point,
    handle: HandleKind,
    options: CropOptions,
) -> Result<Transform, GeometryError> {
    let crop = crop_rect_from_source_point(transform, source, source_point, handle, options)?;
    let mut updated = transform;
    updated.crop = Some(crop);
    updated.validate()?;
    Ok(updated)
}

pub fn crop_transform_from_canvas_drag(
    transform: Transform,
    geometry: VisualGeometry,
    start_canvas: Point,
    current_canvas: Point,
    handle: HandleKind,
    options: CropOptions,
) -> Result<Transform, GeometryError> {
    let source = geometry.source_geometry().ok_or_else(|| {
        GeometryError::UnsupportedGeometry("crop dragging requires media geometry".to_owned())
    })?;
    // Resolving both endpoints makes invalid/non-finite drag state fail at the
    // UI boundary, even though the current endpoint determines the resulting
    // crop rectangle.
    geometry.inverse_source_point(transform, start_canvas)?;
    let current_source = geometry.inverse_source_point(transform, current_canvas)?;
    crop_transform_from_source_point(transform, source, current_source, handle, options)
}

fn current_crop(
    transform: Transform,
    source_width: u32,
    source_height: u32,
) -> Result<CropRect, GeometryError> {
    transform.validate()?;
    if let Some(crop) = transform.crop {
        if !crop.fits_within(source_width, source_height)? {
            return Err(GeometryError::InvalidGeometry(
                "current crop is outside source bounds".to_owned(),
            ));
        }
        Ok(crop)
    } else {
        CropRect::new(0, 0, source_width, source_height).map_err(GeometryError::from)
    }
}

fn oriented_dimensions(width: f64, height: f64, orientation: Orientation) -> (f64, f64) {
    match orientation {
        Orientation::Normal | Orientation::Rotate180 => (width, height),
        Orientation::Rotate90 | Orientation::Rotate270 => (height, width),
    }
}

fn raw_oriented_width(width: f64, height: f64, orientation: Orientation) -> f64 {
    oriented_dimensions(width, height, orientation).0
}

fn raw_oriented_height(width: f64, height: f64, orientation: Orientation) -> f64 {
    oriented_dimensions(width, height, orientation).1
}

fn oriented_to_raw(point: Point, width: f64, height: f64, orientation: Orientation) -> Point {
    match orientation {
        Orientation::Normal => point,
        Orientation::Rotate90 => Point::new(point.y, height - point.x),
        Orientation::Rotate180 => Point::new(width - point.x, height - point.y),
        Orientation::Rotate270 => Point::new(width - point.y, point.x),
    }
}

fn raw_to_oriented(point: Point, width: f64, height: f64, orientation: Orientation) -> Point {
    match orientation {
        Orientation::Normal => point,
        Orientation::Rotate90 => Point::new(height - point.y, point.x),
        Orientation::Rotate180 => Point::new(width - point.x, height - point.y),
        Orientation::Rotate270 => Point::new(point.y, width - point.x),
    }
}

fn oriented_rect_to_raw(
    left: f64,
    top: f64,
    right: f64,
    bottom: f64,
    width: f64,
    height: f64,
    orientation: Orientation,
) -> (f64, f64, f64, f64) {
    match orientation {
        Orientation::Normal => (left, top, right, bottom),
        Orientation::Rotate90 => (top, height - right, bottom, height - left),
        Orientation::Rotate180 => (width - right, height - bottom, width - left, height - top),
        Orientation::Rotate270 => (width - bottom, left, width - top, right),
    }
}

fn bounded_u32(value: f64, minimum: u32, maximum: u32, label: &str) -> Result<u32, GeometryError> {
    if !value.is_finite() {
        return Err(GeometryError::InvalidGeometry(format!(
            "{label} is not finite"
        )));
    }
    let rounded = value.round();
    if rounded < f64::from(minimum) || rounded > f64::from(maximum) {
        return Err(GeometryError::InvalidGeometry(format!(
            "{label} is outside source bounds"
        )));
    }
    Ok(rounded as u32)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GuideAxis {
    Vertical,
    Horizontal,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Guide {
    pub axis: GuideAxis,
    pub coordinate: f64,
}

pub fn canvas_guides<C>(canvas: C) -> Result<[Guide; 6], GeometryError>
where
    C: Into<Size>,
{
    let canvas = canvas.into();
    canvas_size_check(canvas)?;
    Ok([
        Guide {
            axis: GuideAxis::Vertical,
            coordinate: 0.0,
        },
        Guide {
            axis: GuideAxis::Vertical,
            coordinate: canvas.width * 0.5,
        },
        Guide {
            axis: GuideAxis::Vertical,
            coordinate: canvas.width,
        },
        Guide {
            axis: GuideAxis::Horizontal,
            coordinate: 0.0,
        },
        Guide {
            axis: GuideAxis::Horizontal,
            coordinate: canvas.height * 0.5,
        },
        Guide {
            axis: GuideAxis::Horizontal,
            coordinate: canvas.height,
        },
    ])
}

pub fn snap_to_guides(
    point: Point,
    guides: &[Guide],
    tolerance: f64,
) -> Result<Point, GeometryError> {
    validate_point(point)?;
    if !tolerance.is_finite() || tolerance < 0.0 {
        return Err(GeometryError::InvalidGeometry(
            "guide tolerance must be finite and non-negative".to_owned(),
        ));
    }
    let mut snapped = point;
    for guide in guides {
        if !guide.coordinate.is_finite() {
            return Err(GeometryError::InvalidGeometry(
                "guide coordinate must be finite".to_owned(),
            ));
        }
        match guide.axis {
            GuideAxis::Vertical if (snapped.x - guide.coordinate).abs() <= tolerance => {
                snapped.x = guide.coordinate
            }
            GuideAxis::Horizontal if (snapped.y - guide.coordinate).abs() <= tolerance => {
                snapped.y = guide.coordinate
            }
            _ => {}
        }
    }
    Ok(snapped)
}

fn midpoint(first: Point, second: Point) -> Point {
    Point::new((first.x + second.x) * 0.5, (first.y + second.y) * 0.5)
}

fn subtract(first: Point, second: Point) -> Point {
    Point::new(first.x - second.x, first.y - second.y)
}

fn dot(first: Point, second: Point) -> f64 {
    first.x * second.x + first.y * second.y
}

fn cross(first: Point, second: Point) -> f64 {
    first.x * second.y - first.y * second.x
}

fn squared_distance(first: Point, second: Point) -> f64 {
    let delta = subtract(first, second);
    dot(delta, delta)
}

fn vector_length(vector: Point) -> f64 {
    squared_distance(vector, Point::ZERO).sqrt()
}

fn normalized_difference(first: Point, second: Point) -> Result<Point, GeometryError> {
    let difference = subtract(first, second);
    let length = vector_length(difference);
    if !length.is_finite() || length <= f64::EPSILON {
        return Err(GeometryError::InvalidGeometry(
            "selection geometry is degenerate".to_owned(),
        ));
    }
    Ok(Point::new(difference.x / length, difference.y / length))
}

fn axis_aligned_bounds(corners: [Point; 4]) -> Result<Rect, GeometryError> {
    let mut min_x = f64::INFINITY;
    let mut min_y = f64::INFINITY;
    let mut max_x = f64::NEG_INFINITY;
    let mut max_y = f64::NEG_INFINITY;
    for corner in corners {
        validate_point(corner)?;
        min_x = min_x.min(corner.x);
        min_y = min_y.min(corner.y);
        max_x = max_x.max(corner.x);
        max_y = max_y.max(corner.y);
    }
    Rect::new(min_x, min_y, max_x - min_x, max_y - min_y)
}

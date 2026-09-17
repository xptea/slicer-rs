//! Coordinate conversion helpers shared by the CPU compositor and hit tests.

use crate::project::{ClipError, Point, Transform};

/// Return the visual extent after applying a source crop and pixel aspect.
pub fn content_extent(
    transform: Transform,
    source_width: u32,
    source_height: u32,
    pixel_aspect: f64,
) -> Result<Point, ClipError> {
    if !pixel_aspect.is_finite() || pixel_aspect <= 0.0 {
        return Err(ClipError::NonFiniteGeometry);
    }
    let extent = transform.local_extent(source_width, source_height)?;
    Ok(Point::new(extent.x * pixel_aspect, extent.y))
}

/// Convert a canvas point into untransformed content coordinates.
pub fn inverse_point(
    transform: Transform,
    source_width: u32,
    source_height: u32,
    pixel_aspect: f64,
    canvas_point: Point,
) -> Result<Point, ClipError> {
    transform.validate()?;
    let extent = content_extent(transform, source_width, source_height, pixel_aspect)?;
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
    if !pixel_aspect.is_finite() || pixel_aspect <= 0.0 {
        return Err(ClipError::NonFiniteGeometry);
    }
    Ok(Point::new(
        (rotated.x / transform.scale.x + extent.x * transform.anchor.x) / pixel_aspect,
        rotated.y / transform.scale.y + extent.y * transform.anchor.y,
    ))
}

/// Convert a source-local sample coordinate to canvas coordinates.
pub fn local_to_source(
    transform: Transform,
    source_width: u32,
    source_height: u32,
    pixel_aspect: f64,
    local: Point,
) -> Result<Point, ClipError> {
    if !pixel_aspect.is_finite() || pixel_aspect <= 0.0 {
        return Err(ClipError::NonFiniteGeometry);
    }
    transform.validate()?;
    let extent = content_extent(transform, source_width, source_height, pixel_aspect)?;
    let centered = Point::new(
        (local.x * pixel_aspect - extent.x * transform.anchor.x) * transform.scale.x,
        (local.y - extent.y * transform.anchor.y) * transform.scale.y,
    );
    let angle = transform.rotation_degrees.to_radians();
    let (sin, cos) = angle.sin_cos();
    Ok(Point::new(
        transform.position.x + centered.x * cos - centered.y * sin,
        transform.position.y + centered.x * sin + centered.y * cos,
    ))
}

/// Compute an axis-aligned canvas bounding box for a transformed rectangle.
pub fn transformed_bounds(
    transform: Transform,
    source_width: u32,
    source_height: u32,
    pixel_aspect: f64,
) -> Result<(Point, Point), ClipError> {
    let extent = content_extent(transform, source_width, source_height, pixel_aspect)?;
    let corners = [
        Point::new(0.0, 0.0),
        Point::new(extent.x, 0.0),
        Point::new(0.0, extent.y),
        extent,
    ];
    let mut min = Point::new(f64::INFINITY, f64::INFINITY);
    let mut max = Point::new(f64::NEG_INFINITY, f64::NEG_INFINITY);
    for corner in corners {
        let point = local_to_source(
            transform,
            source_width,
            source_height,
            pixel_aspect,
            Point::new(corner.x / pixel_aspect, corner.y),
        )?;
        min.x = min.x.min(point.x);
        min.y = min.y.min(point.y);
        max.x = max.x.max(point.x);
        max.y = max.y.max(point.y);
    }
    Ok((min, max))
}

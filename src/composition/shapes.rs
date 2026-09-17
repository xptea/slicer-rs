//! Analytic shape coverage for the reference renderer.

use crate::composition::color::clamp01;
use crate::project::{Point, ShapeClip, ShapeKind};

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ShapeCoverage {
    pub fill: f64,
    pub stroke: f64,
}

/// Evaluate a shape at a local point.  The point is in the shape's own
/// untransformed pixel space and coverage is intentionally analytic, making
/// small headless fixtures deterministic without a rasterizer dependency.
pub fn coverage(shape: &ShapeClip, point: Point) -> ShapeCoverage {
    let half_width = shape.width * 0.5;
    let half_height = shape.height * 0.5;
    let center = Point::new(half_width, half_height);
    let dx = point.x - center.x;
    let dy = point.y - center.y;
    let inside = match shape.shape {
        ShapeKind::Rectangle => rounded_rectangle_inside(
            dx,
            dy,
            half_width,
            half_height,
            shape.corner_radius.min(half_width.min(half_height)),
        ),
        ShapeKind::Ellipse => {
            if half_width <= 0.0 || half_height <= 0.0 {
                false
            } else {
                (dx / half_width).powi(2) + (dy / half_height).powi(2) <= 1.0
            }
        }
    };
    if !inside {
        return ShapeCoverage::default();
    }
    let stroke = shape.stroke.map_or(0.0, |stroke| {
        if stroke.width <= 0.0 {
            return 0.0;
        }
        let inset = stroke.width * 0.5;
        let inner = match shape.shape {
            ShapeKind::Rectangle => rounded_rectangle_inside(
                dx,
                dy,
                (half_width - inset).max(0.0),
                (half_height - inset).max(0.0),
                (shape.corner_radius - inset).max(0.0),
            ),
            ShapeKind::Ellipse => {
                let inner_width = (half_width - inset).max(0.0);
                let inner_height = (half_height - inset).max(0.0);
                inner_width > 0.0
                    && inner_height > 0.0
                    && (dx / inner_width).powi(2) + (dy / inner_height).powi(2) <= 1.0
            }
        };
        if inner { 0.0 } else { 1.0 }
    });
    ShapeCoverage {
        fill: 1.0,
        stroke: clamp01(stroke),
    }
}

fn rounded_rectangle_inside(
    x: f64,
    y: f64,
    half_width: f64,
    half_height: f64,
    radius: f64,
) -> bool {
    let ax = x.abs();
    let ay = y.abs();
    if ax > half_width || ay > half_height {
        return false;
    }
    if radius <= 0.0 || (ax <= half_width - radius) || (ay <= half_height - radius) {
        return true;
    }
    let corner_x = ax - (half_width - radius);
    let corner_y = ay - (half_height - radius);
    corner_x * corner_x + corner_y * corner_y <= radius * radius
}

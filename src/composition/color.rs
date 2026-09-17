//! Working-color and premultiplied-alpha helpers for the CPU compositor.

use super::frame::Rgba8;
use crate::project::Color;

/// Premultiplied working-color sample.
///
/// The project contract stores straight colors.  The compositor converts them
/// to this form before applying opacity, coverage, sampling, or layer order.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct PremultipliedRgba {
    pub r: f64,
    pub g: f64,
    pub b: f64,
    pub a: f64,
}

impl PremultipliedRgba {
    pub const TRANSPARENT: Self = Self {
        r: 0.0,
        g: 0.0,
        b: 0.0,
        a: 0.0,
    };

    pub fn from_color(color: Color) -> Self {
        Self::from_straight(color.r, color.g, color.b, color.a)
    }

    pub fn from_straight(r: f64, g: f64, b: f64, a: f64) -> Self {
        let a = clamp01(a);
        Self {
            r: clamp01(r) * a,
            g: clamp01(g) * a,
            b: clamp01(b) * a,
            a,
        }
    }

    pub fn from_rgba8(value: Rgba8) -> Self {
        let alpha = f64::from(value.a) / 255.0;
        Self {
            r: f64::from(value.r) / 255.0 * alpha,
            g: f64::from(value.g) / 255.0 * alpha,
            b: f64::from(value.b) / 255.0 * alpha,
            a: alpha,
        }
    }

    pub fn scale(self, factor: f64) -> Self {
        let factor = clamp01(factor);
        Self {
            r: self.r * factor,
            g: self.g * factor,
            b: self.b * factor,
            a: self.a * factor,
        }
    }

    /// Porter-Duff source-over with premultiplied source and destination.
    pub fn over(self, destination: Self) -> Self {
        let inverse = 1.0 - self.a;
        Self {
            r: self.r + destination.r * inverse,
            g: self.g + destination.g * inverse,
            b: self.b + destination.b * inverse,
            a: self.a + destination.a * inverse,
        }
    }

    pub fn clamp(self) -> Self {
        Self {
            r: clamp01(self.r),
            g: clamp01(self.g),
            b: clamp01(self.b),
            a: clamp01(self.a),
        }
    }

    /// Convert to the public straight-alpha RGBA8 representation.
    pub fn to_rgba8(self) -> Rgba8 {
        let value = self.clamp();
        if value.a <= f64::EPSILON {
            return Rgba8::TRANSPARENT;
        }
        let alpha = value.a;
        Rgba8::new(
            quantize(value.r / alpha),
            quantize(value.g / alpha),
            quantize(value.b / alpha),
            quantize(alpha),
        )
    }
}

pub fn clamp01(value: f64) -> f64 {
    if value.is_nan() {
        0.0
    } else {
        value.clamp(0.0, 1.0)
    }
}

pub fn quantize(value: f64) -> u8 {
    (clamp01(value) * 255.0).round() as u8
}

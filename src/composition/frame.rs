//! Bounded, owned CPU frame storage used by the reference compositor.

use std::error::Error;
use std::fmt;

/// Maximum number of pixels allocated by [`RgbaFrame::new`] unless a caller
/// supplies a different [`FrameLimits`] value.
pub const DEFAULT_MAX_PIXELS: usize = 16_777_216;

/// Bounds for allocations made by the reference compositor.
///
/// The limit is expressed in pixels rather than bytes so it applies equally
/// to the public output frame and the compositor's working storage.  A zero
/// limit is rejected; callers that need a smaller or larger bounded frame can
/// construct a value explicitly.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FrameLimits {
    max_pixels: usize,
}

impl FrameLimits {
    /// Create allocation limits for at most `max_pixels` pixels.
    pub const fn new(max_pixels: usize) -> Self {
        Self { max_pixels }
    }

    /// Return the configured pixel limit.
    pub const fn max_pixels(self) -> usize {
        self.max_pixels
    }
}

impl Default for FrameLimits {
    fn default() -> Self {
        Self::new(DEFAULT_MAX_PIXELS)
    }
}

/// A straight-alpha 8-bit RGBA sample.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Rgba8 {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Rgba8 {
    pub const TRANSPARENT: Self = Self {
        r: 0,
        g: 0,
        b: 0,
        a: 0,
    };

    pub const fn new(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self { r, g, b, a }
    }

    pub const fn as_array(self) -> [u8; 4] {
        [self.r, self.g, self.b, self.a]
    }
}

impl From<[u8; 4]> for Rgba8 {
    fn from(value: [u8; 4]) -> Self {
        Self::new(value[0], value[1], value[2], value[3])
    }
}

impl From<Rgba8> for [u8; 4] {
    fn from(value: Rgba8) -> Self {
        value.as_array()
    }
}

/// Errors returned while constructing an owned CPU frame.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FrameError {
    InvalidDimensions,
    SizeOverflow,
    PixelLimitExceeded {
        width: u32,
        height: u32,
        max_pixels: usize,
    },
    InvalidByteLength {
        actual: usize,
        expected: usize,
    },
    AllocationFailed,
}

impl fmt::Display for FrameError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidDimensions => formatter.write_str("frame dimensions must be positive"),
            Self::SizeOverflow => formatter.write_str("frame size overflowed the host usize"),
            Self::PixelLimitExceeded {
                width,
                height,
                max_pixels,
            } => write!(
                formatter,
                "frame {width}x{height} exceeds the bounded pixel limit {max_pixels}"
            ),
            Self::InvalidByteLength { actual, expected } => {
                write!(
                    formatter,
                    "RGBA byte length {actual} does not equal {expected}"
                )
            }
            Self::AllocationFailed => formatter.write_str("frame allocation failed"),
        }
    }
}

impl Error for FrameError {}

/// An owned straight-alpha RGBA8 frame.
///
/// The reference compositor performs all layer math in premultiplied floating
/// point and converts to this straight-alpha representation only when a frame
/// is finalized.  `pixels()` is therefore suitable for ordinary RGBA image
/// consumers, while the alpha compositing convention remains explicit and
/// deterministic inside the renderer.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RgbaFrame {
    width: u32,
    height: u32,
    pixels: Vec<u8>,
}

impl RgbaFrame {
    /// Allocate a transparent frame using [`FrameLimits::default`].
    pub fn new(width: u32, height: u32) -> Result<Self, FrameError> {
        Self::with_limits(width, height, FrameLimits::default())
    }

    /// Allocate a transparent frame using explicit bounded limits.
    pub fn with_limits(width: u32, height: u32, limits: FrameLimits) -> Result<Self, FrameError> {
        let byte_len = checked_byte_len(width, height, limits)?;
        let mut pixels = Vec::new();
        pixels
            .try_reserve_exact(byte_len)
            .map_err(|_| FrameError::AllocationFailed)?;
        pixels.resize(byte_len, 0);
        Ok(Self {
            width,
            height,
            pixels,
        })
    }

    /// Build a frame from a straight-alpha RGBA8 byte vector.
    pub fn from_rgba8(
        width: u32,
        height: u32,
        pixels: Vec<u8>,
        limits: FrameLimits,
    ) -> Result<Self, FrameError> {
        let expected = checked_byte_len(width, height, limits)?;
        if pixels.len() != expected {
            return Err(FrameError::InvalidByteLength {
                actual: pixels.len(),
                expected,
            });
        }
        Ok(Self {
            width,
            height,
            pixels,
        })
    }

    pub const fn width(&self) -> u32 {
        self.width
    }

    pub const fn height(&self) -> u32 {
        self.height
    }

    pub const fn dimensions(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    /// Straight-alpha RGBA8 bytes in row-major, top-to-bottom order.
    pub fn pixels(&self) -> &[u8] {
        &self.pixels
    }

    /// Mutable access for callers that need to post-process an owned frame.
    pub fn pixels_mut(&mut self) -> &mut [u8] {
        &mut self.pixels
    }

    /// Read one straight-alpha sample, returning `None` outside the frame.
    pub fn pixel(&self, x: u32, y: u32) -> Option<Rgba8> {
        let index = self.pixel_offset(x, y)?;
        Some(Rgba8::new(
            self.pixels[index],
            self.pixels[index + 1],
            self.pixels[index + 2],
            self.pixels[index + 3],
        ))
    }

    /// Set one straight-alpha sample, returning `false` outside the frame.
    pub fn set_pixel(&mut self, x: u32, y: u32, value: Rgba8) -> bool {
        let Some(index) = self.pixel_offset(x, y) else {
            return false;
        };
        self.pixels[index..index + 4].copy_from_slice(&value.as_array());
        true
    }

    fn pixel_offset(&self, x: u32, y: u32) -> Option<usize> {
        if x >= self.width || y >= self.height {
            return None;
        }
        let row = usize::try_from(y).ok()?.checked_mul(self.width as usize)?;
        row.checked_add(x as usize)?.checked_mul(4)
    }
}

pub(crate) fn checked_pixel_count(
    width: u32,
    height: u32,
    limits: FrameLimits,
) -> Result<usize, FrameError> {
    if width == 0 || height == 0 {
        return Err(FrameError::InvalidDimensions);
    }
    let pixels = usize::try_from(width)
        .ok()
        .and_then(|width| {
            usize::try_from(height)
                .ok()
                .and_then(|height| width.checked_mul(height))
        })
        .ok_or(FrameError::SizeOverflow)?;
    if limits.max_pixels == 0 || pixels > limits.max_pixels {
        return Err(FrameError::PixelLimitExceeded {
            width,
            height,
            max_pixels: limits.max_pixels,
        });
    }
    Ok(pixels)
}

pub(crate) fn checked_byte_len(
    width: u32,
    height: u32,
    limits: FrameLimits,
) -> Result<usize, FrameError> {
    checked_pixel_count(width, height, limits)?
        .checked_mul(4)
        .ok_or(FrameError::SizeOverflow)
}

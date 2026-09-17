//! Decoder contracts and the software FFmpeg fallback.
//!
//! The first decoder implementation uses the bundled FFmpeg executable.  It
//! is intentionally an application-controlled fallback: it does not inspect
//! `PATH`, returns owned CPU RGBA frames, bounds frame allocations, and lets a
//! newer seek cancel an older subprocess.  A library-backed decoder can
//! implement the same request/event contract later without changing callers.

use anyhow::{Result, bail};
use std::path::PathBuf;
use std::sync::Arc;

mod software;

pub use software::{SoftwareDecodeWorker, SoftwareDecoder};

/// Maximum dimension accepted by the fallback decoder.  This protects the
/// worker from malformed metadata and accidental unbounded allocations.
pub const MAX_FRAME_DIMENSION: u32 = 16_384;
/// Maximum RGBA frame allocation accepted by the fallback decoder.
pub const DEFAULT_MAX_FRAME_BYTES: usize = 256 * 1024 * 1024;

/// A request for one presentation frame.  Timestamps are source seconds at
/// this API boundary; the project model remains responsible for rational
/// timeline arithmetic and source-time mapping.
#[derive(Clone, Debug, PartialEq)]
pub struct DecodeRequest {
    pub path: PathBuf,
    pub timestamp: f64,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub generation: u64,
}

impl DecodeRequest {
    pub fn validate(&self) -> Result<()> {
        if self.path.as_os_str().is_empty() {
            bail!("decode path is empty");
        }
        if !self.timestamp.is_finite() || self.timestamp < 0.0 {
            bail!("decode timestamp must be a finite non-negative value");
        }
        match (self.width, self.height) {
            (Some(width), Some(height)) => validate_dimensions(width, height)?,
            (None, None) => {}
            _ => bail!("decode width and height must be provided together"),
        }
        Ok(())
    }
}

fn validate_dimensions(width: u32, height: u32) -> Result<()> {
    if width == 0 || height == 0 {
        bail!("decode dimensions must be positive");
    }
    if width > MAX_FRAME_DIMENSION || height > MAX_FRAME_DIMENSION {
        bail!(
            "decode dimensions exceed the {} pixel limit",
            MAX_FRAME_DIMENSION
        );
    }
    let bytes = usize::try_from(width)
        .ok()
        .and_then(|width| {
            usize::try_from(height)
                .ok()
                .and_then(|height| width.checked_mul(height))
        })
        .and_then(|pixels| pixels.checked_mul(4))
        .ok_or_else(|| anyhow::anyhow!("decode frame size overflows"))?;
    if bytes > DEFAULT_MAX_FRAME_BYTES {
        bail!(
            "decode frame exceeds the {} MiB limit",
            DEFAULT_MAX_FRAME_BYTES / (1024 * 1024)
        );
    }
    Ok(())
}

/// Memory backing for a decoded frame.  The enum makes a future GPU/imported
/// lease explicit instead of leaking an unowned pointer to UI code.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MemoryKind {
    CpuRgba,
}

/// An owned decoded frame that remains valid until all clones are dropped.
#[derive(Clone, Debug, PartialEq)]
pub struct FrameLease {
    pub path: PathBuf,
    pub source_pts: f64,
    pub duration: f64,
    pub width: u32,
    pub height: u32,
    pub generation: u64,
    pub memory: MemoryKind,
    pixels: Arc<[u8]>,
}

impl FrameLease {
    pub(crate) fn new(
        path: PathBuf,
        source_pts: f64,
        width: u32,
        height: u32,
        generation: u64,
        pixels: Vec<u8>,
    ) -> Self {
        Self {
            path,
            source_pts,
            duration: 0.0,
            width,
            height,
            generation,
            memory: MemoryKind::CpuRgba,
            pixels: Arc::from(pixels),
        }
    }

    /// Packed, straight-alpha RGBA pixels in row-major order.
    pub fn pixels(&self) -> &[u8] {
        &self.pixels
    }

    pub fn byte_len(&self) -> usize {
        self.pixels.len()
    }
}

/// Result emitted by a background decode worker.
#[derive(Clone, Debug)]
pub struct DecodeEvent {
    pub request: DecodeRequest,
    pub result: Result<FrameLease, String>,
}

pub(crate) fn validate_frame_size(width: u32, height: u32, byte_len: usize) -> Result<()> {
    validate_dimensions(width, height)?;
    let expected = usize::try_from(width)
        .ok()
        .and_then(|width| {
            usize::try_from(height)
                .ok()
                .and_then(|height| width.checked_mul(height))
        })
        .and_then(|pixels| pixels.checked_mul(4))
        .ok_or_else(|| anyhow::anyhow!("decoded frame size overflows"))?;
    if byte_len != expected {
        bail!("decoded frame has {byte_len} bytes; expected {expected}");
    }
    Ok(())
}

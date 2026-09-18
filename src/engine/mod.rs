//! Timeline-owned media evaluation. UI, playback and export share this model.
#[cfg(feature = "desktop")]
pub mod decoder;
pub mod export;
#[cfg(feature = "desktop")]
pub mod playback;
pub mod project;

#[cfg(feature = "desktop")]
pub mod gl_canvas;

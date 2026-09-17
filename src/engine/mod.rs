//! Application-controlled media engine building blocks.
//!
//! The engine is deliberately kept separate from the GPUI application.  Its
//! workers own media processes and frame buffers, while callers exchange
//! bounded requests and owned frame leases.  Additional scheduler, audio,
//! compositor, and presentation modules can be added without making the UI
//! responsible for decoder or GPU lifetimes.

pub mod api;
pub mod audio;
pub mod cache;
pub mod clock;
pub mod decode;
pub mod gpu;
pub mod proxy;
pub mod scheduler;

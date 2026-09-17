//! Thread-safe playback contracts shared by preview, export, and backends.
//!
//! This file is deliberately independent of GPUI and of a particular decoder
//! or audio device.  The engine owner is expected to run these contracts on a
//! worker thread.  UI code receives owned commands/events through bounded
//! `try_*` APIs and never receives a decoder pointer or a device handle.

use crate::project::{AssetId, ClipId, ProjectId, Time, TimeRange, TrackId};
use serde::{Deserialize, Serialize};
use std::error::Error;
use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender, TryRecvError, TrySendError};
use std::thread::{self, JoinHandle};

/// A committed project revision.  A revision is meaningful only within its
/// project id; workers must not compare revisions from unrelated projects.
pub type Revision = u64;

/// A monotonically increasing seek/control generation within one revision.
pub type Generation = u64;

/// Identity of an immutable project snapshot used by asynchronous work.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RevisionTag {
    pub project_id: ProjectId,
    pub revision: Revision,
}

impl RevisionTag {
    pub const fn new(project_id: ProjectId, revision: Revision) -> Self {
        Self {
            project_id,
            revision,
        }
    }
}

/// The immutable identity attached to every command, request, and event.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkTag {
    pub revision: RevisionTag,
    pub generation: Generation,
}

impl WorkTag {
    pub const fn new(revision: RevisionTag, generation: Generation) -> Self {
        Self {
            revision,
            generation,
        }
    }

    pub const fn initial(revision: RevisionTag) -> Self {
        Self::new(revision, 0)
    }

    /// Return the next generation, reserving zero as the initial generation.
    pub fn next_generation(self) -> Self {
        let generation = self.generation.checked_add(1).unwrap_or(1);
        Self { generation, ..self }
    }

    pub fn is_stale_for(self, current: Self) -> bool {
        self.revision != current.revision || self.generation < current.generation
    }
}

/// Output state used by both the project clock and backend events.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum PlaybackState {
    Paused,
    Playing,
    Ended,
}

/// A transport command.  The value is immutable once constructed and can be
/// copied across a bounded channel without borrowing project or UI state.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PlaybackCommand {
    pub tag: WorkTag,
    pub kind: PlaybackCommandKind,
}

impl PlaybackCommand {
    pub fn new(tag: WorkTag, kind: PlaybackCommandKind) -> Self {
        Self { tag, kind }
    }

    pub fn play(tag: WorkTag) -> Self {
        Self::new(tag, PlaybackCommandKind::Play)
    }

    pub fn pause(tag: WorkTag) -> Self {
        Self::new(tag, PlaybackCommandKind::Pause)
    }

    pub fn seek(tag: WorkTag, time: Time, exact: bool) -> Self {
        Self::new(tag, PlaybackCommandKind::Seek { time, exact })
    }

    pub fn is_stale_for(&self, current: WorkTag) -> bool {
        self.tag.is_stale_for(current)
    }
}

/// The command payload is separate from its immutable work tag so commands
/// can be pattern matched without losing revision/generation ownership.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum PlaybackCommandKind {
    /// Load an immutable media/project description.  The project snapshot
    /// itself is owned by the engine coordinator; this command carries only
    /// the fields needed by a transport backend.
    Load {
        path: PathBuf,
        duration: Time,
        range: Option<TimeRange>,
    },
    Play,
    Pause,
    Seek {
        time: Time,
        exact: bool,
    },
    SetRange {
        range: Option<TimeRange>,
    },
    /// Monitor mute affects preview/device output only.  It never changes
    /// clip gains or the samples retained for export.
    SetMonitorMute {
        muted: bool,
    },
    Resize {
        width: u32,
        height: u32,
    },
    RequestDiagnostics,
    Shutdown,
}

/// A compact event payload.  Events are tagged even when they are generated
/// by a backend that has no visual frame, allowing the coordinator to discard
/// late observations after a seek or edit.
#[derive(Clone, Debug, PartialEq)]
pub struct PlaybackEvent {
    pub tag: WorkTag,
    pub kind: PlaybackEventKind,
}

impl PlaybackEvent {
    pub fn new(tag: WorkTag, kind: PlaybackEventKind) -> Self {
        Self { tag, kind }
    }

    pub fn is_stale_for(&self, current: WorkTag) -> bool {
        self.tag.is_stale_for(current)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum PlaybackEventKind {
    Ready {
        duration: Time,
        range: Option<TimeRange>,
    },
    CurrentTime {
        time: Time,
        state: PlaybackState,
        eof: bool,
    },
    PresentedFrame {
        request: FrameRequest,
        frame: FrameLease,
    },
    Buffering {
        active: bool,
        pending: usize,
    },
    Error {
        code: EventErrorCode,
        message: String,
        recoverable: bool,
    },
    CapabilityChanged {
        name: String,
        available: bool,
        detail: Option<String>,
    },
    Statistics(PlaybackStatistics),
    Shutdown,
}

/// High-level transport boundary implemented by a composition backend or the
/// legacy single-video adapter.  Implementations must enqueue/defer work and
/// return without waiting for decode, presentation, or an audio device.
pub trait PlaybackBackend: Send {
    fn submit(&mut self, command: PlaybackCommand) -> Result<(), BackendError>;
    fn try_event(&mut self) -> Option<PlaybackEvent>;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BackendError {
    StaleCommand,
    InvalidCommand(String),
    Unsupported(String),
    Failed(String),
    Shutdown,
}

impl fmt::Display for BackendError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::StaleCommand => formatter.write_str("backend rejected a stale command"),
            Self::InvalidCommand(message) => {
                write!(formatter, "invalid backend command: {message}")
            }
            Self::Unsupported(message) => {
                write!(formatter, "backend operation is unsupported: {message}")
            }
            Self::Failed(message) => formatter.write_str(message),
            Self::Shutdown => formatter.write_str("backend is shut down"),
        }
    }
}

impl Error for BackendError {}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EventErrorCode {
    StaleWork,
    InvalidCommand,
    Source,
    Backend,
    DeviceLost,
    Internal,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct PlaybackStatistics {
    pub presented_frames: u64,
    pub dropped_frames: u64,
    pub decoded_frames: u64,
    pub audio_underruns: u64,
    pub queued_commands: usize,
    pub queued_events: usize,
}

/// A source instance distinguishes two independent uses of the same asset at
/// different source times.  It is intentionally not a filesystem path: an
/// engine may use an original, proxy, or another decoder instance behind it.
#[derive(
    Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize,
)]
#[serde(transparent)]
pub struct SourceId(pub u64);

impl SourceId {
    pub const fn new(value: u64) -> Self {
        Self(value)
    }
}

/// Cache identity.  Generation is deliberately absent: an exact frame from
/// the same asset/source/revision/time can be reused after a seek, then
/// retagged for the current generation before delivery.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FrameCacheKey {
    pub asset_id: AssetId,
    pub source_id: SourceId,
    pub revision: Revision,
    pub time: Time,
}

impl FrameCacheKey {
    pub const fn new(
        asset_id: AssetId,
        source_id: SourceId,
        revision: Revision,
        time: Time,
    ) -> Self {
        Self {
            asset_id,
            source_id,
            revision,
            time,
        }
    }
}

/// CPU frame layout supported by the software fallback.  A future backend
/// can add an imported/GPU variant without exposing a raw pointer to callers.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FrameMemoryKind {
    Cpu,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PixelFormat {
    Rgba8,
}

/// An owned decoded frame.  `pixels` and `lifetime` are reference counted, so
/// a compositor/cache can retain a frame after the source worker has returned.
#[derive(Clone, Debug, PartialEq)]
pub struct FrameLease {
    pub key: FrameCacheKey,
    pub tag: WorkTag,
    pub source_pts: Time,
    pub duration: Option<Time>,
    pub width: u32,
    pub height: u32,
    pub format: PixelFormat,
    pub memory: FrameMemoryKind,
    pixels: Arc<[u8]>,
    lifetime: Arc<FrameLifetime>,
}

#[derive(Debug, PartialEq, Eq)]
pub struct FrameLifetime;

impl FrameLease {
    pub fn from_rgba(
        key: FrameCacheKey,
        tag: WorkTag,
        source_pts: Time,
        duration: Option<Time>,
        width: u32,
        height: u32,
        pixels: Vec<u8>,
    ) -> Result<Self, FrameError> {
        if width == 0 || height == 0 {
            return Err(FrameError::InvalidDimensions);
        }
        let expected = usize::try_from(width)
            .ok()
            .and_then(|width| {
                usize::try_from(height)
                    .ok()
                    .and_then(|height| width.checked_mul(height))
            })
            .and_then(|pixels| pixels.checked_mul(4))
            .ok_or(FrameError::SizeOverflow)?;
        if pixels.len() != expected {
            return Err(FrameError::InvalidByteLength {
                actual: pixels.len(),
                expected,
            });
        }
        if key.revision != tag.revision.revision {
            return Err(FrameError::RevisionMismatch);
        }
        if source_pts < Time::ZERO {
            return Err(FrameError::NegativeTime);
        }
        if duration.is_some_and(|duration| duration <= Time::ZERO) {
            return Err(FrameError::InvalidDuration);
        }
        Ok(Self {
            key,
            tag,
            source_pts,
            duration,
            width,
            height,
            format: PixelFormat::Rgba8,
            memory: FrameMemoryKind::Cpu,
            pixels: Arc::from(pixels),
            lifetime: Arc::new(FrameLifetime),
        })
    }

    pub fn pixels(&self) -> &[u8] {
        &self.pixels
    }

    pub fn byte_len(&self) -> usize {
        self.pixels.len()
    }

    pub fn lifetime_token(&self) -> Arc<FrameLifetime> {
        Arc::clone(&self.lifetime)
    }

    /// Re-tag a cache hit for the current seek generation without changing
    /// its decoded bytes or revision identity.
    pub fn retag(&self, tag: WorkTag) -> Result<Self, FrameError> {
        if tag.revision != self.tag.revision || tag.revision.revision != self.key.revision {
            return Err(FrameError::RevisionMismatch);
        }
        Ok(Self {
            tag,
            key: self.key,
            source_pts: self.source_pts,
            duration: self.duration,
            width: self.width,
            height: self.height,
            format: self.format,
            memory: self.memory,
            pixels: Arc::clone(&self.pixels),
            lifetime: Arc::clone(&self.lifetime),
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FrameError {
    InvalidDimensions,
    SizeOverflow,
    InvalidByteLength { actual: usize, expected: usize },
    RevisionMismatch,
    NegativeTime,
    InvalidDuration,
}

impl fmt::Display for FrameError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidDimensions => formatter.write_str("frame dimensions must be positive"),
            Self::SizeOverflow => formatter.write_str("frame byte size overflowed"),
            Self::InvalidByteLength { actual, expected } => {
                write!(formatter, "frame has {actual} bytes; expected {expected}")
            }
            Self::RevisionMismatch => formatter.write_str("frame revision does not match its tag"),
            Self::NegativeTime => formatter.write_str("frame source time must be non-negative"),
            Self::InvalidDuration => formatter.write_str("frame duration must be positive"),
        }
    }
}

impl Error for FrameError {}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum FramePriority {
    Exact,
    Prefetch,
}

/// A visible video request owned by the scheduler/source boundary.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FrameRequest {
    pub tag: WorkTag,
    pub clip_id: ClipId,
    pub track_id: TrackId,
    pub key: FrameCacheKey,
    pub project_time: Time,
    pub source_time: Time,
    pub priority: FramePriority,
}

impl FrameRequest {
    pub fn validate(&self) -> Result<(), SourceError> {
        if self.key.revision != self.tag.revision.revision {
            return Err(SourceError::InvalidRequest(
                "frame key and request revision differ".to_owned(),
            ));
        }
        if self.key.time != self.source_time {
            return Err(SourceError::InvalidRequest(
                "frame key and request source time differ".to_owned(),
            ));
        }
        if self.project_time < Time::ZERO || self.source_time < Time::ZERO {
            return Err(SourceError::InvalidRequest(
                "frame request times must be non-negative".to_owned(),
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct FrameSourceEvent {
    pub request: FrameRequest,
    pub result: Result<FrameLease, SourceError>,
}

/// An audio request is also a worker-owned, immutable value.  Audio providers
/// return decoded sample blocks; they do not know about an output device.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AudioRequest {
    pub tag: WorkTag,
    pub asset_id: AssetId,
    pub source_id: SourceId,
    pub project_range: TimeRange,
    pub source_range: TimeRange,
    pub sample_rate: u32,
    pub channels: u16,
}

impl AudioRequest {
    pub fn validate(&self) -> Result<(), SourceError> {
        if self.sample_rate == 0 || self.channels == 0 {
            return Err(SourceError::InvalidRequest(
                "audio layout must have a positive sample rate and channel count".to_owned(),
            ));
        }
        if self.project_range.start < Time::ZERO || self.source_range.start < Time::ZERO {
            return Err(SourceError::InvalidRequest(
                "audio ranges must be non-negative".to_owned(),
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct AudioBlock {
    pub tag: WorkTag,
    pub source_id: SourceId,
    pub start: Time,
    pub sample_rate: u32,
    pub channels: u16,
    pub samples: Arc<[f32]>,
}

impl AudioBlock {
    pub fn new(
        tag: WorkTag,
        source_id: SourceId,
        start: Time,
        sample_rate: u32,
        channels: u16,
        samples: Vec<f32>,
    ) -> Result<Self, SourceError> {
        if sample_rate == 0 || channels == 0 {
            return Err(SourceError::InvalidRequest(
                "audio layout must have a positive sample rate and channel count".to_owned(),
            ));
        }
        if start < Time::ZERO {
            return Err(SourceError::InvalidRequest(
                "audio block start must be non-negative".to_owned(),
            ));
        }
        if !samples.len().is_multiple_of(usize::from(channels)) {
            return Err(SourceError::InvalidRequest(
                "audio sample count is not divisible by channel count".to_owned(),
            ));
        }
        if samples.iter().any(|sample| !sample.is_finite()) {
            return Err(SourceError::InvalidRequest(
                "audio samples must be finite".to_owned(),
            ));
        }
        Ok(Self {
            tag,
            source_id,
            start,
            sample_rate,
            channels,
            samples: Arc::from(samples),
        })
    }

    pub fn frames(&self) -> usize {
        self.samples.len() / usize::from(self.channels)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct AudioSourceEvent {
    pub request: AudioRequest,
    pub result: Result<AudioBlock, SourceError>,
}

/// Errors from bounded queues and source contracts.  No method on the public
/// boundary waits for capacity or for a worker to finish.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SourceError {
    InvalidRequest(String),
    QueueFull,
    Disconnected,
    Cancelled,
    Failed(String),
}

impl fmt::Display for SourceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidRequest(message) => write!(formatter, "invalid source request: {message}"),
            Self::QueueFull => formatter.write_str("source request queue is full"),
            Self::Disconnected => formatter.write_str("source worker is disconnected"),
            Self::Cancelled => formatter.write_str("source request was cancelled"),
            Self::Failed(message) => formatter.write_str(message),
        }
    }
}

impl Error for SourceError {}

/// A source is driven by an engine worker.  All entry points are non-blocking;
/// implementations may decode on their own worker thread.
pub trait FrameSource: Send {
    fn request_frame(&self, request: FrameRequest) -> Result<(), SourceError>;
    fn try_receive(&self) -> Option<FrameSourceEvent>;
    fn cancel_generation(&self, generation: Generation);
}

pub trait AudioSource: Send {
    fn request_audio(&self, request: AudioRequest) -> Result<(), SourceError>;
    fn try_receive(&self) -> Option<AudioSourceEvent>;
    fn cancel_generation(&self, generation: Generation);
}

/// Software fallback hook.  The callback is called only by the source worker,
/// never by the UI thread.  It can wrap the existing FFmpeg/software decoder.
pub trait SoftwareFrameDecoder: Send + Sync + 'static {
    fn decode(&self, request: &FrameRequest) -> Result<FrameLease, SourceError>;
}

impl<F> SoftwareFrameDecoder for F
where
    F: Fn(&FrameRequest) -> Result<FrameLease, SourceError> + Send + Sync + 'static,
{
    fn decode(&self, request: &FrameRequest) -> Result<FrameLease, SourceError> {
        self(request)
    }
}

pub trait SoftwareAudioDecoder: Send + Sync + 'static {
    fn decode(&self, request: &AudioRequest) -> Result<AudioBlock, SourceError>;
}

impl<F> SoftwareAudioDecoder for F
where
    F: Fn(&AudioRequest) -> Result<AudioBlock, SourceError> + Send + Sync + 'static,
{
    fn decode(&self, request: &AudioRequest) -> Result<AudioBlock, SourceError> {
        self(request)
    }
}

/// A bounded software frame source.  It coalesces requests received while a
/// decode is in flight, so rapid scrubbing does not build an unbounded queue.
pub struct SoftwareFrameSource<D> {
    requests: Option<SyncSender<FrameRequest>>,
    events: Receiver<FrameSourceEvent>,
    stop: Arc<AtomicBool>,
    cancelled_generation: Arc<AtomicU64>,
    worker: Option<JoinHandle<()>>,
    _decoder: std::marker::PhantomData<D>,
}

impl<D> SoftwareFrameSource<D>
where
    D: SoftwareFrameDecoder,
{
    pub fn new(
        decoder: D,
        request_capacity: usize,
        event_capacity: usize,
    ) -> Result<Self, SourceError> {
        if request_capacity == 0 || event_capacity == 0 {
            return Err(SourceError::InvalidRequest(
                "software source capacities must be positive".to_owned(),
            ));
        }
        let (request_tx, request_rx): (SyncSender<FrameRequest>, Receiver<FrameRequest>) =
            mpsc::sync_channel(request_capacity);
        let (event_tx, events): (SyncSender<FrameSourceEvent>, Receiver<FrameSourceEvent>) =
            mpsc::sync_channel(event_capacity);
        let stop = Arc::new(AtomicBool::new(false));
        let cancelled_generation = Arc::new(AtomicU64::new(0));
        let worker_stop = Arc::clone(&stop);
        let worker_cancelled = Arc::clone(&cancelled_generation);
        let worker = thread::Builder::new()
            .name("slicer-software-frame-source".to_owned())
            .spawn(move || {
                while let Ok(mut request) = request_rx.recv() {
                    while let Ok(newer) = request_rx.try_recv() {
                        request = newer;
                    }
                    if worker_stop.load(Ordering::Acquire)
                        || request.tag.generation < worker_cancelled.load(Ordering::Acquire)
                    {
                        continue;
                    }
                    let result = decoder.decode(&request);
                    if worker_stop.load(Ordering::Acquire)
                        || request.tag.generation < worker_cancelled.load(Ordering::Acquire)
                    {
                        continue;
                    }
                    if event_tx
                        .try_send(FrameSourceEvent { request, result })
                        .is_err()
                    {
                        // The event owner is allowed to fall behind; dropping
                        // an old frame is preferable to blocking the decoder.
                    }
                }
            })
            .map_err(|error| {
                SourceError::Failed(format!("unable to start frame source: {error}"))
            })?;
        Ok(Self {
            requests: Some(request_tx),
            events,
            stop,
            cancelled_generation,
            worker: Some(worker),
            _decoder: std::marker::PhantomData,
        })
    }
}

impl<D> FrameSource for SoftwareFrameSource<D>
where
    D: SoftwareFrameDecoder,
{
    fn request_frame(&self, request: FrameRequest) -> Result<(), SourceError> {
        request.validate()?;
        let sender = self.requests.as_ref().ok_or(SourceError::Disconnected)?;
        sender.try_send(request).map_err(|error| match error {
            TrySendError::Full(_) => SourceError::QueueFull,
            TrySendError::Disconnected(_) => SourceError::Disconnected,
        })
    }

    fn try_receive(&self) -> Option<FrameSourceEvent> {
        self.events.try_recv().ok()
    }

    fn cancel_generation(&self, generation: Generation) {
        self.cancelled_generation
            .fetch_max(generation, Ordering::AcqRel);
    }
}

impl<D> Drop for SoftwareFrameSource<D> {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        self.requests.take();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

/// The audio counterpart to [`SoftwareFrameSource`].  The provider returns
/// sample blocks, leaving resampling and mixing to `audio.rs`.
pub struct SoftwareAudioSource<D> {
    requests: Option<SyncSender<AudioRequest>>,
    events: Receiver<AudioSourceEvent>,
    stop: Arc<AtomicBool>,
    cancelled_generation: Arc<AtomicU64>,
    worker: Option<JoinHandle<()>>,
    _decoder: std::marker::PhantomData<D>,
}

impl<D> SoftwareAudioSource<D>
where
    D: SoftwareAudioDecoder,
{
    pub fn new(
        decoder: D,
        request_capacity: usize,
        event_capacity: usize,
    ) -> Result<Self, SourceError> {
        if request_capacity == 0 || event_capacity == 0 {
            return Err(SourceError::InvalidRequest(
                "software source capacities must be positive".to_owned(),
            ));
        }
        let (request_tx, request_rx): (SyncSender<AudioRequest>, Receiver<AudioRequest>) =
            mpsc::sync_channel(request_capacity);
        let (event_tx, events): (SyncSender<AudioSourceEvent>, Receiver<AudioSourceEvent>) =
            mpsc::sync_channel(event_capacity);
        let stop = Arc::new(AtomicBool::new(false));
        let cancelled_generation = Arc::new(AtomicU64::new(0));
        let worker_stop = Arc::clone(&stop);
        let worker_cancelled = Arc::clone(&cancelled_generation);
        let worker = thread::Builder::new()
            .name("slicer-software-audio-source".to_owned())
            .spawn(move || {
                while let Ok(mut request) = request_rx.recv() {
                    while let Ok(newer) = request_rx.try_recv() {
                        request = newer;
                    }
                    if worker_stop.load(Ordering::Acquire)
                        || request.tag.generation < worker_cancelled.load(Ordering::Acquire)
                    {
                        continue;
                    }
                    let result = decoder.decode(&request);
                    if worker_stop.load(Ordering::Acquire)
                        || request.tag.generation < worker_cancelled.load(Ordering::Acquire)
                    {
                        continue;
                    }
                    if event_tx
                        .try_send(AudioSourceEvent { request, result })
                        .is_err()
                    {
                        // Do not let a slow consumer block a source worker.
                    }
                }
            })
            .map_err(|error| {
                SourceError::Failed(format!("unable to start audio source: {error}"))
            })?;
        Ok(Self {
            requests: Some(request_tx),
            events,
            stop,
            cancelled_generation,
            worker: Some(worker),
            _decoder: std::marker::PhantomData,
        })
    }
}

impl<D> AudioSource for SoftwareAudioSource<D>
where
    D: SoftwareAudioDecoder,
{
    fn request_audio(&self, request: AudioRequest) -> Result<(), SourceError> {
        request.validate()?;
        let sender = self.requests.as_ref().ok_or(SourceError::Disconnected)?;
        sender.try_send(request).map_err(|error| match error {
            TrySendError::Full(_) => SourceError::QueueFull,
            TrySendError::Disconnected(_) => SourceError::Disconnected,
        })
    }

    fn try_receive(&self) -> Option<AudioSourceEvent> {
        self.events.try_recv().ok()
    }

    fn cancel_generation(&self, generation: Generation) {
        self.cancelled_generation
            .fetch_max(generation, Ordering::AcqRel);
    }
}

impl<D> Drop for SoftwareAudioSource<D> {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        self.requests.take();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

/// A non-blocking bounded command sender.  There is intentionally no `send`
/// method here: a UI event handler must be able to report a full queue rather
/// than waiting on an engine worker.
#[derive(Clone)]
pub struct CommandSender {
    sender: SyncSender<PlaybackCommand>,
}

pub struct CommandReceiver {
    receiver: Receiver<PlaybackCommand>,
}

pub fn command_channel(capacity: usize) -> Result<(CommandSender, CommandReceiver), QueueError> {
    if capacity == 0 {
        return Err(QueueError::ZeroCapacity);
    }
    let (sender, receiver) = mpsc::sync_channel(capacity);
    Ok((CommandSender { sender }, CommandReceiver { receiver }))
}

impl CommandSender {
    pub fn try_send(&self, command: PlaybackCommand) -> Result<(), QueueError> {
        self.sender.try_send(command).map_err(|error| match error {
            TrySendError::Full(_) => QueueError::Full,
            TrySendError::Disconnected(_) => QueueError::Disconnected,
        })
    }
}

impl CommandReceiver {
    pub fn try_recv(&self) -> Result<Option<PlaybackCommand>, QueueError> {
        match self.receiver.try_recv() {
            Ok(command) => Ok(Some(command)),
            Err(TryRecvError::Empty) => Ok(None),
            Err(TryRecvError::Disconnected) => Err(QueueError::Disconnected),
        }
    }

    /// Drain all currently available commands and return the newest one.  The
    /// engine may use this for coalescible seek/control commands.
    pub fn try_recv_latest(&self) -> Result<Option<PlaybackCommand>, QueueError> {
        let mut latest = self.try_recv()?;
        loop {
            match self.receiver.try_recv() {
                Ok(command) => latest = Some(command),
                Err(TryRecvError::Empty) => return Ok(latest),
                Err(TryRecvError::Disconnected) => {
                    return if latest.is_some() {
                        Ok(latest)
                    } else {
                        Err(QueueError::Disconnected)
                    };
                }
            }
        }
    }
}

#[derive(Clone)]
pub struct EventSender {
    sender: SyncSender<PlaybackEvent>,
}

pub struct EventReceiver {
    receiver: Receiver<PlaybackEvent>,
}

pub fn event_channel(capacity: usize) -> Result<(EventSender, EventReceiver), QueueError> {
    if capacity == 0 {
        return Err(QueueError::ZeroCapacity);
    }
    let (sender, receiver) = mpsc::sync_channel(capacity);
    Ok((EventSender { sender }, EventReceiver { receiver }))
}

impl EventSender {
    pub fn try_send(&self, event: PlaybackEvent) -> Result<(), QueueError> {
        self.sender.try_send(event).map_err(|error| match error {
            TrySendError::Full(_) => QueueError::Full,
            TrySendError::Disconnected(_) => QueueError::Disconnected,
        })
    }
}

impl EventReceiver {
    pub fn try_recv(&self) -> Result<Option<PlaybackEvent>, QueueError> {
        match self.receiver.try_recv() {
            Ok(event) => Ok(Some(event)),
            Err(TryRecvError::Empty) => Ok(None),
            Err(TryRecvError::Disconnected) => Err(QueueError::Disconnected),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum QueueError {
    ZeroCapacity,
    Full,
    Disconnected,
}

impl fmt::Display for QueueError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ZeroCapacity => formatter.write_str("queue capacity must be positive"),
            Self::Full => formatter.write_str("queue is full"),
            Self::Disconnected => formatter.write_str("queue is disconnected"),
        }
    }
}

impl Error for QueueError {}

//! Persistent, native libmpv playback for the GPUI preview surface.
//!
//! One libmpv client handle lives for the lifetime of a player. Commands are
//! sent to a worker thread, so decoding, audio, seeking, and mpv's GPU video
//! output never run on the GPUI thread. The worker observes playback
//! properties and publishes a lock-free snapshot that the UI can poll.
//!
//! Only the stable client part of libmpv's C ABI is declared here. Loading the
//! library at runtime keeps development and packaging independent of a system
//! linker configuration. Packaged applications resolve libmpv beside the
//! executable; a development build can select a different library explicitly
//! with SLICER_MPV_LIBRARY.

use libloading::Library;
use std::collections::HashMap;
use std::ffi::{CStr, CString, OsStr};
use std::os::raw::{c_char, c_double, c_int, c_void};
use std::path::{Path, PathBuf};
use std::ptr;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender, TryRecvError, TrySendError};
use std::sync::{Arc, Mutex, RwLock};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

#[cfg(unix)]
use std::os::unix::ffi::OsStrExt;

const MPV_FORMAT_STRING: c_int = 1;
const MPV_FORMAT_FLAG: c_int = 3;
const MPV_FORMAT_INT64: c_int = 4;
const MPV_FORMAT_DOUBLE: c_int = 5;
const MPV_FORMAT_NODE: c_int = 6;

const MPV_EVENT_NONE: c_int = 0;
const MPV_EVENT_SHUTDOWN: c_int = 1;
const MPV_EVENT_COMMAND_REPLY: c_int = 5;
const MPV_EVENT_START_FILE: c_int = 6;
const MPV_EVENT_END_FILE: c_int = 7;
const MPV_EVENT_FILE_LOADED: c_int = 8;
const MPV_EVENT_SEEK: c_int = 20;
const MPV_EVENT_PLAYBACK_RESTART: c_int = 21;
const MPV_EVENT_PROPERTY_CHANGE: c_int = 22;

const MAX_AUDIO_NODE_DEPTH: usize = 4;
const MAX_AUDIO_NODE_ENTRIES: usize = 32;
const MAX_EVENTS_PER_TICK: usize = 64;
const EVENT_WAIT_SECONDS: c_double = 0.025;
// High-resolution seeks decode forward from the previous keyframe. During a
// drag, keep the latest target but give the decoder at least one frame's worth
// of time before dispatching another request. The final exact release seek is
// never delayed by this cadence.
const DRAG_SEEK_INTERVAL: Duration = Duration::from_millis(33);
const DRAG_SEEK_SETTLE_TIMEOUT: Duration = Duration::from_millis(250);
const EXACT_SEEK_SETTLE_TIMEOUT: Duration = Duration::from_secs(2);

fn startup_timing(label: &str, began: Instant) {
    if std::env::var_os("SLICER_MPV_TIMING").is_some() {
        eprintln!(
            "native_player_timing label={label} elapsed_ms={:.1}",
            began.elapsed().as_secs_f64() * 1000.0
        );
    }
}

type MpvHandle = c_void;

type MpvCreate = unsafe extern "C" fn() -> *mut MpvHandle;
type MpvInitialize = unsafe extern "C" fn(*mut MpvHandle) -> c_int;
type MpvTerminateDestroy = unsafe extern "C" fn(*mut MpvHandle);
type MpvSetOptionString =
    unsafe extern "C" fn(*mut MpvHandle, *const c_char, *const c_char) -> c_int;
type MpvCommandAsync = unsafe extern "C" fn(*mut MpvHandle, u64, *const *const c_char) -> c_int;
type MpvObserveProperty = unsafe extern "C" fn(*mut MpvHandle, u64, *const c_char, c_int) -> c_int;
type MpvWaitEvent = unsafe extern "C" fn(*mut MpvHandle, c_double) -> *mut MpvEvent;
type MpvWakeup = unsafe extern "C" fn(*mut MpvHandle);
type MpvErrorString = unsafe extern "C" fn(c_int) -> *const c_char;

/// A point-in-time view of the native player.
///
/// Values that mpv cannot provide for the current file are represented by
/// None. Decoder and output diagnostics are read from mpv rather than guessed
/// from the host GPU.
#[derive(Clone, Debug, PartialEq)]
pub struct NativePlayerSnapshot {
    pub position: f64,
    pub duration: f64,
    pub paused: bool,
    pub seeking: bool,
    pub loaded: bool,
    pub eof: bool,
    pub error: Option<String>,
    pub hwdec: Option<String>,
    pub video_output: Option<String>,
    pub video_width: Option<u32>,
    pub video_height: Option<u32>,
    pub display_fps: Option<f64>,
    pub estimated_vf_fps: Option<f64>,
    pub audio_codec: Option<String>,
    pub audio_params: Option<String>,
    pub audio_active: Option<bool>,
    pub decoder_frame_drop_count: Option<u64>,
    pub muted: bool,
}

impl Default for NativePlayerSnapshot {
    fn default() -> Self {
        Self {
            position: 0.0,
            duration: 0.0,
            paused: true,
            seeking: false,
            loaded: false,
            eof: false,
            error: None,
            hwdec: None,
            video_output: None,
            video_width: None,
            video_height: None,
            display_fps: None,
            estimated_vf_fps: None,
            audio_codec: None,
            audio_params: None,
            audio_active: None,
            decoder_frame_drop_count: None,
            muted: false,
        }
    }
}

/// Short name for callers that prefer it.
pub type Snapshot = NativePlayerSnapshot;

/// Resolve the libmpv path selected by the current installation.
///
/// SLICER_MPV_LIBRARY is an explicit development/test override. Without it,
/// only executable-relative packaged locations are checked. There is no
/// implicit system fallback, so a packaged build cannot silently depend on an
/// unrelated host mpv installation.
pub fn resolve_mpv_library() -> Result<PathBuf, String> {
    if let Some(path) = std::env::var_os("SLICER_MPV_LIBRARY") {
        let path = PathBuf::from(path);
        if path.is_file() {
            return Ok(path);
        }
        return Err(format!(
            "SLICER_MPV_LIBRARY does not point to a regular file: {}",
            path.display()
        ));
    }

    let executable = std::env::current_exe()
        .map_err(|error| format!("unable to determine application path: {error}"))?;
    let executable_dir = executable
        .parent()
        .ok_or_else(|| "application path has no parent directory".to_owned())?;
    let name = mpv_library_name();
    let candidates = [
        executable_dir.join("../lib/slicer/playback").join(name),
        executable_dir.join("../lib/slicer").join(name),
        executable_dir.join("resources").join(name),
        executable_dir.join("resources/lib").join(name),
    ];
    candidates
        .iter()
        .find(|path| path.is_file())
        .cloned()
        .ok_or_else(|| {
            format!(
                "bundled libmpv was not found; checked {}; set SLICER_MPV_LIBRARY for development",
                candidates
                    .iter()
                    .map(|path| path.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        })
}

fn mpv_library_name() -> &'static str {
    #[cfg(target_os = "windows")]
    {
        "mpv-2.dll"
    }
    #[cfg(target_os = "macos")]
    {
        "libmpv.2.dylib"
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        "libmpv.so.2"
    }
}

struct MpvApi {
    // Keep the library alive for as long as any copied function pointer is used.
    _library: Library,
    create: MpvCreate,
    initialize: MpvInitialize,
    terminate_destroy: MpvTerminateDestroy,
    set_option_string: MpvSetOptionString,
    command_async: MpvCommandAsync,
    observe_property: MpvObserveProperty,
    wait_event: MpvWaitEvent,
    wakeup: Option<MpvWakeup>,
    error_string: MpvErrorString,
}

impl MpvApi {
    fn load(path: &Path) -> Result<Self, String> {
        // Library::new is unsafe because loading foreign code executes its
        // initialization code. The path is explicitly selected by the caller.
        let library = unsafe { Library::new(path) }
            .map_err(|error| format!("unable to load libmpv {}: {error}", path.display()))?;

        unsafe fn symbol<T: Copy>(library: &Library, name: &'static [u8]) -> Result<T, String> {
            unsafe { library.get::<T>(name) }
                .map(|symbol| *symbol)
                .map_err(|error| {
                    format!(
                        "selected libmpv is missing {}: {error}",
                        String::from_utf8_lossy(&name[..name.len() - 1])
                    )
                })
        }

        Ok(Self {
            create: unsafe { symbol(&library, b"mpv_create\0")? },
            initialize: unsafe { symbol(&library, b"mpv_initialize\0")? },
            terminate_destroy: unsafe { symbol(&library, b"mpv_terminate_destroy\0")? },
            set_option_string: unsafe { symbol(&library, b"mpv_set_option_string\0")? },
            command_async: unsafe { symbol(&library, b"mpv_command_async\0")? },
            observe_property: unsafe { symbol(&library, b"mpv_observe_property\0")? },
            wait_event: unsafe { symbol(&library, b"mpv_wait_event\0")? },
            // Current libmpv releases export mpv_wakeup. Keep this optional
            // for older private runtimes; the worker still has its bounded
            // wait timeout as a fallback.
            wakeup: unsafe {
                library
                    .get::<MpvWakeup>(b"mpv_wakeup\0")
                    .ok()
                    .map(|symbol| *symbol)
            },
            error_string: unsafe { symbol(&library, b"mpv_error_string\0")? },
            _library: library,
        })
    }

    #[allow(unsafe_op_in_unsafe_fn)]
    unsafe fn error(&self, code: c_int) -> String {
        let ptr = unsafe { (self.error_string)(code) };
        if ptr.is_null() {
            return format!("libmpv error {code}");
        }
        unsafe { CStr::from_ptr(ptr) }
            .to_string_lossy()
            .into_owned()
    }
}

// The library is moved to the one worker that owns every mpv call. It is never
// accessed concurrently from another thread.
unsafe impl Send for MpvApi {}

#[repr(C)]
struct MpvEvent {
    event_id: c_int,
    error: c_int,
    reply_userdata: u64,
    data: *mut c_void,
}

#[repr(C)]
struct MpvEventProperty {
    name: *const c_char,
    format: c_int,
    data: *mut c_void,
}

#[repr(C)]
struct MpvEventEndFile {
    reason: c_int,
    error: c_int,
    playlist_entry_id: u64,
    playlist_insert_id: u64,
    playlist_insert_num_entries: i64,
}

#[repr(C)]
union MpvNodeData {
    string: *mut c_char,
    flag: c_int,
    int64: i64,
    double_: c_double,
    list: *mut MpvNodeList,
    byte_array: *mut MpvByteArray,
}

#[repr(C)]
struct MpvNode {
    data: MpvNodeData,
    format: c_int,
}

#[repr(C)]
struct MpvNodeList {
    num: c_int,
    values: *mut MpvNode,
    keys: *mut *mut c_char,
}

#[repr(C)]
struct MpvByteArray {
    data: *mut c_void,
    size: usize,
}

#[derive(Clone, Copy)]
struct SeekRequest {
    seconds: f64,
    exact: bool,
    serial: u64,
}

/// An asynchronous mpv command that still needs a `MPV_EVENT_COMMAND_REPLY`
/// acknowledgement. Keeping the command kind alongside the reply id lets the
/// worker surface failures without waiting in `mpv_command` and lets stale
/// seek/control replies avoid clobbering a newer request.
#[derive(Clone, Copy)]
enum PendingCommand {
    Speed,
    Load,
    Seek(SeekRequest),
    Pause(bool),
    Mute(bool),
    Crop,
}

#[derive(Clone)]
struct Range {
    start: f64,
    end: f64,
}

#[derive(Default)]
struct PendingControls {
    paused: Option<bool>,
    muted: Option<bool>,
    range: Option<Range>,
    crop: Option<String>,
    speed: Option<f64>,
}

struct SharedState {
    position: AtomicU64,
    duration: AtomicU64,
    paused: AtomicBool,
    seeking: AtomicBool,
    loaded: AtomicBool,
    eof: AtomicBool,
    muted: AtomicBool,
    error: RwLock<Option<String>>,
    hwdec: RwLock<Option<String>>,
    video_output: RwLock<Option<String>>,
    video_width: AtomicU64,
    video_height: AtomicU64,
    display_fps: AtomicU64,
    estimated_vf_fps: AtomicU64,
    audio_codec: RwLock<Option<String>>,
    audio_params: RwLock<Option<String>>,
    audio_active: AtomicU64,
    decoder_frame_drop_count: AtomicU64,
}

impl Default for SharedState {
    fn default() -> Self {
        Self {
            position: AtomicU64::new(0.0f64.to_bits()),
            duration: AtomicU64::new(0.0f64.to_bits()),
            paused: AtomicBool::new(true),
            seeking: AtomicBool::new(false),
            loaded: AtomicBool::new(false),
            eof: AtomicBool::new(false),
            muted: AtomicBool::new(false),
            error: RwLock::new(None),
            hwdec: RwLock::new(None),
            video_output: RwLock::new(None),
            video_width: AtomicU64::new(0),
            video_height: AtomicU64::new(0),
            display_fps: AtomicU64::new(0.0f64.to_bits()),
            estimated_vf_fps: AtomicU64::new(0.0f64.to_bits()),
            audio_codec: RwLock::new(None),
            audio_params: RwLock::new(None),
            audio_active: AtomicU64::new(0),
            decoder_frame_drop_count: AtomicU64::new(0),
        }
    }
}

impl SharedState {
    fn snapshot(&self) -> NativePlayerSnapshot {
        NativePlayerSnapshot {
            position: f64::from_bits(self.position.load(Ordering::Acquire)),
            duration: f64::from_bits(self.duration.load(Ordering::Acquire)),
            paused: self.paused.load(Ordering::Acquire),
            seeking: self.seeking.load(Ordering::Acquire),
            loaded: self.loaded.load(Ordering::Acquire),
            eof: self.eof.load(Ordering::Acquire),
            error: try_clone_lock(&self.error),
            hwdec: try_clone_lock(&self.hwdec),
            video_output: try_clone_lock(&self.video_output),
            video_width: nonzero_u32(self.video_width.load(Ordering::Acquire)),
            video_height: nonzero_u32(self.video_height.load(Ordering::Acquire)),
            display_fps: nonzero_f64(self.display_fps.load(Ordering::Acquire)),
            estimated_vf_fps: nonzero_f64(self.estimated_vf_fps.load(Ordering::Acquire)),
            audio_codec: try_clone_lock(&self.audio_codec),
            audio_params: try_clone_lock(&self.audio_params),
            audio_active: match self.audio_active.load(Ordering::Acquire) {
                1 => Some(true),
                2 => Some(false),
                _ => None,
            },
            decoder_frame_drop_count: nonzero_u64(
                self.decoder_frame_drop_count.load(Ordering::Acquire),
            ),
            muted: self.muted.load(Ordering::Acquire),
        }
    }

    fn clear_for_load(&self, start: f64) {
        self.position.store(start.to_bits(), Ordering::Release);
        self.duration.store(0.0f64.to_bits(), Ordering::Release);
        self.paused.store(true, Ordering::Release);
        self.seeking.store(false, Ordering::Release);
        self.loaded.store(false, Ordering::Release);
        self.eof.store(false, Ordering::Release);
        self.set_string(&self.error, None);
        self.set_string(&self.hwdec, None);
        self.set_string(&self.video_output, None);
        self.video_width.store(0, Ordering::Release);
        self.video_height.store(0, Ordering::Release);
        self.display_fps.store(0.0f64.to_bits(), Ordering::Release);
        self.estimated_vf_fps
            .store(0.0f64.to_bits(), Ordering::Release);
        self.set_string(&self.audio_codec, None);
        self.set_string(&self.audio_params, None);
        self.audio_active.store(0, Ordering::Release);
        self.decoder_frame_drop_count.store(0, Ordering::Release);
    }

    fn set_error(&self, value: impl Into<String>) {
        self.set_string(&self.error, Some(value.into()));
    }

    fn set_string(&self, target: &RwLock<Option<String>>, value: Option<String>) {
        if let Ok(mut slot) = target.write() {
            *slot = value;
        }
    }
}

fn try_clone_lock(lock: &RwLock<Option<String>>) -> Option<String> {
    lock.try_read().ok().and_then(|slot| slot.clone())
}

fn nonzero_u32(value: u64) -> Option<u32> {
    (value > 0).then(|| value.min(u32::MAX as u64) as u32)
}

fn nonzero_u64(value: u64) -> Option<u64> {
    (value > 0).then_some(value)
}

fn nonzero_f64(value: u64) -> Option<f64> {
    let value = f64::from_bits(value);
    (value.is_finite() && value > 0.0).then_some(value)
}

enum WorkerCommand {
    Load {
        path: CString,
        start: f64,
        end: Option<f64>,
    },
    Wake,
    Shutdown,
}

#[derive(Clone, Copy)]
struct MpvWaker {
    wakeup: MpvWakeup,
    handle: *mut MpvHandle,
}

// The wakeup callback and handle are installed only after mpv initialization
// and are cleared before the worker destroys the handle. NativePlayer::drop
// uses this pair only to interrupt mpv_wait_event before joining the worker.
unsafe impl Send for MpvWaker {}
unsafe impl Sync for MpvWaker {}

struct Shared {
    commands: SyncSender<WorkerCommand>,
    pending_seek: Mutex<Option<SeekRequest>>,
    pending_controls: Mutex<PendingControls>,
    mpv_waker: Mutex<Option<MpvWaker>>,
    next_seek_serial: AtomicU64,
    wake_pending: AtomicBool,
    shutdown: AtomicBool,
}

impl Shared {
    fn set_mpv_waker(&self, waker: Option<MpvWaker>) {
        if let Ok(mut slot) = self.mpv_waker.lock() {
            *slot = waker;
        }
    }

    fn wake_mpv(&self) {
        if let Ok(slot) = self.mpv_waker.lock()
            && let Some(waker) = *slot
        {
            // Keep the guard through the C call. The worker clears this slot
            // immediately before destroying the handle, so releasing it first
            // would allow a use-after-free during teardown.
            unsafe { (waker.wakeup)(waker.handle) };
        }
    }
}

/// A persistent libmpv player attached to a native child window.
pub struct NativePlayer {
    shared: Arc<Shared>,
    state: Arc<SharedState>,
    worker: Option<JoinHandle<()>>,
    window_id: u64,
    render_handle: usize,
}

impl NativePlayer {
    /// Create a player using the packaged libmpv location.
    pub fn new(window_id: u64) -> Result<Self, String> {
        let began = Instant::now();
        let path = resolve_mpv_library()?;
        let player = Self::new_with_library(window_id, path)?;
        startup_timing("new_total", began);
        Ok(player)
    }

    /// Create a player from an explicit libmpv path for development/tests.
    pub fn new_with_library(window_id: u64, library: impl AsRef<Path>) -> Result<Self, String> {
        Self::new_internal(window_id, library.as_ref(), false)
    }

    /// A controller for a caller-owned OpenGL render context. Audio is mixed by the timeline.
    pub(crate) fn new_for_render() -> Result<Self, String> {
        Self::new_internal(0, &resolve_mpv_library()?, true)
    }

    /// Only the render API may use this handle, on a separate render thread.
    /// The render context must be freed before dropping this player.
    pub(crate) fn render_handle(&self) -> *mut c_void {
        self.render_handle as *mut c_void
    }

    fn new_internal(window_id: u64, library: &Path, render: bool) -> Result<Self, String> {
        let library = library.to_path_buf();
        if !library.is_file() {
            return Err(format!(
                "libmpv library does not exist: {}",
                library.display()
            ));
        }
        let api_began = Instant::now();
        let api = MpvApi::load(&library)?;
        startup_timing("load_library", api_began);
        let state = Arc::new(SharedState::default());
        let (commands, receiver) = mpsc::sync_channel(32);
        let shared = Arc::new(Shared {
            commands,
            pending_seek: Mutex::new(None),
            pending_controls: Mutex::new(PendingControls::default()),
            mpv_waker: Mutex::new(None),
            next_seek_serial: AtomicU64::new(0),
            wake_pending: AtomicBool::new(false),
            shutdown: AtomicBool::new(false),
        });

        let worker_shared = Arc::clone(&shared);
        let worker_state = Arc::clone(&state);
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        let worker = thread::Builder::new()
            .name("slicer-native-player".to_owned())
            .spawn(move || {
                match PlayerWorker::initialize(
                    api,
                    window_id,
                    render,
                    worker_shared.clone(),
                    worker_state,
                ) {
                    Ok(worker) => {
                        let _ = ready_tx.send(Ok(worker.handle as usize));
                        worker.run(receiver);
                    }
                    Err(error) => {
                        let _ = ready_tx.send(Err(error));
                    }
                }
            })
            .map_err(|error| format!("unable to start libmpv worker: {error}"))?;

        let ready_began = Instant::now();
        match ready_rx.recv_timeout(Duration::from_secs(3)) {
            Ok(Ok(render_handle)) => {
                startup_timing("worker_ready_wait", ready_began);
                Ok(Self {
                    shared,
                    state,
                    worker: Some(worker),
                    window_id,
                    render_handle,
                })
            }
            Ok(Err(error)) => {
                shared.shutdown.store(true, Ordering::Release);
                let _ = worker.join();
                Err(error)
            }
            Err(error) => {
                shared.shutdown.store(true, Ordering::Release);
                let _ = worker.join();
                Err(format!("libmpv worker startup timed out: {error}"))
            }
        }
    }

    /// Return the native child-window id used by libmpv's wid option.
    pub fn window_id(&self) -> u64 {
        self.window_id
    }

    /// Load a file without setting a stopping range.
    pub fn load(&self, path: impl AsRef<Path>) -> Result<(), String> {
        self.queue_load(path.as_ref(), 0.0, None)
    }

    /// Load an externally timed source directly at its first needed frame.
    pub(crate) fn load_at(&self, path: &Path, start: f64) -> Result<(), String> {
        self.queue_load(path, start, None)
    }

    /// Load a file, beginning at start and pausing at end.
    pub fn load_file(&self, path: impl AsRef<Path>, start: f64, end: f64) -> Result<(), String> {
        validate_range(start, end)?;
        self.queue_load(path.as_ref(), start, Some(end))
    }

    /// Set the active trim range for an already loaded file.
    pub fn set_range(&self, start: f64, end: f64) -> Result<(), String> {
        validate_range(start, end)?;
        self.with_controls(|controls| controls.range = Some(Range { start, end }))
    }

    /// Stop playback when the current position reaches end.
    pub fn stop_at(&self, end: f64) -> Result<(), String> {
        if !end.is_finite() || end <= 0.0 {
            return Err("Playback stop time must be a positive finite number".to_owned());
        }
        self.with_controls(|controls| {
            controls.range = Some(Range { start: 0.0, end });
        })
    }

    /// Pause or resume playback. The requested state is reflected immediately
    /// in the snapshot; libmpv applies it on the worker thread.
    pub fn set_paused(&self, paused: bool) -> Result<(), String> {
        self.state.paused.store(paused, Ordering::Release);
        self.with_controls(|controls| controls.paused = Some(paused))
    }

    /// Small clock correction for externally mixed multitrack audio; never seeks.
    pub(crate) fn set_clock_speed(&self, speed: f64) -> Result<(), String> {
        if !speed.is_finite() || !(0.95..=1.05).contains(&speed) {
            return Err("Invalid video clock correction".into());
        }
        self.with_controls(|controls| controls.speed = Some(speed))
    }

    /// Mute or unmute native audio output.
    pub fn set_mute(&self, muted: bool) -> Result<(), String> {
        self.state.muted.store(muted, Ordering::Release);
        self.with_controls(|controls| controls.muted = Some(muted))
    }

    /// Set the GPU presentation crop in source pixels; None restores the full image.
    pub fn set_crop(&self, crop: Option<(u32, u32, u32, u32)>) -> Result<(), String> {
        let value = crop
            .map(|(x, y, width, height)| format!("{width}x{height}+{x}+{y}"))
            .unwrap_or_default();
        self.with_controls(|controls| controls.crop = Some(value))
    }

    /// Request a seek. Repeated drag requests replace the pending request and
    /// are paced on the worker so the decoder is not flooded. Both drag and
    /// release requests use high-resolution seeks; `exact` marks the release
    /// request, which bypasses drag pacing and is always the newest target.
    pub fn seek(&self, seconds: f64, exact: bool) -> Result<(), String> {
        if !seconds.is_finite() || seconds < 0.0 {
            return Err("Seek time must be a nonnegative finite number".to_owned());
        }
        let serial = self
            .shared
            .next_seek_serial
            .fetch_add(1, Ordering::AcqRel)
            .wrapping_add(1);
        if let Ok(mut pending) = self.shared.pending_seek.lock() {
            *pending = Some(SeekRequest {
                seconds,
                exact,
                serial,
            });
        } else {
            return Err("native player seek queue is unavailable".to_owned());
        }
        self.state.seeking.store(true, Ordering::Release);
        self.wake()
    }

    /// Read the current state without waiting on libmpv.
    pub fn snapshot(&self) -> NativePlayerSnapshot {
        self.state.snapshot()
    }

    /// Poll is an explicit alias for snapshot for UI event loops.
    pub fn poll(&self) -> NativePlayerSnapshot {
        self.snapshot()
    }

    /// Whether the worker still owns a live libmpv handle.
    pub fn is_running(&self) -> bool {
        !self.shared.shutdown.load(Ordering::Acquire)
            && self
                .worker
                .as_ref()
                .is_some_and(|worker| !worker.is_finished())
    }

    fn queue_load(&self, path: &Path, start: f64, end: Option<f64>) -> Result<(), String> {
        if !start.is_finite() || start < 0.0 {
            return Err("Playback start time must be a nonnegative finite number".to_owned());
        }
        let metadata = std::fs::metadata(path)
            .map_err(|error| format!("unable to read media file {}: {error}", path.display()))?;
        if !metadata.is_file() {
            return Err(format!(
                "media input is not a regular file: {}",
                path.display()
            ));
        }
        let path = path_to_cstring(path.as_os_str())?;
        // A seek request is kept in a separate coalescing slot from the load
        // command. Drop a target captured for the previous file before the
        // replacement is handed to the worker; requests made after this point
        // belong to the new file.
        if let Ok(mut pending) = self.shared.pending_seek.lock() {
            *pending = None;
        } else {
            return Err("native player seek queue is unavailable".to_owned());
        }
        self.state.clear_for_load(start);
        self.try_send(WorkerCommand::Load { path, start, end })
    }

    fn with_controls(&self, update: impl FnOnce(&mut PendingControls)) -> Result<(), String> {
        if let Ok(mut controls) = self.shared.pending_controls.lock() {
            update(&mut controls);
        } else {
            return Err("native player command queue is unavailable".to_owned());
        }
        self.wake()
    }

    fn wake(&self) -> Result<(), String> {
        if self.shared.wake_pending.swap(true, Ordering::AcqRel) {
            self.shared.wake_mpv();
            return Ok(());
        }
        let result = match self.shared.commands.try_send(WorkerCommand::Wake) {
            Ok(()) => Ok(()),
            Err(TrySendError::Full(_)) => {
                // The worker will drain the existing command and poll the
                // pending slots on its next bounded iteration. Do not leave
                // the coalescing bit set when no wake token was queued.
                self.shared.wake_pending.store(false, Ordering::Release);
                Ok(())
            }
            Err(TrySendError::Disconnected(_)) => {
                self.shared.wake_pending.store(false, Ordering::Release);
                Err("native player has stopped".to_owned())
            }
        };
        self.shared.wake_mpv();
        result
    }

    fn try_send(&self, command: WorkerCommand) -> Result<(), String> {
        match self.shared.commands.try_send(command) {
            Ok(()) => Ok(()),
            Err(TrySendError::Full(_)) => {
                Err("native player command queue is full; try again shortly".to_owned())
            }
            Err(TrySendError::Disconnected(_)) => Err("native player has stopped".to_owned()),
        }
    }
}

impl Drop for NativePlayer {
    fn drop(&mut self) {
        // Set the flag first so a full command queue cannot keep the worker
        // alive. Wake mpv directly as well: some private builds can remain in
        // their event condition variable beyond the nominal wait timeout.
        // The worker clears the wakeup pair before destroying the handle.
        self.shared.shutdown.store(true, Ordering::Release);
        self.shared.wake_mpv();
        let _ = self.shared.commands.try_send(WorkerCommand::Shutdown);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

struct PlayerWorker {
    api: MpvApi,
    handle: *mut MpvHandle,
    shared: Arc<Shared>,
    state: Arc<SharedState>,
    range: Option<Range>,
    pending_start: Option<f64>,
    eof_latched: bool,
    replay_pending: bool,
    last_seek_serial: u64,
    last_approximate_seek: Option<Instant>,
    seek_in_flight: Option<SeekRequest>,
    seek_started: Option<Instant>,
    next_command_userdata: u64,
    pending_commands: HashMap<u64, PendingCommand>,
    last_pause_command: Option<bool>,
    last_mute_command: Option<bool>,
}

// The handle is created and destroyed by this worker. It never crosses back
// to the public player or gets accessed concurrently.
unsafe impl Send for PlayerWorker {}

impl PlayerWorker {
    fn initialize(
        api: MpvApi,
        window_id: u64,
        render: bool,
        shared: Arc<Shared>,
        state: Arc<SharedState>,
    ) -> Result<Self, String> {
        let handle = unsafe { (api.create)() };
        if handle.is_null() {
            return Err("libmpv could not create a player handle".to_owned());
        }

        let setup_began = Instant::now();
        let setup = Self::configure(&api, handle, window_id, render).and_then(|()| {
            let code = unsafe { (api.initialize)(handle) };
            if code < 0 {
                Err(format!("libmpv initialization failed: {}", unsafe {
                    api.error(code)
                }))
            } else {
                Ok(())
            }
        });
        startup_timing("configure_initialize", setup_began);
        if let Err(error) = setup {
            unsafe { (api.terminate_destroy)(handle) };
            return Err(error);
        }

        let mut worker = Self {
            api,
            handle,
            shared,
            state,
            range: None,
            pending_start: None,
            eof_latched: false,
            replay_pending: false,
            last_seek_serial: 0,
            last_approximate_seek: None,
            seek_in_flight: None,
            seek_started: None,
            next_command_userdata: 1,
            pending_commands: HashMap::new(),
            last_pause_command: None,
            last_mute_command: None,
        };
        worker.observe_properties();
        if let Some(wakeup) = worker.api.wakeup {
            worker
                .shared
                .set_mpv_waker(Some(MpvWaker { wakeup, handle }));
        }
        Ok(worker)
    }

    fn configure(
        api: &MpvApi,
        handle: *mut MpvHandle,
        window_id: u64,
        render: bool,
    ) -> Result<(), String> {
        let basic = [
            ("config", "no"),
            ("idle", "yes"),
            ("terminal", "no"),
            ("osd-level", "0"),
            ("input-default-bindings", "no"),
            ("input-cursor", "no"),
            ("pause", "yes"),
            ("keep-open", "yes"),
            ("audio-client-name", "Slicer"),
            // Local recordings do not need mpv's large streaming cache. Keep
            // demuxer and decoder allocations bounded so opening a short,
            // high-resolution clip does not reserve hundreds of megabytes.
            ("cache", "no"),
            ("demuxer-max-bytes", "16MiB"),
            ("demuxer-max-back-bytes", "4MiB"),
            ("demuxer-readahead-secs", "1"),
            // Auto-sized decoder thread pools can reserve a large per-thread
            // frame queue on machines with many cores. Two video workers are
            // enough for smooth preview while keeping the memory footprint
            // predictable; audio decoding is effectively single-threaded for
            // the formats Slicer exports.
            ("vd-lavc-threads", "2"),
            ("ad-lavc-threads", "1"),
        ];
        for (name, value) in basic {
            set_option(api, handle, name, value)?;
        }

        if let Some(log_path) = std::env::var_os("SLICER_MPV_LOG") {
            let log_path = path_to_cstring(OsStr::new(&log_path))?;
            set_option_cstring(api, handle, "log-file", &log_path)?;
        }

        if render {
            set_option(api, handle, "vo", "libmpv")?;
            set_option(api, handle, "aid", "no")?;
            set_option(api, handle, "keepaspect", "no")?;
            set_option(api, handle, "video-sync", "audio")?;
            set_option(api, handle, "video-timing-offset", "0")?;
            set_option(
                api,
                handle,
                "hwdec",
                &std::env::var("SLICER_MPV_HWDEC").unwrap_or("nvdec,vaapi".into()),
            )?;
            // Render every source into a common SDR sRGB space for the compositor.
            set_option(api, handle, "target-prim", "bt.709")?;
            set_option(api, handle, "target-trc", "srgb")?;
        } else if window_id == 0 {
            // Zero is useful for headless integration tests and package
            // diagnostics. Production UI supplies a real X11 child id.
            set_option(api, handle, "vo", "null")?;
            set_option(api, handle, "ao", "null")?;
            set_option(api, handle, "hwdec", "no")?;
        } else {
            #[cfg(target_os = "linux")]
            set_option(api, handle, "wid", &window_id.to_string())?;

            // Keep libmpv on its lightweight OpenGL presentation path on
            // Linux. Vulkan is excellent for the GPUI scene, but asking mpv
            // to create a second Vulkan device in the same process causes the
            // NVIDIA driver to map another large allocator and shader stack.
            // OpenGL still presents directly to the native X11 child (there
            // are no CPU frame copies) while avoiding that duplicate Vulkan
            // allocation. An explicit override is available for diagnostics
            // and for platforms with a different native video backend.
            let gpu_api = std::env::var("SLICER_MPV_GPU_API").unwrap_or_else(|_| {
                if cfg!(target_os = "linux") {
                    "opengl".to_owned()
                } else {
                    String::new()
                }
            });
            if !gpu_api.is_empty() {
                set_option(api, handle, "gpu-api", &gpu_api).map_err(|error| {
                    format!("SLICER_MPV_GPU_API={gpu_api:?} is not supported by libmpv: {error}")
                })?;
            }

            // mpv treats a comma-separated vo value as a priority list, so
            // backend initialization can fall through from gpu-next to gpu.
            // Neither path creates CPU frames or a PNG preview. Older private
            // builds that reject a list get the explicit gpu fallback.
            if set_option(api, handle, "vo", "gpu-next,gpu").is_err()
                && set_option(api, handle, "vo", "gpu").is_err()
            {
                return Err("libmpv could not configure gpu-next or gpu video output".to_owned());
            }
            if let Some(hwdec) = std::env::var_os("SLICER_MPV_HWDEC") {
                let hwdec = hwdec.to_string_lossy();
                set_option(api, handle, "hwdec", &hwdec).map_err(|error| {
                    format!("SLICER_MPV_HWDEC={hwdec:?} is not supported by libmpv: {error}")
                })?;
            // Vulkan Video decoding stalled during repeated seeks on the
            // tested NVIDIA driver. Prefer the established native decoders;
            // mpv falls back to software decoding for unsupported hardware.
            // GPU presentation remains enabled in either case.
            } else if set_option(api, handle, "hwdec", "nvdec,vaapi").is_err() {
                set_option(api, handle, "hwdec", "no")?;
            }
        }
        Ok(())
    }

    fn observe_properties(&mut self) {
        let properties = [
            (1, "time-pos", MPV_FORMAT_DOUBLE),
            (2, "duration", MPV_FORMAT_DOUBLE),
            (3, "pause", MPV_FORMAT_FLAG),
            (4, "eof-reached", MPV_FORMAT_FLAG),
            (5, "hwdec-current", MPV_FORMAT_STRING),
            (6, "current-vo", MPV_FORMAT_STRING),
            (7, "video-params/w", MPV_FORMAT_INT64),
            (8, "video-params/h", MPV_FORMAT_INT64),
            (9, "display-fps", MPV_FORMAT_DOUBLE),
            (10, "estimated-vf-fps", MPV_FORMAT_DOUBLE),
            (11, "audio-codec-name", MPV_FORMAT_STRING),
            (12, "audio-params", MPV_FORMAT_NODE),
            (13, "audio-active", MPV_FORMAT_FLAG),
            (14, "decoder-frame-drop-count", MPV_FORMAT_INT64),
            (15, "seeking", MPV_FORMAT_FLAG),
            (16, "mute", MPV_FORMAT_FLAG),
        ];
        for (userdata, name, format) in properties {
            if let Ok(name) = CString::new(name) {
                let code = unsafe {
                    (self.api.observe_property)(self.handle, userdata, name.as_ptr(), format)
                };
                if code < 0 {
                    // Optional properties may be unavailable on an older
                    // private libmpv; core playback remains usable.
                    continue;
                }
            }
        }
    }

    fn run(mut self, receiver: Receiver<WorkerCommand>) {
        loop {
            self.drain_commands(&receiver);
            if self.shared.shutdown.load(Ordering::Acquire) {
                break;
            }
            self.apply_pending_controls();
            self.apply_pending_seek();
            if self.shared.shutdown.load(Ordering::Acquire) {
                break;
            }

            let event = unsafe { (self.api.wait_event)(self.handle, EVENT_WAIT_SECONDS) };
            if !event.is_null() {
                self.handle_event(event);
            }

            // Drain without waiting so a busy file cannot overflow the client
            // ringbuffer. Cap one pass so command slots and range enforcement
            // get service even when a private runtime emits a property event
            // for every decoded frame.
            for _ in 0..MAX_EVENTS_PER_TICK {
                let event = unsafe { (self.api.wait_event)(self.handle, 0.0) };
                if event.is_null() {
                    break;
                }
                let event_id = unsafe { (*event).event_id };
                if event_id == MPV_EVENT_NONE {
                    break;
                }
                self.handle_event(event);
                if self.shared.shutdown.load(Ordering::Acquire) {
                    break;
                }
            }
            self.enforce_range();
        }

        // Do not leave a callable wakeup pair pointing at a handle that is
        // about to be destroyed. NativePlayer::drop may race this cleanup
        // while waiting for the worker to finish.
        self.shared.set_mpv_waker(None);
        unsafe { (self.api.terminate_destroy)(self.handle) };
        self.state.loaded.store(false, Ordering::Release);
    }

    fn drain_commands(&mut self, receiver: &Receiver<WorkerCommand>) {
        loop {
            match receiver.try_recv() {
                Ok(WorkerCommand::Load { path, start, end }) => self.load_file(path, start, end),
                Ok(WorkerCommand::Shutdown) => {
                    self.shared.shutdown.store(true, Ordering::Release);
                }
                Ok(WorkerCommand::Wake) => {
                    self.shared.wake_pending.store(false, Ordering::Release);
                }
                Err(TryRecvError::Empty | TryRecvError::Disconnected) => break,
            }
        }
    }

    fn apply_pending_controls(&mut self) {
        let controls = if let Ok(mut controls) = self.shared.pending_controls.lock() {
            let mut next = PendingControls::default();
            std::mem::swap(&mut *controls, &mut next);
            next
        } else {
            return;
        };

        if let Some(speed) = controls.speed {
            if let Err(error) = self.command_async(
                &["set", "speed", &format!("{speed:.6}")],
                PendingCommand::Speed,
            ) {
                self.state.set_error(error);
            }
        }
        if let Some(crop) = controls.crop
            && let Err(error) =
                self.command_async(&["set", "video-crop", &crop], PendingCommand::Crop)
        {
            self.state.set_error(error);
        }
        if let Some(range) = controls.range {
            self.range = Some(range);
        }
        if let Some(paused) = controls.paused {
            if !paused {
                // A range stop and natural EOF both leave mpv paused at the
                // terminal frame. Resuming from that state must begin at the
                // selected start; otherwise mpv can report a transient
                // unpaused state while immediately jumping back to EOF.
                let has_pending_seek = self
                    .shared
                    .pending_seek
                    .lock()
                    .ok()
                    .is_some_and(|pending| pending.is_some());
                if (self.eof_latched || self.state.eof.load(Ordering::Acquire)) && !has_pending_seek
                {
                    self.restart_from_boundary();
                }
            }
            if paused {
                // A user pause supersedes an automatic replay request that has
                // not presented its first frame yet.
                self.replay_pending = false;
            }
            self.set_pause_command(paused);
        }
        if let Some(muted) = controls.muted {
            self.set_mute_command(muted);
        }
    }

    fn apply_pending_seek(&mut self) {
        let pending_is_exact = if let Ok(pending) = self.shared.pending_seek.lock() {
            pending.as_ref().is_some_and(|request| request.exact)
        } else {
            return;
        };

        // Keep only one drag seek in flight. Once mpv reports that seek as
        // settled, the newest pending target is dispatched. A private runtime
        // may omit the `seeking` property, so an approximate request has a
        // bounded fallback timeout; exact release requests wait for their
        // completion instead of overlapping decoder work.
        if let Some(in_flight) = self.seek_in_flight {
            let seeking = self.state.seeking.load(Ordering::Acquire);
            let timed_out = self.seek_started.is_some_and(|started| {
                started.elapsed()
                    >= if in_flight.exact {
                        EXACT_SEEK_SETTLE_TIMEOUT
                    } else {
                        DRAG_SEEK_SETTLE_TIMEOUT
                    }
            });
            if !seeking || timed_out {
                self.seek_in_flight = None;
                self.seek_started = None;
            } else {
                // Do not enqueue a release seek behind an unfinished drag
                // seek. mpv's async command queue preserves order, but two
                // simultaneous high-resolution decoder restarts can keep the
                // VO busy indefinitely on long-GOP hardware-decoded streams.
                // The pending exact request remains latest-wins and is sent as
                // soon as the current restart reports settled (or times out).
                return;
            }
        }

        if !pending_is_exact
            && self
                .last_approximate_seek
                .is_some_and(|sent| sent.elapsed() < DRAG_SEEK_INTERVAL)
        {
            return;
        }

        let request = if let Ok(mut pending) = self.shared.pending_seek.lock() {
            pending.take()
        } else {
            None
        };
        let Some(request) = request else { return };
        if request.serial <= self.last_seek_serial {
            return;
        }
        let was_at_end = self.eof_latched || self.state.eof.load(Ordering::Acquire);
        let boundary = self.boundary_position();
        self.last_seek_serial = request.serial;
        self.eof_latched = false;
        self.state.eof.store(false, Ordering::Release);
        self.replay_pending =
            was_at_end && boundary.is_some_and(|boundary| request.seconds < boundary - 0.015);
        let seconds = format!("{:.6}", request.seconds);
        // Keyframe seeks make long-GOP/VFR recordings visibly jump between
        // sparse keyframes. Use exact/hr seeks for drag
        // previews as well, while the pending slot and cadence above bound
        // how often the decoder is asked to restart.
        if let Err(error) = self.command_async(
            &["seek", &seconds, "absolute", "exact"],
            PendingCommand::Seek(request),
        ) {
            self.state.set_error(error);
            self.state.seeking.store(false, Ordering::Release);
            self.seek_in_flight = None;
            self.seek_started = None;
            return;
        }
        let now = Instant::now();
        self.seek_in_flight = Some(request);
        self.seek_started = Some(now);
        if !request.exact {
            self.last_approximate_seek = Some(now);
        }
    }

    fn restart_from_boundary(&mut self) {
        let start = self.range.as_ref().map(|range| range.start).unwrap_or(0.0);
        // Replay is an internal seek, but it must consume the same serial
        // stream as public seek requests. Advancing only the worker-local
        // value would make the next UI seek compare equal and get discarded as
        // stale by apply_pending_seek().
        let serial = self
            .shared
            .next_seek_serial
            .fetch_add(1, Ordering::AcqRel)
            .wrapping_add(1);
        self.last_seek_serial = serial;
        let request = SeekRequest {
            seconds: start,
            exact: true,
            serial,
        };
        let seconds = format!("{start:.6}");
        self.eof_latched = false;
        self.state.eof.store(false, Ordering::Release);
        self.replay_pending = true;
        self.state.seeking.store(true, Ordering::Release);
        if let Err(error) = self.command_async(
            &["seek", &seconds, "absolute", "exact"],
            PendingCommand::Seek(request),
        ) {
            self.state.set_error(error);
            self.state.seeking.store(false, Ordering::Release);
            return;
        }
        self.seek_in_flight = Some(request);
        self.seek_started = Some(Instant::now());
    }

    fn load_file(&mut self, path: CString, start: f64, end: Option<f64>) {
        self.range = end.map(|end| Range { start, end });
        self.pending_start = (start > 0.0).then_some(start);
        self.eof_latched = false;
        self.replay_pending = false;
        self.last_approximate_seek = None;
        self.seek_in_flight = None;
        self.seek_started = None;
        // mpv may change pause while replacing a file or reaching EOF. Do not
        // let the command sent for the previous file suppress the initial
        // paused state for this load.
        self.last_pause_command = None;
        self.state.clear_for_load(start);
        // Keep the loadfile command compatible with mpv versions before and
        // after 0.38. Those versions disagree about the optional playlist
        // index argument. The exact initial seek is issued after FILE_LOADED,
        // when the file is ready, so the filename remains one literal argument.
        let command = CString::new("loadfile").expect("static command");
        let replace = CString::new("replace").expect("static command");
        if let Err(error) = self.command_async_cstrings(
            &[command.as_c_str(), path.as_c_str(), replace.as_c_str()],
            PendingCommand::Load,
        ) {
            self.state.set_error(error);
            self.state.loaded.store(false, Ordering::Release);
            self.pending_start = None;
        } else {
            // Keep replacement loads paused until the caller explicitly
            // resumes transport. This command follows loadfile in mpv's
            // command queue and also establishes a fresh dedup baseline.
            self.set_pause_command(true);
        }
    }

    fn enforce_range(&mut self) {
        if self.replay_pending {
            // The old terminal time-pos can remain visible until the replay
            // seek presents its first frame. Never relatch EOF from that stale
            // sample while the new playback is in flight.
            return;
        }
        let Some(boundary) = self.boundary_position() else {
            return;
        };
        let range = self.range.as_ref();
        let position = f64::from_bits(self.state.position.load(Ordering::Acquire));
        let paused = self.state.paused.load(Ordering::Acquire);
        let start = range.map(|range| range.start).unwrap_or(0.0);
        let tolerance = range.map_or(0.001, |_| 0.015);
        let reached = position.is_finite() && position >= start && position >= boundary - tolerance;
        // With keep-open enabled, mpv can stop at the natural duration without
        // emitting END_FILE and while already reporting pause=true. Treat that
        // exact duration as EOF too; a manually paused frame before the end is
        // still left alone.
        if reached && (!paused || range.is_none() || position >= boundary) {
            self.state
                .position
                .store(boundary.to_bits(), Ordering::Release);
            self.state.eof.store(true, Ordering::Release);
            self.eof_latched = true;
            if !paused {
                self.set_pause_command(true);
            }
            self.state.paused.store(true, Ordering::Release);
            self.state.seeking.store(false, Ordering::Release);
        }
    }

    fn boundary_position(&self) -> Option<f64> {
        let duration = f64::from_bits(self.state.duration.load(Ordering::Acquire));
        let configured_end = self.range.as_ref().map(|range| range.end);
        match (configured_end, duration) {
            (Some(end), duration) if end.is_finite() && end > 0.0 => {
                Some(if duration.is_finite() && duration > 0.0 {
                    end.min(duration)
                } else {
                    end
                })
            }
            (None, duration) if duration.is_finite() && duration > 0.0 => Some(duration),
            _ => None,
        }
    }

    fn set_pause_command(&mut self, paused: bool) {
        if self.last_pause_command == Some(paused) {
            return;
        }
        let value = if paused { "yes" } else { "no" };
        if let Err(error) =
            self.command_async(&["set", "pause", value], PendingCommand::Pause(paused))
        {
            self.state.set_error(error);
        } else {
            self.last_pause_command = Some(paused);
        }
    }

    fn set_mute_command(&mut self, muted: bool) {
        if self.last_mute_command == Some(muted) {
            return;
        }
        let value = if muted { "yes" } else { "no" };
        if let Err(error) = self.command_async(&["set", "mute", value], PendingCommand::Mute(muted))
        {
            self.state.set_error(error);
        } else {
            self.last_mute_command = Some(muted);
        }
    }

    fn command_async(&mut self, args: &[&str], pending: PendingCommand) -> Result<u64, String> {
        let mut owned = Vec::with_capacity(args.len());
        for arg in args {
            owned.push(
                CString::new(*arg)
                    .map_err(|_| "libmpv command argument contains NUL".to_owned())?,
            );
        }
        let args = owned.iter().map(|arg| arg.as_c_str()).collect::<Vec<_>>();
        self.command_async_cstrings(&args, pending)
    }

    fn command_async_cstrings(
        &mut self,
        args: &[&CStr],
        pending: PendingCommand,
    ) -> Result<u64, String> {
        let mut pointers = args.iter().map(|arg| arg.as_ptr()).collect::<Vec<_>>();
        pointers.push(ptr::null());
        let reply_userdata = self.next_reply_userdata();
        let code =
            unsafe { (self.api.command_async)(self.handle, reply_userdata, pointers.as_ptr()) };
        if code < 0 {
            return Err(format!("libmpv command failed: {}", unsafe {
                self.api.error(code)
            }));
        }
        self.pending_commands.insert(reply_userdata, pending);
        Ok(reply_userdata)
    }

    fn next_reply_userdata(&mut self) -> u64 {
        loop {
            let value = self.next_command_userdata;
            self.next_command_userdata = self.next_command_userdata.wrapping_add(1);
            if value != 0 && !self.pending_commands.contains_key(&value) {
                return value;
            }
        }
    }

    fn handle_command_reply(&mut self, reply_userdata: u64, event_error: c_int) {
        let Some(command) = self.pending_commands.remove(&reply_userdata) else {
            // Replies from commands issued by another client or commands that
            // completed while a replacement load was being torn down are not
            // actionable for this worker.
            return;
        };
        if event_error >= 0 {
            return;
        }

        let error = unsafe { self.api.error(event_error) };
        match command {
            PendingCommand::Speed => self
                .state
                .set_error(format!("Video clock correction failed: {error}")),
            PendingCommand::Crop => self
                .state
                .set_error(format!("Could not crop preview: {error}")),
            PendingCommand::Load => {
                self.state
                    .set_error(format!("libmpv load command failed: {error}"));
                self.state.loaded.store(false, Ordering::Release);
                self.pending_start = None;
                self.seek_in_flight = None;
                self.seek_started = None;
                self.state.seeking.store(false, Ordering::Release);
            }
            PendingCommand::Seek(request) => {
                // A superseded drag seek can fail after the release seek has
                // already been accepted. Only expose and clear an error for
                // the currently active request.
                let current = self.seek_in_flight.is_some_and(|active| {
                    active.serial == request.serial
                        && active.seconds.to_bits() == request.seconds.to_bits()
                        && active.exact == request.exact
                });
                if current {
                    self.state
                        .set_error(format!("libmpv seek command failed: {error}"));
                    self.seek_in_flight = None;
                    self.seek_started = None;
                    self.state.seeking.store(false, Ordering::Release);
                }
            }
            PendingCommand::Pause(value) => {
                if self.last_pause_command == Some(value) {
                    self.last_pause_command = None;
                    self.state
                        .set_error(format!("libmpv pause command failed: {error}"));
                }
            }
            PendingCommand::Mute(value) => {
                if self.last_mute_command == Some(value) {
                    self.last_mute_command = None;
                    self.state
                        .set_error(format!("libmpv mute command failed: {error}"));
                }
            }
        }
    }

    fn handle_event(&mut self, event: *mut MpvEvent) {
        let (event_id, event_error, data) =
            unsafe { ((*event).event_id, (*event).error, (*event).data) };
        match event_id {
            MPV_EVENT_COMMAND_REPLY => {
                self.handle_command_reply(unsafe { (*event).reply_userdata }, event_error);
            }
            MPV_EVENT_PROPERTY_CHANGE => {
                if !data.is_null() {
                    self.handle_property(data as *const MpvEventProperty);
                }
            }
            MPV_EVENT_START_FILE => {
                self.state.loaded.store(false, Ordering::Release);
                self.state.eof.store(false, Ordering::Release);
                self.eof_latched = false;
                self.replay_pending = false;
                self.seek_in_flight = None;
                self.seek_started = None;
            }
            MPV_EVENT_FILE_LOADED => {
                self.state.loaded.store(true, Ordering::Release);
                self.state.eof.store(false, Ordering::Release);
                self.eof_latched = false;
                self.replay_pending = false;
                self.state.set_string(&self.state.error, None);
                if let Some(start) = self.pending_start.take() {
                    let seconds = format!("{start:.6}");
                    self.state.seeking.store(true, Ordering::Release);
                    let request = SeekRequest {
                        seconds: start,
                        exact: true,
                        serial: self.last_seek_serial,
                    };
                    if let Err(error) = self.command_async(
                        &["seek", &seconds, "absolute", "exact"],
                        PendingCommand::Seek(request),
                    ) {
                        self.state.set_error(error);
                        self.state.seeking.store(false, Ordering::Release);
                    } else {
                        self.seek_in_flight = Some(request);
                        self.seek_started = Some(Instant::now());
                    }
                }
            }
            MPV_EVENT_SEEK => self.state.seeking.store(true, Ordering::Release),
            MPV_EVENT_PLAYBACK_RESTART => {
                self.state.seeking.store(false, Ordering::Release);
                self.seek_in_flight = None;
                self.seek_started = None;
                self.eof_latched = false;
                self.state.eof.store(false, Ordering::Release);
            }
            MPV_EVENT_END_FILE => {
                self.seek_in_flight = None;
                self.seek_started = None;
                let (reason, error) = if data.is_null() {
                    (0, event_error)
                } else {
                    let end_file = unsafe { &*(data as *const MpvEventEndFile) };
                    (end_file.reason, end_file.error)
                };
                // Reason 0 is EOF. Keep the file marked loaded so the last
                // rendered GPU frame remains visible while transport is stopped.
                if reason == 0 && !self.replay_pending {
                    if let Some(boundary) = self.boundary_position() {
                        self.state
                            .position
                            .store(boundary.to_bits(), Ordering::Release);
                    }
                    self.eof_latched = true;
                    self.state.eof.store(true, Ordering::Release);
                    self.state.paused.store(true, Ordering::Release);
                    self.state.seeking.store(false, Ordering::Release);
                    self.last_pause_command = Some(true);
                }
                if error < 0 {
                    self.state
                        .set_error(format!("libmpv playback ended: {}", unsafe {
                            self.api.error(error)
                        }));
                    self.state.paused.store(true, Ordering::Release);
                    self.last_pause_command = Some(true);
                }
            }
            MPV_EVENT_SHUTDOWN => {
                self.shared.shutdown.store(true, Ordering::Release);
                self.state.loaded.store(false, Ordering::Release);
                self.seek_in_flight = None;
                self.seek_started = None;
            }
            _ => {}
        }
    }

    fn handle_property(&mut self, property: *const MpvEventProperty) {
        let property = unsafe { &*property };
        if property.name.is_null() || property.data.is_null() {
            return;
        }
        let name = unsafe { CStr::from_ptr(property.name).to_bytes() };
        match name {
            b"time-pos" if property.format == MPV_FORMAT_DOUBLE => {
                let value = unsafe { *(property.data as *const f64) };
                if value.is_finite() && value >= 0.0 && self.replay_pending {
                    let before_boundary = self
                        .boundary_position()
                        .is_none_or(|boundary| value < boundary - 0.015);
                    if before_boundary {
                        self.replay_pending = false;
                        self.eof_latched = false;
                        self.state.eof.store(false, Ordering::Release);
                    } else {
                        return;
                    }
                }
                if !self.eof_latched && value.is_finite() && value >= 0.0 {
                    self.state
                        .position
                        .store(value.to_bits(), Ordering::Release);
                }
            }
            b"duration" if property.format == MPV_FORMAT_DOUBLE => {
                let value = unsafe { *(property.data as *const f64) };
                if value.is_finite() && value >= 0.0 {
                    self.state
                        .duration
                        .store(value.to_bits(), Ordering::Release);
                }
            }
            b"pause" if property.format == MPV_FORMAT_FLAG => {
                let value = unsafe { *(property.data as *const c_int) != 0 };
                if self.replay_pending && value {
                    // Ignore the stale paused value from the old EOF while
                    // the restart seek is still presenting its first frame.
                    return;
                }
                if self.eof_latched && !value {
                    // keep-open and a few mpv versions can emit a stale
                    // unpaused property after END_FILE. The terminal state is
                    // authoritative until a new seek or resume clears it.
                    return;
                }
                self.state.paused.store(value, Ordering::Release);
                // EOF, a replacement load, and mpv's own transport handling
                // can change pause without a command from this worker. Keep
                // the deduplication baseline in sync with the observed value
                // so a later play/pause request is never suppressed by a
                // stale command from the previous file.
                self.last_pause_command = Some(value);
            }
            b"eof-reached" if property.format == MPV_FORMAT_FLAG => {
                let value = unsafe { *(property.data as *const c_int) != 0 };
                if value {
                    if self.replay_pending {
                        return;
                    }
                    self.eof_latched = true;
                    if let Some(boundary) = self.boundary_position() {
                        self.state
                            .position
                            .store(boundary.to_bits(), Ordering::Release);
                    }
                    self.state.paused.store(true, Ordering::Release);
                } else if !self.eof_latched {
                    self.state.eof.store(false, Ordering::Release);
                }
            }
            b"seeking" if property.format == MPV_FORMAT_FLAG => {
                let seeking = unsafe { *(property.data as *const c_int) != 0 };
                self.state.seeking.store(seeking, Ordering::Release);
                if !seeking {
                    self.seek_in_flight = None;
                    self.seek_started = None;
                }
            }
            b"hwdec-current" if property.format == MPV_FORMAT_STRING => {
                self.state
                    .set_string(&self.state.hwdec, property_string(property.data));
            }
            b"current-vo" if property.format == MPV_FORMAT_STRING => {
                self.state
                    .set_string(&self.state.video_output, property_string(property.data));
            }
            b"video-params/w" if property.format == MPV_FORMAT_INT64 => {
                self.state.video_width.store(
                    unsafe { *(property.data as *const i64) }.max(0) as u64,
                    Ordering::Release,
                );
            }
            b"video-params/h" if property.format == MPV_FORMAT_INT64 => {
                self.state.video_height.store(
                    unsafe { *(property.data as *const i64) }.max(0) as u64,
                    Ordering::Release,
                );
            }
            b"display-fps" if property.format == MPV_FORMAT_DOUBLE => {
                let value = unsafe { *(property.data as *const f64) };
                if value.is_finite() && value > 0.0 {
                    self.state
                        .display_fps
                        .store(value.to_bits(), Ordering::Release);
                }
            }
            b"estimated-vf-fps" if property.format == MPV_FORMAT_DOUBLE => {
                let value = unsafe { *(property.data as *const f64) };
                if value.is_finite() && value > 0.0 {
                    self.state
                        .estimated_vf_fps
                        .store(value.to_bits(), Ordering::Release);
                }
            }
            b"audio-codec-name" if property.format == MPV_FORMAT_STRING => {
                self.state
                    .set_string(&self.state.audio_codec, property_string(property.data));
            }
            b"audio-params" if property.format == MPV_FORMAT_NODE => {
                let value = unsafe { node_to_string(property.data as *const MpvNode, 0) };
                self.state.set_string(&self.state.audio_params, value);
            }
            b"audio-active" if property.format == MPV_FORMAT_FLAG => {
                let value = unsafe { *(property.data as *const c_int) != 0 };
                self.state
                    .audio_active
                    .store(if value { 1 } else { 2 }, Ordering::Release);
            }
            b"mute" if property.format == MPV_FORMAT_FLAG => {
                let value = unsafe { *(property.data as *const c_int) != 0 };
                self.state.muted.store(value, Ordering::Release);
                self.last_mute_command = Some(value);
            }
            b"decoder-frame-drop-count" if property.format == MPV_FORMAT_INT64 => {
                let value = unsafe { *(property.data as *const i64) };
                if value >= 0 {
                    self.state
                        .decoder_frame_drop_count
                        .store(value as u64, Ordering::Release);
                }
            }
            _ => {}
        }
    }
}

fn set_option(api: &MpvApi, handle: *mut MpvHandle, name: &str, value: &str) -> Result<(), String> {
    let name = CString::new(name).map_err(|_| "libmpv option name contains NUL".to_owned())?;
    let value = CString::new(value).map_err(|_| "libmpv option value contains NUL".to_owned())?;
    let code = unsafe { (api.set_option_string)(handle, name.as_ptr(), value.as_ptr()) };
    if code < 0 {
        return Err(format!("libmpv option {name:?} failed: {}", unsafe {
            api.error(code)
        }));
    }
    Ok(())
}

fn set_option_cstring(
    api: &MpvApi,
    handle: *mut MpvHandle,
    name: &str,
    value: &CString,
) -> Result<(), String> {
    let name = CString::new(name).map_err(|_| "libmpv option name contains NUL".to_owned())?;
    let code = unsafe { (api.set_option_string)(handle, name.as_ptr(), value.as_ptr()) };
    if code < 0 {
        return Err(format!("libmpv option {name:?} failed: {}", unsafe {
            api.error(code)
        }));
    }
    Ok(())
}

fn path_to_cstring(path: &OsStr) -> Result<CString, String> {
    #[cfg(unix)]
    {
        CString::new(path.as_bytes()).map_err(|_| "media path contains NUL".to_owned())
    }
    #[cfg(not(unix))]
    {
        CString::new(path.to_string_lossy().as_bytes())
            .map_err(|_| "media path contains NUL".to_owned())
    }
}

fn validate_range(start: f64, end: f64) -> Result<(), String> {
    if !start.is_finite() || !end.is_finite() || start < 0.0 || end <= start {
        return Err("Invalid playback range".to_owned());
    }
    Ok(())
}

fn property_string(data: *mut c_void) -> Option<String> {
    if data.is_null() {
        return None;
    }
    // mpv_event_property::data points to the variable holding a char*.
    let value = unsafe { *(data as *const *const c_char) };
    if value.is_null() {
        None
    } else {
        Some(
            unsafe { CStr::from_ptr(value) }
                .to_string_lossy()
                .into_owned(),
        )
    }
}

#[allow(unsafe_op_in_unsafe_fn)]
unsafe fn node_to_string(node: *const MpvNode, depth: usize) -> Option<String> {
    if node.is_null() || depth > MAX_AUDIO_NODE_DEPTH {
        return None;
    }
    let node = &*node;
    match node.format {
        MPV_FORMAT_STRING => {
            let value = node.data.string;
            (!value.is_null()).then(|| CStr::from_ptr(value).to_string_lossy().into_owned())
        }
        MPV_FORMAT_FLAG => Some(if node.data.flag != 0 { "yes" } else { "no" }.to_owned()),
        MPV_FORMAT_INT64 => Some(node.data.int64.to_string()),
        MPV_FORMAT_DOUBLE => Some(format!("{:.6}", node.data.double_)),
        7 | 8 => {
            let list = node.data.list;
            if list.is_null() || (*list).num < 0 {
                return None;
            }
            let count = ((*list).num as usize).min(MAX_AUDIO_NODE_ENTRIES);
            let mut fields = Vec::with_capacity(count);
            for index in 0..count {
                if (*list).values.is_null() {
                    break;
                }
                let value = (*list).values.add(index);
                let rendered = node_to_string(value, depth + 1).unwrap_or_default();
                if node.format == 8 && !(*list).keys.is_null() {
                    let key = *(*list).keys.add(index);
                    if !key.is_null() {
                        fields.push(format!(
                            "{}={rendered}",
                            CStr::from_ptr(key).to_string_lossy()
                        ));
                        continue;
                    }
                }
                fields.push(rendered);
            }
            Some(fields.join(", "))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_invalid_ranges_without_touching_mpv() {
        assert!(validate_range(f64::NAN, 1.0).is_err());
        assert!(validate_range(1.0, f64::INFINITY).is_err());
        assert!(validate_range(2.0, 2.0).is_err());
        assert!(validate_range(-0.1, 1.0).is_err());
    }

    #[test]
    fn path_conversion_preserves_unicode_and_rejects_nul() {
        let value = path_to_cstring(OsStr::new("Native 1080p60 日本.mp4")).unwrap();
        assert_eq!(value.to_bytes(), "Native 1080p60 日本.mp4".as_bytes());
        assert!(path_to_cstring(OsStr::new("bad\0path")).is_err());
    }

    #[test]
    fn snapshot_is_nonblocking_and_starts_paused() {
        let state = SharedState::default();
        let snapshot = state.snapshot();
        assert_eq!(snapshot.position, 0.0);
        assert!(snapshot.paused);
        assert!(!snapshot.loaded);
        assert!(snapshot.error.is_none());
    }

    #[test]
    fn seek_requests_are_latest_wins() {
        let shared = Shared {
            commands: mpsc::sync_channel(1).0,
            pending_seek: Mutex::new(None),
            pending_controls: Mutex::new(PendingControls::default()),
            mpv_waker: Mutex::new(None),
            next_seek_serial: AtomicU64::new(0),
            wake_pending: AtomicBool::new(false),
            shutdown: AtomicBool::new(false),
        };
        *shared.pending_seek.lock().unwrap() = Some(SeekRequest {
            seconds: 1.0,
            exact: false,
            serial: 1,
        });
        *shared.pending_seek.lock().unwrap() = Some(SeekRequest {
            seconds: 9.0,
            exact: true,
            serial: 2,
        });
        let request = shared.pending_seek.lock().unwrap().take().unwrap();
        assert_eq!(request.seconds, 9.0);
        assert!(request.exact);
    }
}

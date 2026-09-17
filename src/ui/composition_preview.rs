//! Latest-request-wins layered composition preview for the desktop UI.
//!
//! The UI owns the immutable project snapshot and exact timeline time for a
//! request. This module keeps FFmpeg setup, decoding, composition, and PNG
//! encoding off the GPUI thread while retaining only one queued request.

use slicer::{
    composition::{self, frame::RgbaFrame},
    export::FfmpegMediaSource,
    media::Binaries,
    project::{Project, Time},
};
use std::{
    io::Cursor,
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{self, Receiver, Sender},
    },
    thread::{self, JoinHandle},
};

/// A completed or failed layered preview request.
#[derive(Debug)]
pub struct CompositionPreviewEvent {
    /// Generation assigned by the UI. Stale events can be ignored cheaply.
    pub generation: u64,
    /// Exact project time used for the render.
    pub time: Time,
    /// PNG bytes on success, or a user-facing error on failure.
    pub result: Result<Vec<u8>, String>,
}

struct CompositionPreviewRequest {
    generation: u64,
    sequence: u64,
    project: Project,
    time: Time,
}

#[derive(Default)]
struct RequestQueue {
    pending: Mutex<Option<CompositionPreviewRequest>>,
    wake: Condvar,
}

/// Background worker used by the desktop UI to render all visible project
/// layers into one PNG preview image.
pub struct CompositionPreviewWorker {
    requests: Arc<RequestQueue>,
    /// Poll this receiver from the UI thread; no UI callback is invoked by the
    /// worker thread.
    pub events: Receiver<CompositionPreviewEvent>,
    latest_sequence: Arc<AtomicU64>,
    stop: Arc<AtomicBool>,
    active_source: Arc<Mutex<Option<Arc<FfmpegMediaSource>>>>,
    worker: Option<JoinHandle<()>>,
}

impl CompositionPreviewWorker {
    /// Start a worker using the application's already-resolved FFmpeg pair.
    pub fn new(binaries: Binaries) -> Self {
        let requests = Arc::new(RequestQueue::default());
        let latest_sequence = Arc::new(AtomicU64::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let active_source = Arc::new(Mutex::new(None));
        let (event_sender, events) = mpsc::channel();

        let worker_requests = Arc::clone(&requests);
        let worker_latest_sequence = Arc::clone(&latest_sequence);
        let worker_stop = Arc::clone(&stop);
        let worker_active_source = Arc::clone(&active_source);
        let worker = thread::Builder::new()
            .name("slicer-composition-preview".to_owned())
            .spawn(move || {
                run_worker(
                    binaries,
                    worker_requests,
                    event_sender,
                    worker_latest_sequence,
                    worker_stop,
                    worker_active_source,
                )
            })
            .expect("composition preview worker thread must start");

        Self {
            requests,
            events,
            latest_sequence,
            stop,
            active_source,
            worker: Some(worker),
        }
    }

    /// Queue a project snapshot for rendering at an exact project time.
    ///
    /// The project should normally be supplied as `session.project().clone()`.
    /// Replacing a pending request is non-blocking; if a render is already in
    /// progress its FFmpeg source is cancelled and its result is discarded.
    pub fn request(&self, generation: u64, project: Project, time: Time) {
        let sequence = self.next_sequence();
        self.latest_sequence.store(sequence, Ordering::Release);
        cancel_active_source(&self.active_source);

        if self.stop.load(Ordering::Acquire) {
            return;
        }

        let mut pending = lock_unpoisoned(&self.requests.pending);
        if self.stop.load(Ordering::Acquire) {
            return;
        }
        *pending = Some(CompositionPreviewRequest {
            generation,
            sequence,
            project,
            time,
        });
        self.requests.wake.notify_one();
    }

    fn next_sequence(&self) -> u64 {
        self.latest_sequence
            .fetch_add(1, Ordering::AcqRel)
            .wrapping_add(1)
    }
}

impl Drop for CompositionPreviewWorker {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        self.latest_sequence.fetch_add(1, Ordering::AcqRel);
        cancel_active_source(&self.active_source);

        let mut pending = lock_unpoisoned(&self.requests.pending);
        *pending = None;
        self.requests.wake.notify_all();
        drop(pending);

        // The source cancellation above lets an active FFmpeg decode terminate
        // promptly. Joining also guarantees that no worker can outlive the UI
        // object's binaries, event channel, or project data.
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn run_worker(
    binaries: Binaries,
    requests: Arc<RequestQueue>,
    events: Sender<CompositionPreviewEvent>,
    latest_sequence: Arc<AtomicU64>,
    stop: Arc<AtomicBool>,
    active_source: Arc<Mutex<Option<Arc<FfmpegMediaSource>>>>,
) {
    loop {
        let Some(request) = take_latest_request(&requests, &stop) else {
            return;
        };
        if !is_current(request.sequence, &latest_sequence, &stop) {
            continue;
        }

        let source = match FfmpegMediaSource::new(&request.project, binaries.clone()) {
            Ok(source) => Arc::new(source),
            Err(error) => {
                let result = Err(format!("Layered preview media setup failed: {error}"));
                if is_current(request.sequence, &latest_sequence, &stop)
                    && events
                        .send(CompositionPreviewEvent {
                            generation: request.generation,
                            time: request.time,
                            result,
                        })
                        .is_err()
                {
                    return;
                }
                continue;
            }
        };

        {
            let mut active = lock_unpoisoned(&active_source);
            if !is_current(request.sequence, &latest_sequence, &stop) {
                source.cancel();
                continue;
            }
            *active = Some(Arc::clone(&source));
        }

        // A request may arrive between the initial check and publishing the
        // active source. Check once more so that race cannot start stale work.
        if !is_current(request.sequence, &latest_sequence, &stop) {
            source.cancel();
            clear_active_source(&active_source, &source);
            continue;
        }

        let result = render_png(&request.project, request.time, source.as_ref());
        clear_active_source(&active_source, &source);
        if !is_current(request.sequence, &latest_sequence, &stop) {
            continue;
        }

        if events
            .send(CompositionPreviewEvent {
                generation: request.generation,
                time: request.time,
                result,
            })
            .is_err()
        {
            return;
        }
    }
}

fn take_latest_request(
    requests: &RequestQueue,
    stop: &AtomicBool,
) -> Option<CompositionPreviewRequest> {
    let mut pending = lock_unpoisoned(&requests.pending);
    loop {
        if stop.load(Ordering::Acquire) {
            return None;
        }
        if let Some(request) = pending.take() {
            return Some(request);
        }
        pending = requests
            .wake
            .wait(pending)
            .unwrap_or_else(|poisoned| poisoned.into_inner());
    }
}

fn render_png(
    project: &Project,
    time: Time,
    source: &FfmpegMediaSource,
) -> Result<Vec<u8>, String> {
    let frame = composition::render(project, time, source)
        .map_err(|error| format!("Layered preview composition failed: {error}"))?;
    encode_png(&frame)
}

fn encode_png(frame: &RgbaFrame) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    {
        let mut encoder = png::Encoder::new(Cursor::new(&mut bytes), frame.width(), frame.height());
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder
            .write_header()
            .map_err(|error| format!("Layered preview PNG header encoding failed: {error}"))?;
        writer
            .write_image_data(frame.pixels())
            .map_err(|error| format!("Layered preview PNG image encoding failed: {error}"))?;
    }
    Ok(bytes)
}

fn is_current(sequence: u64, latest_sequence: &AtomicU64, stop: &AtomicBool) -> bool {
    !stop.load(Ordering::Acquire) && latest_sequence.load(Ordering::Acquire) == sequence
}

fn cancel_active_source(active_source: &Mutex<Option<Arc<FfmpegMediaSource>>>) {
    let source = lock_unpoisoned(active_source).as_ref().cloned();
    if let Some(source) = source {
        source.cancel();
    }
}

fn clear_active_source(
    active_source: &Mutex<Option<Arc<FfmpegMediaSource>>>,
    source: &Arc<FfmpegMediaSource>,
) {
    let mut active = lock_unpoisoned(active_source);
    if active
        .as_ref()
        .is_some_and(|current| Arc::ptr_eq(current, source))
    {
        *active = None;
    }
}

fn lock_unpoisoned<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

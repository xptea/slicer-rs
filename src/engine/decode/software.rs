use super::{DEFAULT_MAX_FRAME_BYTES, DecodeEvent, DecodeRequest, FrameLease, validate_frame_size};
use crate::media::Binaries;
use anyhow::{Context, Result, anyhow, bail};
use std::io::Read;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::Duration;

const MAX_DIAGNOSTIC_BYTES: usize = 16 * 1024;
// FFmpeg's output seek normally drops the last source frame when the request
// falls in the tail interval between that frame and stream duration. A
// modest constant-rate filter duplicates presentation frames across those
// intervals, making a point request total for VFR and short sources without
// allocating an unbounded decoded timeline.
const POINT_REQUEST_RATE: &str = "120";

/// A bounded software decoder backed by the explicitly resolved FFmpeg tool.
#[derive(Clone, Debug)]
pub struct SoftwareDecoder {
    binaries: Binaries,
    max_frame_bytes: usize,
}

impl SoftwareDecoder {
    pub fn new(binaries: Binaries) -> Result<Self> {
        binaries.validate()?;
        Ok(Self {
            binaries,
            max_frame_bytes: DEFAULT_MAX_FRAME_BYTES,
        })
    }

    /// Override the frame-byte budget for a test or a deliberately smaller
    /// preview profile.  Values of zero are rejected.
    pub fn with_max_frame_bytes(mut self, max_frame_bytes: usize) -> Result<Self> {
        if max_frame_bytes == 0 {
            bail!("decoder frame budget must be positive");
        }
        self.max_frame_bytes = max_frame_bytes;
        Ok(self)
    }

    /// Decode one frame into an owned RGBA lease.  The subprocess is polled
    /// while a reader drains its pipes, so cancellation can kill FFmpeg even
    /// when it is waiting on a damaged or slow input.
    pub fn decode(&self, request: &DecodeRequest, cancel: &AtomicBool) -> Result<FrameLease> {
        request.validate()?;
        if cancel.load(Ordering::Acquire) {
            bail!("decode cancelled");
        }
        let (source_width, source_height) = dimensions(&self.binaries, &request.path)?;
        let (width, height, scale_filter) = match (request.width, request.height) {
            (Some(width), Some(height)) => (width, height, Some(format!("scale={width}:{height}"))),
            (None, None) => (source_width, source_height, None),
            _ => unreachable!("DecodeRequest::validate checked paired dimensions"),
        };
        let filter = Some(match scale_filter {
            Some(scale) => format!("fps={POINT_REQUEST_RATE},{scale}"),
            None => format!("fps={POINT_REQUEST_RATE}"),
        });
        validate_frame_size(width, height, width_height_bytes(width, height)?)?;
        let expected_bytes = width_height_bytes(width, height)?;
        if expected_bytes > self.max_frame_bytes {
            bail!(
                "decoded frame requires {expected_bytes} bytes, over the configured {} byte budget",
                self.max_frame_bytes
            );
        }

        let mut command = Command::new(&self.binaries.ffmpeg);
        command
            .args(["-hide_banner", "-loglevel", "error", "-i"])
            .arg(&request.path)
            .args(["-ss", &request.timestamp.to_string(), "-frames:v", "1"]);
        if let Some(filter) = filter {
            command.args(["-vf", &filter]);
        }
        command
            // The bundled FFmpeg build intentionally omits the standalone
            // rawvideo muxer but retains the image2pipe demuxer/encoder.  A
            // rawvideo codec over image2pipe has the same byte contract and
            // keeps this fallback usable with the packaged runtime.
            .args([
                "-f",
                "image2pipe",
                "-vcodec",
                "rawvideo",
                "-pix_fmt",
                "rgba",
                "pipe:1",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let mut child = command.spawn().with_context(|| {
            format!(
                "failed to launch bundled FFmpeg for {}",
                request.path.display()
            )
        })?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow!("FFmpeg stdout was not piped"))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| anyhow!("FFmpeg stderr was not piped"))?;
        let stdout_reader = thread::spawn(move || read_limited(stdout, expected_bytes));
        let stderr_reader = thread::spawn(move || read_limited(stderr, MAX_DIAGNOSTIC_BYTES));

        let status = wait_for_child(&mut child, cancel)?;
        let pixels = stdout_reader
            .join()
            .map_err(|_| anyhow!("FFmpeg frame reader panicked"))??;
        let diagnostics = stderr_reader
            .join()
            .map_err(|_| anyhow!("FFmpeg diagnostic reader panicked"))??;
        if cancel.load(Ordering::Acquire) {
            bail!("decode cancelled");
        }
        if !status.success() {
            let detail = String::from_utf8_lossy(&diagnostics).trim().to_owned();
            if detail.is_empty() {
                bail!("FFmpeg frame decode failed with status {status}");
            }
            bail!("FFmpeg frame decode failed with status {status}: {detail}");
        }
        validate_frame_size(width, height, pixels.len())?;
        Ok(FrameLease::new(
            request.path.clone(),
            request.timestamp,
            width,
            height,
            request.generation,
            pixels,
        ))
    }
}

/// Latest-request-wins worker for preview seeks.  At most one FFmpeg process
/// is active and the input channel is coalesced before every decode.
pub struct SoftwareDecodeWorker {
    requests: Sender<DecodeRequest>,
    pub events: Receiver<DecodeEvent>,
    stop: Arc<AtomicBool>,
}

impl SoftwareDecodeWorker {
    pub fn new(decoder: SoftwareDecoder) -> Self {
        let (requests, request_rx) = mpsc::channel::<DecodeRequest>();
        let (event_tx, events) = mpsc::channel::<DecodeEvent>();
        let stop = Arc::new(AtomicBool::new(false));
        let stopping = Arc::clone(&stop);
        thread::Builder::new()
            .name("slicer-software-decode".to_owned())
            .spawn(move || {
                while let Ok(mut request) = request_rx.recv() {
                    while let Ok(newer) = request_rx.try_recv() {
                        request = newer;
                    }
                    if stopping.load(Ordering::Acquire) {
                        break;
                    }
                    let result = decoder
                        .decode(&request, &stopping)
                        .map_err(|error| format!("{error:#}"));
                    if stopping.load(Ordering::Acquire) {
                        break;
                    }
                    if event_tx.send(DecodeEvent { request, result }).is_err() {
                        break;
                    }
                }
            })
            .expect("software decode worker thread must start");
        Self {
            requests,
            events,
            stop,
        }
    }

    pub fn request(&self, request: DecodeRequest) -> Result<()> {
        request.validate()?;
        self.requests
            .send(request)
            .map_err(|_| anyhow!("software decode worker is stopped"))
    }
}

impl Drop for SoftwareDecodeWorker {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
    }
}

fn dimensions(binaries: &Binaries, path: &Path) -> Result<(u32, u32)> {
    let info = crate::media::inspect(binaries, path)?;
    let stream = info
        .streams
        .iter()
        .find(|stream| stream.kind == "video")
        .ok_or_else(|| anyhow!("decode input has no video stream"))?;
    let width = stream
        .width
        .ok_or_else(|| anyhow!("video width is unknown"))?;
    let height = stream
        .height
        .ok_or_else(|| anyhow!("video height is unknown"))?;
    Ok((width, height))
}

fn width_height_bytes(width: u32, height: u32) -> Result<usize> {
    usize::try_from(width)
        .ok()
        .and_then(|width| {
            usize::try_from(height)
                .ok()
                .and_then(|height| width.checked_mul(height))
        })
        .and_then(|pixels| pixels.checked_mul(4))
        .ok_or_else(|| anyhow!("frame dimensions overflow"))
}

fn read_limited<R: Read>(mut reader: R, limit: usize) -> Result<Vec<u8>> {
    let mut bytes = Vec::with_capacity(limit.min(1024 * 1024));
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        if bytes.len().saturating_add(read) > limit {
            bail!("FFmpeg output exceeded the configured {} byte limit", limit);
        }
        bytes.extend_from_slice(&buffer[..read]);
    }
    Ok(bytes)
}

fn wait_for_child(child: &mut Child, cancel: &AtomicBool) -> Result<std::process::ExitStatus> {
    loop {
        if cancel.load(Ordering::Acquire) {
            let _ = child.kill();
            let _ = child.wait();
            bail!("decode cancelled");
        }
        match child.try_wait()? {
            Some(status) => return Ok(status),
            None => thread::sleep(Duration::from_millis(5)),
        }
    }
}

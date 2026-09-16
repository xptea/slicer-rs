//! Latest-request-wins, bounded FFmpeg still-frame preview. No UI-thread decoding.
use crate::media::Binaries;
use std::{
    io::Read,
    path::PathBuf,
    process::{Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, Sender},
    },
    thread,
    time::{Duration, Instant},
};

pub struct PreviewEvent {
    pub path: PathBuf,
    pub seconds: f64,
    pub result: Result<Vec<u8>, String>,
}
pub struct PreviewWorker {
    requests: Sender<(PathBuf, f64)>,
    pub events: Receiver<PreviewEvent>,
    stop: Arc<AtomicBool>,
}
impl PreviewWorker {
    pub fn new(binaries: Binaries) -> Self {
        let (requests, rx) = mpsc::channel::<(PathBuf, f64)>();
        let (tx, events) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let stopping = stop.clone();
        thread::spawn(move || {
            while let Ok(mut request) = rx.recv() {
                while let Ok(newer) = rx.try_recv() {
                    request = newer;
                }
                if stopping.load(Ordering::Relaxed) {
                    break;
                }
                let (path, seconds) = request;
                let result = frame(&binaries, &path, seconds, &stopping);
                if stopping.load(Ordering::Relaxed) {
                    break;
                }
                if tx
                    .send(PreviewEvent {
                        path,
                        seconds,
                        result,
                    })
                    .is_err()
                {
                    break;
                }
            }
        });
        Self {
            requests,
            events,
            stop,
        }
    }
    pub fn request(&self, path: PathBuf, seconds: f64) {
        let _ = self.requests.send((path, seconds));
    }
}
impl Drop for PreviewWorker {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}
fn frame(
    binaries: &Binaries,
    path: &PathBuf,
    seconds: f64,
    stop: &AtomicBool,
) -> Result<Vec<u8>, String> {
    if !seconds.is_finite() || seconds < 0.0 {
        return Err("Preview time must be a nonnegative number".into());
    }
    let mut child = Command::new(&binaries.ffmpeg)
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-nostdin",
            "-ss",
            &format!("{seconds:.6}"),
            "-i",
        ])
        .arg(path)
        .args([
            "-map",
            "0:v:0",
            "-frames:v",
            "1",
            "-vf",
            "scale=960:540:force_original_aspect_ratio=decrease",
            "-threads",
            "1",
            "-f",
            "image2pipe",
            "-c:v",
            "png",
            "pipe:1",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("Cannot start preview: {e}"))?;
    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();
    let out = thread::spawn(move || {
        let mut bytes = Vec::new();
        stdout
            .take(8 * 1024 * 1024)
            .read_to_end(&mut bytes)
            .map(|_| bytes)
    });
    let err = thread::spawn(move || {
        let mut bytes = Vec::new();
        stderr
            .take(64 * 1024)
            .read_to_end(&mut bytes)
            .map(|_| bytes)
    });
    let began = Instant::now();
    let status = loop {
        if stop.load(Ordering::Relaxed) || began.elapsed() > Duration::from_secs(15) {
            let _ = child.kill();
            let _ = child.wait();
            let _ = out.join();
            let _ = err.join();
            return Err("Preview cancelled or timed out".into());
        }
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => thread::sleep(Duration::from_millis(15)),
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(e.to_string());
            }
        }
    };
    let bytes = out
        .join()
        .map_err(|_| "Preview reader stopped")?
        .map_err(|e| e.to_string())?;
    let errors = err
        .join()
        .map_err(|_| "Preview error reader stopped")?
        .map_err(|e| e.to_string())?;
    if !status.success() {
        return Err(String::from_utf8_lossy(&errors).trim().to_string());
    }
    if bytes.is_empty() {
        return Err("No video frame at this position".into());
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_invalid_seek_before_launching() {
        let bins = Binaries {
            ffmpeg: "/nonexistent".into(),
            ffprobe: "/nonexistent".into(),
        };
        assert!(
            frame(&bins, &"missing".into(), f64::NAN, &AtomicBool::new(false))
                .unwrap_err()
                .contains("nonnegative")
        );
    }
}

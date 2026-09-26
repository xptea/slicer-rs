//! Preview-only decoded-frame cache. Neither FFmpeg nor filesystem work runs on
//! the UI/GL thread. Original media always supplies playback, release and export.
use super::{
    decoder::{Decoder, Frame},
    preview_source::PreviewSource,
    project::Time,
};
use std::{
    collections::{HashMap, VecDeque},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, SystemTime},
};
const FRAME_BUDGET: usize = 128 * 1024 * 1024;
const DISK_BUDGET: u64 = 2 * 1024 * 1024 * 1024;
const FILE_BUDGET: u64 = DISK_BUDGET / 4;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Target {
    pub path: PathBuf,
    pub time: Time,
}
#[derive(Clone)]
struct Cached {
    path: PathBuf,
    frame: Arc<Frame>,
}
#[derive(Default)]
struct Frames {
    entries: VecDeque<Cached>,
    bytes: usize,
}
impl Frames {
    fn exact(&self, path: &Path, time: Time) -> Option<Arc<Frame>> {
        self.entries
            .iter()
            .rev()
            .find(|c| c.path == path && contains(&c.frame, time))
            .map(|c| c.frame.clone())
    }
    fn nearest(&self, path: &Path, time: Time) -> Option<Arc<Frame>> {
        self.exact(path, time).or_else(|| {
            self.entries
                .iter()
                .filter(|c| c.path == path && (c.frame.pts - time).abs() <= 500_000)
                .min_by_key(|c| (c.frame.pts - time).abs())
                .map(|c| c.frame.clone())
        })
    }
    fn insert(&mut self, path: PathBuf, frame: Arc<Frame>) {
        if let Some(index) = self
            .entries
            .iter()
            .position(|c| c.path == path && c.frame.pts == frame.pts)
        {
            if self.entries[index].frame.duration >= frame.duration {
                return;
            }
            self.bytes -= self.entries.remove(index).unwrap().frame.pixels.len();
        }
        self.bytes += frame.pixels.len();
        self.entries.push_back(Cached { path, frame });
        while self.bytes > FRAME_BUDGET || self.entries.len() > 240 {
            self.bytes -= self.entries.pop_front().unwrap().frame.pixels.len();
        }
    }
}
fn contains(frame: &Frame, time: Time) -> bool {
    time >= frame.pts && time < frame.pts + frame.duration
}
#[derive(Default)]
struct State {
    sources: Vec<PathBuf>,
    targets: Vec<Target>,
    frames: Frames,
    stopped: bool,
    proxies: HashMap<PathBuf, Result<PathBuf, String>>,
    proxy_queue: VecDeque<PathBuf>,
    building: Option<PathBuf>,
    errors: HashMap<PathBuf, String>,
}
struct Shared {
    state: Mutex<State>,
    wake: Condvar,
    cancel: Arc<AtomicU64>,
    directory: PathBuf,
}
pub struct ScrubCache {
    shared: Arc<Shared>,
    workers: Vec<JoinHandle<()>>,
}
impl ScrubCache {
    pub fn new() -> Self {
        Self::in_directory(cache_root())
    }
    pub fn in_directory(directory: PathBuf) -> Self {
        let shared = Arc::new(Shared {
            state: Mutex::new(State::default()),
            wake: Condvar::new(),
            cancel: Arc::new(AtomicU64::new(0)),
            directory,
        });
        let decode = shared.clone();
        let proxy = shared.clone();
        Self {
            shared,
            workers: vec![
                thread::spawn(move || decode_loop(decode)),
                thread::spawn(move || proxy_loop(proxy)),
            ],
        }
    }
    /// Prepare imported media before the playhead reaches it. This also lets
    /// the filmstrip worker reuse the same HDR-to-SDR proxy.
    pub fn prepare(&self, sources: Vec<PathBuf>) {
        let mut s = self.shared.state.lock().unwrap();
        if s.sources == sources {
            return;
        }
        s.proxy_queue.retain(|p| sources.contains(p));
        for path in &sources {
            if !s.proxies.contains_key(path)
                && s.building.as_ref() != Some(path)
                && !s.proxy_queue.contains(path)
            {
                s.proxy_queue.push_back(path.clone());
            }
        }
        s.sources = sources;
        self.shared.wake.notify_all();
    }
    pub fn request(&self, targets: Vec<Target>) {
        let mut s = self.shared.state.lock().unwrap();
        if s.targets == targets {
            return;
        }
        for t in &targets {
            if !s.proxies.contains_key(&t.path)
                && s.building.as_ref() != Some(&t.path)
                && !s.proxy_queue.contains(&t.path)
            {
                s.proxy_queue.push_front(t.path.clone());
            }
        }
        s.targets = targets;
        self.shared.wake.notify_all();
    }
    pub fn frame(&self, path: &Path, time: Time) -> Option<Arc<Frame>> {
        self.shared.state.lock().unwrap().frames.nearest(path, time)
    }
    pub fn failed(&self, path: &Path) -> bool {
        self.shared.state.lock().unwrap().errors.contains_key(path)
    }
    pub fn status(&self) -> String {
        let s = self.shared.state.lock().unwrap();
        let ready = s.proxies.values().filter(|p| p.is_ok()).count();
        if s.building.is_some() {
            format!("Preparing scrub previews · {ready} ready")
        } else if let Some(error) = s.errors.values().next() {
            format!("Original preview · {error}")
        } else if let Some(error) = s.proxies.values().find_map(|p| p.as_ref().err()) {
            format!("Frame cache active · proxy unavailable: {error}")
        } else {
            format!("Scrub cache · {ready} previews ready")
        }
    }
}
impl Default for ScrubCache {
    fn default() -> Self {
        Self::new()
    }
}
impl Drop for ScrubCache {
    fn drop(&mut self) {
        self.shared.state.lock().unwrap().stopped = true;
        self.shared.cancel.store(1, Ordering::Release);
        self.shared.wake.notify_all();
        for worker in self.workers.drain(..) {
            let _ = worker.join();
        }
    }
}
fn decode_loop(shared: Arc<Shared>) {
    let mut decoders: VecDeque<(PathBuf, Decoder)> = VecDeque::new();
    loop {
        let (target, input) = {
            let mut s = shared.state.lock().unwrap();
            loop {
                if s.stopped {
                    return;
                }
                // All visible layers first, then a small neighborhood in both directions.
                let job = [0, 33_334, -33_334, 66_668, -66_668, 100_002, -100_002]
                    .into_iter()
                    .find_map(|offset| {
                        s.targets.iter().find_map(|t| {
                            let time = (t.time + offset).max(0);
                            (!s.errors.contains_key(&t.path)
                                && s.frames.exact(&t.path, time).is_none())
                            .then(|| Target {
                                path: t.path.clone(),
                                time,
                            })
                        })
                    });
                if let Some(target) = job {
                    let input = s
                        .proxies
                        .get(&target.path)
                        .and_then(|r| r.as_ref().ok())
                        .cloned()
                        .unwrap_or_else(|| target.path.clone());
                    break (target, input);
                }
                s = shared.wake.wait(s).unwrap();
            }
        };
        let result = (|| -> anyhow::Result<Arc<Frame>> {
            let index = decoders.iter().position(|(p, _)| p == &input);
            let mut d = if let Some(i) = index {
                decoders.remove(i).unwrap().1
            } else {
                let mut d = Decoder::open_preview(&input)?;
                d.cancel_on_generation(shared.cancel.clone(), 0);
                d
            };
            let frame = d.video(target.time.min((d.info.duration_us - 1).max(0)))?;
            decoders.push_back((input.clone(), d));
            while decoders.len() > 4 {
                decoders.pop_front();
            }
            Ok(frame)
        })();
        let mut s = shared.state.lock().unwrap();
        match result {
            Ok(frame) => {
                // Clamp prefetch at EOF so the worker cannot spin forever there.
                let mut frame = (*frame).clone();
                if target.time >= frame.pts + frame.duration {
                    frame.duration = target.time - frame.pts + 1;
                }
                s.frames.insert(target.path, Arc::new(frame));
            }
            Err(e) if input != target.path => {
                // An evicted/damaged disk cache is never a media failure.
                s.proxies.insert(target.path, Err(e.to_string()));
            }
            Err(e) => {
                s.errors.insert(target.path, e.to_string());
            }
        }
    }
}

fn cache_root() -> PathBuf {
    std::env::var_os("SLICER_PROXY_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            std::env::var_os("XDG_CACHE_HOME")
                .map(PathBuf::from)
                .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))
                .unwrap_or_else(std::env::temp_dir)
                .join("slicer/scrub-v1")
        })
}
/// Only finished proxies are published under this name (atomic rename).
/// Call on workers, never on the UI thread.
pub fn cached_proxy(path: &Path) -> Option<PathBuf> {
    let output = cache_root().join(proxy_name(path).ok()?);
    output.is_file().then_some(output)
}
/// Stable identity includes source metadata; replacing media invalidates its proxy.
fn proxy_name(path: &Path) -> anyhow::Result<String> {
    let path = path.canonicalize()?;
    let meta = path.metadata()?;
    let mut hash = 0xcbf29ce484222325u64;
    let stamp = format!(
        "rgba640-png30-v3:{}:{:?}",
        meta.len(),
        meta.modified()?.duration_since(SystemTime::UNIX_EPOCH)?
    );
    for b in path
        .as_os_str()
        .as_encoded_bytes()
        .iter()
        .chain(stamp.as_bytes())
    {
        hash = (hash ^ u64::from(*b)).wrapping_mul(0x100000001b3);
    }
    Ok(format!("{hash:016x}.mov"))
}
fn proxy_loop(shared: Arc<Shared>) {
    loop {
        let path = {
            let mut s = shared.state.lock().unwrap();
            while !s.stopped && s.proxy_queue.is_empty() {
                s = shared.wake.wait(s).unwrap();
            }
            if s.stopped {
                return;
            }
            let path = s.proxy_queue.pop_front().unwrap();
            s.building = Some(path.clone());
            path
        };
        let result = build_proxy(&path, &shared).map_err(|e| e.to_string());
        let mut s = shared.state.lock().unwrap();
        if result.is_ok() {
            s.errors.remove(&path);
        }
        s.proxies.insert(path, result);
        s.building = None;
        shared.wake.notify_all();
    }
}
fn build_proxy(path: &Path, shared: &Shared) -> anyhow::Result<PathBuf> {
    let root = &shared.directory;
    std::fs::create_dir_all(root)?;
    let output = root.join(proxy_name(path)?);
    if output.is_file()
        && Decoder::open_preview(&output)
            .and_then(|mut d| d.video(0))
            .is_ok()
    {
        return Ok(output);
    }
    // Keep HDR color conversion in the same GPU pipeline used for playback.
    // Relabeling HLG/PQ as SDR here would produce washed-out thumbnails.
    let mut check = Decoder::open(path, false)?;
    check.cancel_on_generation(shared.cancel.clone(), 0);
    match check.video(0) {
        Ok(_) => {}
        Err(error) if error.to_string().contains("HDR media") => {
            drop(check);
            return build_color_proxy(path, &output, shared);
        }
        Err(error) => return Err(error),
    }
    drop(check);
    let partial = output.with_extension(format!("{}.partial.mov", std::process::id()));
    let log = partial.with_extension("log");
    let result = (|| -> anyhow::Result<()> {
        let bins = crate::media::Binaries::resolve()?;
        let stderr = std::fs::File::create(&log)?;
        let mut child = Command::new(bins.ffmpeg)
            .args([
                "-nostdin",
                "-hide_banner",
                "-loglevel",
                "error",
                "-y",
                "-threads",
                "2",
                "-i",
            ])
            .arg(path)
            .args([
                "-map",
                "0:v:0",
                "-an",
                "-sn",
                "-dn",
                "-vf",
                "scale=w='min(640,iw)':h='min(640,ih)':force_original_aspect_ratio=decrease:force_divisible_by=2,fps=30",
                "-filter_threads",
                "1",
                "-c:v",
                "png",
                "-compression_level",
                "1",
                "-pred",
                "mixed",
                "-threads",
                "2",
                "-pix_fmt",
                "rgb24",
                "-f",
                "mov",
            ])
            .arg(&partial)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(stderr)
            .spawn()?;
        loop {
            let stopped = shared.state.lock().unwrap().stopped;
            let oversized = partial.metadata().is_ok_and(|m| m.len() > FILE_BUDGET);
            if stopped || oversized {
                let _ = child.kill();
                let _ = child.wait();
                anyhow::bail!(if stopped {
                    "Preview preparation cancelled"
                } else {
                    "Preview exceeds 512 MiB cache limit"
                });
            }
            match child.try_wait() {
                Ok(Some(status)) => {
                    if !status.success() {
                        let error = std::fs::read_to_string(&log).unwrap_or_default();
                        anyhow::bail!("{}", error.chars().take(240).collect::<String>());
                    }
                    break;
                }
                Ok(None) => {
                    let s = shared.state.lock().unwrap();
                    let _ = shared
                        .wake
                        .wait_timeout(s, Duration::from_millis(50))
                        .unwrap();
                }
                Err(e) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(e.into());
                }
            }
        }
        Decoder::open_preview(&partial)?.video(0)?;
        std::fs::rename(&partial, &output)?;
        prune(&root, &output);
        Ok(())
    })();
    let _ = std::fs::remove_file(partial);
    let _ = std::fs::remove_file(log);
    result?;
    Ok(output)
}

fn build_color_proxy(path: &Path, output: &Path, shared: &Shared) -> anyhow::Result<PathBuf> {
    use image::{
        ImageEncoder,
        codecs::png::{CompressionType, FilterType, PngEncoder},
    };
    let partial = output.with_extension(format!("{}.partial.mov", std::process::id()));
    let log = partial.with_extension("log");
    let result = (|| -> anyhow::Result<()> {
        let bins = crate::media::Binaries::resolve()?;
        let mut source = PreviewSource::open(path)?;
        source.cancel_on_generation(shared.cancel.clone(), 0);
        let mut child = Command::new(bins.ffmpeg)
            .args([
                "-nostdin",
                "-hide_banner",
                "-loglevel",
                "error",
                "-y",
                "-f",
                "image2pipe",
                "-c:v",
                "png",
                "-framerate",
                "30",
                "-i",
                "pipe:0",
                "-an",
                "-c:v",
                "png",
                "-compression_level",
                "1",
                "-pred",
                "mixed",
                "-threads",
                "2",
                "-pix_fmt",
                "rgb24",
                "-color_primaries",
                "bt709",
                "-color_trc",
                "iec61966-2-1",
                "-colorspace",
                "bt709",
                "-f",
                "mov",
            ])
            .arg(&partial)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(std::fs::File::create(&log)?)
            .spawn()?;
        let encode = (|| -> anyhow::Result<()> {
            let mut pipe = child.stdin.take().unwrap();
            let count = (source.info.duration_us * 30 + 999_999) / 1_000_000;
            for i in 0..count {
                anyhow::ensure!(
                    shared.cancel.load(Ordering::Acquire) == 0,
                    "Preview preparation cancelled"
                );
                anyhow::ensure!(
                    !partial.metadata().is_ok_and(|m| m.len() > FILE_BUDGET),
                    "Preview exceeds 512 MiB cache limit"
                );
                let frame = source.sequential_video(i * 1_000_000 / 30)?;
                PngEncoder::new_with_quality(
                    &mut pipe,
                    CompressionType::Uncompressed,
                    FilterType::NoFilter,
                )
                .write_image(
                    &frame.pixels,
                    frame.width,
                    frame.height,
                    image::ExtendedColorType::Rgba8,
                )?;
            }
            Ok(())
        })();
        if encode.is_err() {
            let _ = child.kill();
        }
        let status = child.wait()?;
        encode?;
        anyhow::ensure!(
            status.success(),
            "HDR preview encoding failed: {}",
            std::fs::read_to_string(&log).unwrap_or_default()
        );
        Decoder::open_preview(&partial)?.video(0)?;
        std::fs::rename(&partial, output)?;
        prune(&shared.directory, output);
        Ok(())
    })();
    let _ = std::fs::remove_file(partial);
    let _ = std::fs::remove_file(log);
    result?;
    Ok(output.to_owned())
}
fn prune(root: &Path, keep: &Path) {
    let Ok(files) = std::fs::read_dir(root) else {
        return;
    };
    let mut files: Vec<_> = files
        .flatten()
        .filter_map(|e| {
            let path = e.path();
            let name = path.file_name()?.to_str()?;
            if name.len() != 20
                || !(name.ends_with(".mov") || name.ends_with(".mp4"))
                || !name[..16].bytes().all(|b| b.is_ascii_hexdigit())
            {
                return None;
            }
            let m = e.metadata().ok()?;
            Some((path, m.len(), m.modified().ok()?))
        })
        .collect();
    let mut bytes: u64 = files.iter().map(|(_, n, _)| n).sum();
    files.sort_by_key(|(_, _, time)| *time);
    for (p, n, _) in files {
        if bytes <= DISK_BUDGET {
            break;
        }
        if p != keep && std::fs::remove_file(p).is_ok() {
            bytes -= n;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn frame(pts: Time, bytes: usize) -> Arc<Frame> {
        Arc::new(Frame {
            width: 1,
            height: 1,
            rgba: true,
            full_range: true,
            matrix: 1,
            transfer: 1,
            primaries: 1,
            pts,
            duration: 33_333,
            pixels: vec![0; bytes].into(),
        })
    }
    #[test]
    fn cache_never_crosses_sources_and_is_bounded() {
        let mut c = Frames::default();
        c.insert("a".into(), frame(0, 4));
        c.insert("b".into(), frame(200_000, 4));
        assert_eq!(c.nearest(Path::new("a"), 220_000).unwrap().pts, 0);
        assert!(c.nearest(Path::new("a"), 900_000).is_none());
        for i in 1..300 {
            c.insert("a".into(), frame(i * 33_333, 4));
        }
        assert!(c.entries.len() <= 240);
        assert!(c.exact(Path::new("a"), 0).is_none());
    }
    #[test]
    fn proxy_identity_changes_with_source_contents() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("video.mp4");
        std::fs::write(&path, "old").unwrap();
        let old = proxy_name(&path).unwrap();
        std::fs::write(&path, "replacement").unwrap();
        assert_ne!(old, proxy_name(&path).unwrap());
    }
}

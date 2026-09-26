#![cfg(feature = "desktop")]
use slicer::engine::{
    decoder::Decoder,
    scrub::{ScrubCache, Target},
};
use std::{
    path::{Path, PathBuf},
    process::Command,
    time::{Duration, Instant},
};
fn fixture(dir: &Path) -> PathBuf {
    let path = dir.join("color bars 日本.mov");
    let bins = slicer::media::Binaries::resolve().expect("SLICER_FFMPEG_DIR");
    let o = Command::new(bins.ffmpeg)
        .args([
            "-v",
            "error",
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=720x1280:rate=30",
            "-t",
            "3",
            "-c:v",
            "png",
            "-pix_fmt",
            "rgb24",
            "-threads",
            "1",
        ])
        .arg(&path)
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    path
}
fn wait(mut check: impl FnMut() -> bool) {
    let start = Instant::now();
    while !check() {
        assert!(start.elapsed() < Duration::from_secs(20), "timed out");
        std::thread::sleep(Duration::from_millis(5));
    }
}
#[test]
#[ignore = "Requires bundled FFmpeg"]
fn scaled_decode_proxy_seek_reverse_eof_and_shutdown() {
    let dir = tempfile::tempdir().unwrap();
    let path = fixture(dir.path());
    // Dimensions not aligned to SIMD vectors exercise the packed RGBA buffer.
    let mut d = Decoder::open_preview(&path).unwrap();
    for i in 0..100 {
        let t = (i * 7919 % 100) * 29_999;
        let f = d.video(t).unwrap();
        assert!(f.rgba);
        assert!(f.width <= 640 && f.height <= 640);
        assert_eq!(f.pixels.len(), (f.width * f.height * 4) as usize);
        assert!(
            f.pts <= t && t < f.pts + f.duration,
            "target={t}, pts={}, duration={}",
            f.pts,
            f.duration
        );
    }
    let cache_dir = dir.path().join("cache");
    let cache = ScrubCache::in_directory(cache_dir.clone());
    cache.request(vec![Target {
        path: path.clone(),
        time: 1_000_000,
    }]);
    wait(|| cache.status().contains("1 previews ready"));
    for t in [2_800_000, 200_000, 1_733_333, 0, 2_999_999] {
        cache.request(vec![Target {
            path: path.clone(),
            time: t,
        }]);
        wait(|| {
            cache
                .frame(&path, t)
                .is_some_and(|f| f.pts <= t && t < f.pts + f.duration)
        });
    }
    assert!(cache.frame(Path::new("another clip"), 0).is_none());
    let began = Instant::now();
    drop(cache);
    assert!(began.elapsed() < Duration::from_secs(2));
    let files: Vec<_> = std::fs::read_dir(&cache_dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    assert_eq!(files.len(), 1, "partials must be removed");
    let mut proxy = Decoder::open_preview(&files[0]).unwrap();
    assert!((proxy.info.duration_us - 3_000_000).abs() < 35_000);
    let f = proxy.video(1_733_333).unwrap();
    assert!((f.pts - 1_733_333).abs() < 35_000);
    // Validate actual pixels, not just timestamps: the old MPEG-4 proxies
    // reported successful decodes while displaying green/magenta stripes.
    for t in [200_000, 1_000_000, 2_000_000] {
        let source = d.video(t).unwrap();
        let preview = proxy.video(t).unwrap();
        assert_eq!(
            (source.width, source.height),
            (preview.width, preview.height)
        );
        let error = source
            .pixels
            .iter()
            .zip(preview.pixels.iter())
            .map(|(a, b)| (*a as f64 - *b as f64).abs())
            .sum::<f64>()
            / source.pixels.len() as f64;
        assert!(
            error < 8.,
            "proxy pixels differ from source: mean error {error}"
        );
    }
    let probe = slicer::media::Binaries::resolve().unwrap();
    let output = Command::new(probe.ffprobe)
        .args([
            "-v",
            "error",
            "-select_streams",
            "v:0",
            "-show_entries",
            "frame=key_frame",
            "-of",
            "csv=p=0",
        ])
        .arg(&files[0])
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .all(|line| line.trim() == "1"),
        "proxy must be all intra"
    );
}
#[test]
#[ignore = "Requires bundled FFmpeg"]
fn cancellation_removes_partial_proxy_and_corrupt_cache_falls_back() {
    let dir = tempfile::tempdir().unwrap();
    let path = fixture(dir.path());
    let cache_dir = dir.path().join("cache");
    let cache = ScrubCache::in_directory(cache_dir.clone());
    cache.request(vec![Target {
        path: path.clone(),
        time: 0,
    }]);
    wait(|| cache.status().contains("Preparing"));
    drop(cache);
    if cache_dir.exists() {
        assert!(
            !std::fs::read_dir(&cache_dir).unwrap().any(|e| e
                .unwrap()
                .file_name()
                .to_string_lossy()
                .contains("partial"))
        );
    }
    let cache = ScrubCache::in_directory(cache_dir.clone());
    cache.request(vec![Target {
        path: path.clone(),
        time: 0,
    }]);
    wait(|| cache.status().contains("1 previews ready"));
    drop(cache);
    let file = std::fs::read_dir(&cache_dir)
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    std::fs::write(&file, "broken proxy").unwrap();
    let cache = ScrubCache::in_directory(cache_dir);
    cache.request(vec![Target {
        path: path.clone(),
        time: 2_000_000,
    }]);
    wait(|| cache.status().contains("1 previews ready"));
    wait(|| cache.frame(&path, 2_000_000).is_some());
    assert!(!cache.failed(&path));
}

#[test]
#[ignore = "Requires GPU runtime and SLICER_HDR_TEST_FILE"]
fn hdr_proxy_matches_gpu_preview_and_stays_seekable() {
    use slicer::engine::{preview_source::PreviewSource, scrub::cached_proxy};
    let path = PathBuf::from(std::env::var_os("SLICER_HDR_TEST_FILE").unwrap());
    let dir = tempfile::tempdir().unwrap();
    // Reuse an already benchmarked proxy when provided; otherwise exercise a
    // cold preparation. Source identity still includes size and modified time.
    if let Some(proxy) = cached_proxy(&path) {
        std::fs::copy(&proxy, dir.path().join(proxy.file_name().unwrap())).unwrap();
    }
    let cache = ScrubCache::in_directory(dir.path().into());
    cache.prepare(vec![path.clone()]);
    let began = Instant::now();
    while !cache.status().contains("1 previews ready") {
        assert!(
            began.elapsed() < Duration::from_secs(180),
            "{}",
            cache.status()
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    let proxy = std::fs::read_dir(dir.path())
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let mut decoded = Decoder::open_preview(&proxy).unwrap();
    let mut reference = PreviewSource::open(&path).unwrap();
    assert!((decoded.info.duration_us - reference.info.duration_us).abs() <= 34_000);
    for time in [0, 1_500_000, 12_000_000, 25_000_000, 200_000] {
        let expected = reference.video(time).unwrap();
        let actual = decoded.video(time).unwrap();
        assert_eq!(
            (actual.width, actual.height),
            (expected.width, expected.height)
        );
        assert!(actual.pts <= time && time < actual.pts + actual.duration);
        let error = expected
            .pixels
            .iter()
            .zip(actual.pixels.iter())
            .map(|(a, b)| (*a as f64 - *b as f64).abs())
            .sum::<f64>()
            / actual.pixels.len() as f64;
        println!("HDR proxy target={time}, mean pixel error={error:.3}");
        assert!(
            error < 8.,
            "HDR proxy differs from GPU playback at {time}: {error}"
        );
        cache.request(vec![Target {
            path: path.clone(),
            time,
        }]);
        wait(|| {
            cache
                .frame(&path, time)
                .is_some_and(|f| f.pts <= time && time < f.pts + f.duration)
        });
        assert!(!cache.failed(&path));
    }
    drop(cache);
    let cancelled = tempfile::tempdir().unwrap();
    let cache = ScrubCache::in_directory(cancelled.path().into());
    cache.prepare(vec![path]);
    wait(|| {
        std::fs::read_dir(cancelled.path())
            .unwrap()
            .flatten()
            .any(|e| {
                e.file_name().to_string_lossy().ends_with("partial.mov")
                    && e.metadata().is_ok_and(|m| m.len() > 0)
            })
    });
    let stopping = Instant::now();
    drop(cache);
    assert!(
        stopping.elapsed() < Duration::from_secs(3),
        "HDR proxy shutdown stalled"
    );
    assert_eq!(std::fs::read_dir(cancelled.path()).unwrap().count(), 0);
}

use slicer::{media::Binaries, preview::PreviewWorker};
use std::{process::Command, time::Duration};

#[test]
#[ignore = "requires SLICER_FFMPEG_DIR with bundled FFmpeg"]
fn seek_produces_distinct_png_frames_for_unicode_path() {
    let binaries = Binaries::resolve().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("preview café 日本.mkv");
    assert!(
        Command::new(&binaries.ffmpeg)
            .args([
                "-v",
                "error",
                "-f",
                "lavfi",
                "-i",
                "testsrc2=size=160x90:rate=10:duration=3",
                "-c:v",
                "mpeg4"
            ])
            .arg(&input)
            .status()
            .unwrap()
            .success()
    );
    let worker = PreviewWorker::new(binaries);
    worker.request(input.clone(), 0.2);
    let first = worker
        .events
        .recv_timeout(Duration::from_secs(20))
        .unwrap()
        .result
        .unwrap();
    worker.request(input, 1.8);
    let second = worker
        .events
        .recv_timeout(Duration::from_secs(20))
        .unwrap()
        .result
        .unwrap();
    assert!(first.starts_with(b"\x89PNG\r\n\x1a\n"));
    assert!(second.starts_with(b"\x89PNG\r\n\x1a\n"));
    assert_ne!(first, second, "Seeking must change the decoded frame");
}

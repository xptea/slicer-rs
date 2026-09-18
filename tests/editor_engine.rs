#![cfg(feature = "desktop")]
use slicer::engine::{
    decoder::Decoder,
    export::{ExportEvent, ExportHandle},
    project::*,
};
use std::{
    path::{Path, PathBuf},
    process::Command,
    time::{Duration, Instant},
};
fn fixture(dir: &Path) -> PathBuf {
    let bins = slicer::media::Binaries::resolve().expect("Provide SLICER_FFMPEG_DIR");
    let path = dir.join("moving.mp4");
    let output = Command::new(bins.ffmpeg)
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=320x180:rate=30",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:sample_rate=48000",
            "-t",
            "1.5",
            "-c:v",
            "mpeg4",
            "-c:a",
            "aac",
        ])
        .arg(&path)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    path
}
fn project(path: &Path) -> Project {
    let mut p = Project::default();
    p.width = 320;
    p.height = 180;
    p.next_id = 3;
    let c = Clip {
        id: 1,
        path: path.into(),
        start: 0,
        source_in: 0,
        duration: SECOND,
        source_duration: 1_500_000,
        visual: true,
        audio: true,
        still: false,
        transform: Transform::default(),
        gain: 0.5,
    };
    p.tracks[0].clips.push(c.clone());
    let mut overlay = c;
    overlay.id = 2;
    overlay.source_in = 300_000;
    overlay.transform.width = 0.4;
    overlay.transform.height = 0.4;
    overlay.transform.x = 0.7;
    overlay.transform.y = 0.3;
    overlay.transform.opacity = 0.7;
    p.tracks[1].clips.push(overlay);
    p
}
#[test]
#[ignore = "Requires FFmpeg fixture tools"]
fn persistent_decoder_seeks_and_audio() {
    let dir = tempfile::tempdir().unwrap();
    let path = fixture(dir.path());
    let mut d = Decoder::open(&path, false).unwrap();
    for time in [0, 700_000, 200_000, 1_200_000, 0] {
        let f = d.video(time).unwrap();
        assert!(
            f.pts <= time && f.pts + f.duration > time,
            "{time}: {} + {}",
            f.pts,
            f.duration
        );
        assert_eq!(f.width, 320);
        assert_eq!(f.pixels.len(), 320 * 180 + 160 * 90 * 2);
    }
    let mut d = Decoder::open(&path, true).unwrap();
    let mut samples = vec![0.; 1920];
    d.audio(300_000, &mut samples).unwrap();
    assert!(samples.iter().any(|x| x.abs() > 0.01));
    d.audio(0, &mut samples).unwrap();
    assert!(samples.iter().all(|x| x.is_finite()));
}
#[test]
#[ignore = "Requires X11/EGL, libmpv and FFmpeg tools"]
fn gpu_composition_exports_audio_and_preserves_existing_output() {
    let dir = tempfile::tempdir().unwrap();
    let path = fixture(dir.path());
    let p = project(&path);
    let output = dir.path().join("composition.mp4");
    let h = ExportHandle::spawn(p.clone(), output.clone()).unwrap();
    loop {
        match h.events.recv_timeout(Duration::from_secs(60)).unwrap() {
            ExportEvent::Progress(_) => {}
            ExportEvent::Finished(result) => {
                result.unwrap();
                break;
            }
        }
    }
    let mut d = Decoder::open(&output, false).unwrap();
    assert_eq!(d.info.width, 320);
    assert_eq!(d.info.height, 180);
    assert!(d.info.audio != 0);
    assert!((d.info.duration_us - SECOND).abs() < 100_000);
    let composed = d.video(0).unwrap();
    let base = Decoder::open(&path, false).unwrap().video(0).unwrap();
    let inside = (50 * 320 + 220) as usize;
    let outside = (160 * 320 + 10) as usize;
    assert!(
        rgb(&composed, outside)
            .iter()
            .zip(rgb(&base, outside))
            .all(|(a, b)| (*a - b).abs() < 0.08),
        "{:?} {:?}",
        rgb(&composed, outside),
        rgb(&base, outside)
    );
    assert_ne!(
        &composed.pixels[inside..inside + 20],
        &base.pixels[inside..inside + 20]
    );
    let before = std::fs::read(&output).unwrap();
    assert!(ExportHandle::spawn(p, output.clone()).is_err());
    assert_eq!(before, std::fs::read(output).unwrap());
    assert!(!std::fs::read_dir(dir.path()).unwrap().any(|e| {
        e.unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".slicer-")
    }));
}

fn rgb(f: &slicer::engine::decoder::Frame, index: usize) -> [f32; 3] {
    let w = f.width as usize;
    let h = f.height as usize;
    let uv = (index / w / 2) * w.div_ceil(2) + (index % w / 2);
    let y = (f.pixels[index] as f32 - 16.) / 219.;
    let u = (f.pixels[w * h + uv] as f32 - 128.) / 224.;
    let v = (f.pixels[w * h + w.div_ceil(2) * h.div_ceil(2) + uv] as f32 - 128.) / 224.;
    let (kr, kb) = if f.matrix == 1 {
        (0.2126, 0.0722)
    } else {
        (0.299, 0.114)
    };
    [
        y + 2. * (1. - kr) * v,
        y - 2. * kb * (1. - kb) / (1. - kr - kb) * u - 2. * kr * (1. - kr) / (1. - kr - kb) * v,
        y + 2. * (1. - kb) * u,
    ]
}

#[test]
#[ignore = "Requires X11/EGL, libmpv and FFmpeg tools"]
fn transparent_image_blends_in_linear_light_and_cancel_cleans_up() {
    let dir = tempfile::tempdir().unwrap();
    let bins = slicer::media::Binaries::resolve().unwrap();
    let video = dir.path().join("red.mp4");
    assert!(
        Command::new(bins.ffmpeg)
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-f",
                "lavfi",
                "-i",
                "color=c=red:size=320x180:rate=30",
                "-t",
                "1",
                "-c:v",
                "mpeg4"
            ])
            .arg(&video)
            .status()
            .unwrap()
            .success()
    );
    let png = dir.path().join("alpha.png");
    let mut image = image::RgbaImage::new(32, 32);
    for (x, _, pixel) in image.enumerate_pixels_mut() {
        *pixel = image::Rgba([0, 0, 255, if x < 16 { 0 } else { 128 }]);
    }
    image.save(&png).unwrap();
    let mut p = project(&video);
    p.tracks[0].clips[0].audio = false;
    let overlay = &mut p.tracks[1].clips[0];
    overlay.path = png;
    overlay.source_in = 0;
    overlay.still = true;
    overlay.audio = false;
    overlay.transform = Transform::default();
    let out = dir.path().join("alpha.mp4");
    let handle = ExportHandle::spawn(p.clone(), out.clone()).unwrap();
    loop {
        if let ExportEvent::Finished(r) =
            handle.events.recv_timeout(Duration::from_secs(30)).unwrap()
        {
            r.unwrap();
            break;
        }
    }
    let frame = Decoder::open(&out, false).unwrap().video(0).unwrap();
    let left = rgb(&frame, 90 * 320 + 40);
    let right = rgb(&frame, 90 * 320 + 280);
    assert!(left[0] > 0.9 && left[2] < 0.05, "{left:?}");
    assert!(
        (right[0] - 0.735).abs() < 0.06 && (right[2] - 0.735).abs() < 0.06,
        "{right:?}"
    );
    let cancelled = dir.path().join("cancelled.mp4");
    let handle = ExportHandle::spawn(p, cancelled.clone()).unwrap();
    handle.cancel();
    loop {
        if let ExportEvent::Finished(r) =
            handle.events.recv_timeout(Duration::from_secs(30)).unwrap()
        {
            assert!(r.unwrap_err().contains("cancelled"));
            break;
        }
    }
    drop(handle);
    assert!(!cancelled.exists());
    assert!(!std::fs::read_dir(dir.path()).unwrap().any(|e| {
        e.unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".slicer-")
    }));
}

#[test]
#[ignore = "Requires X11/EGL, libmpv and FFmpeg tools"]
fn opengl_two_videos_image_and_backward_scrubbing() {
    use slicer::engine::gl_canvas::Renderer;
    let dir = tempfile::tempdir().unwrap();
    let bins = slicer::media::Binaries::resolve().unwrap();
    let video = dir.path().join("colors.mp4");
    let raw = dir.path().join("colors.rgb");
    {
        use std::io::Write;
        let mut file = std::fs::File::create(&raw).unwrap();
        for color in [[255u8, 0, 0], [0, 255, 0], [0, 0, 255]] {
            let frame: Vec<_> = (0..320 * 180).flat_map(|_| color).collect();
            for _ in 0..30 {
                file.write_all(&frame).unwrap();
            }
        }
    }
    assert!(
        Command::new(bins.ffmpeg)
            .args([
                "-v",
                "error",
                "-f",
                "rawvideo",
                "-pixel_format",
                "rgb24",
                "-video_size",
                "320x180",
                "-framerate",
                "30",
                "-i"
            ])
            .arg(raw)
            .args(["-c:v", "mpeg4"])
            .arg(&video)
            .status()
            .unwrap()
            .success()
    );
    let png = dir.path().join("yellow.png");
    image::RgbaImage::from_pixel(32, 32, image::Rgba([255, 255, 0, 255]))
        .save(&png)
        .unwrap();
    let mut p = project(&video);
    for track in &mut p.tracks {
        for clip in &mut track.clips {
            clip.duration = 2 * SECOND;
            clip.source_duration = 3 * SECOND;
            clip.audio = false;
        }
    }
    let overlay = &mut p.tracks[1].clips[0];
    overlay.source_in = SECOND;
    overlay.transform.opacity = 1.;
    overlay.transform.rotation = 0.;
    let mut still = p.tracks[0].clips[0].clone();
    still.id = 3;
    still.path = png;
    still.still = true;
    still.transform.width = 0.2;
    still.transform.height = 0.2;
    still.transform.x = 0.15;
    still.transform.y = 0.8;
    p.tracks.push(Track {
        name: "Image".into(),
        muted: false,
        hidden: false,
        locked: false,
        clips: vec![still],
    });
    p.next_id = 4;
    let mut canvas = Renderer::new(0, 320, 180).unwrap();
    for (generation, time) in [200_000, 1_200_000, 200_000, 1_400_000, 400_000]
        .into_iter()
        .enumerate()
    {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let status = canvas
                .render(&p, time, generation as u64 + 1, false, [320, 180], None)
                .unwrap();
            if status.ready {
                break;
            }
            assert!(Instant::now() < deadline, "frame not ready: {status:?}");
            std::thread::sleep(Duration::from_millis(1));
        }
        let pixels = canvas.read_pixels([320, 180]);
        let at = |x: usize, y: usize| &pixels[(y * 320 + x) * 4..(y * 320 + x) * 4 + 3];
        let base = at(20, 20);
        let over = at(224, 54);
        let image = at(48, 144);
        if time < SECOND {
            assert!(base[0] > 220 && base[1] < 20, "base {time}: {base:?}");
            assert!(over[1] > 220 && over[2] < 20, "overlay {time}: {over:?}");
        } else {
            assert!(base[1] > 220 && base[0] < 20, "base {time}: {base:?}");
            assert!(over[2] > 220 && over[1] < 20, "overlay {time}: {over:?}");
        }
        assert!(
            image[0] > 240 && image[1] > 240 && image[2] < 10,
            "image {image:?}"
        );
    }
}

#[test]
#[ignore = "Requires X11/EGL, libmpv and FFmpeg tools"]
fn play_pause_and_continuous_playback_do_not_restart_decoders() {
    use slicer::engine::gl_canvas::Renderer;
    let dir = tempfile::tempdir().unwrap();
    let path = fixture(dir.path());
    let p = project(&path);
    let mut canvas = Renderer::new(0, 320, 180).unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    let initial = loop {
        let s = canvas
            .render(&p, 100_000, 1, false, [320, 180], None)
            .unwrap();
        if s.ready {
            break s.seeks;
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(1));
    };
    let mut cursor = 100_000;
    let mut longest = Duration::ZERO;
    for _ in 0..3 {
        let start = Instant::now();
        while start.elapsed() < Duration::from_millis(200) {
            let target = cursor + start.elapsed().as_micros() as i64;
            let began = Instant::now();
            let s = canvas
                .render(&p, target, 1, true, [320, 180], None)
                .unwrap();
            longest = longest.max(began.elapsed());
            assert_eq!(s.seeks, initial, "ordinary playback must not seek");
            std::thread::sleep(Duration::from_millis(4));
        }
        cursor += start.elapsed().as_micros() as i64;
        for _ in 0..20 {
            let s = canvas
                .render(&p, cursor, 1, false, [320, 180], None)
                .unwrap();
            assert_eq!(s.seeks, initial, "pause must not seek");
            std::thread::sleep(Duration::from_millis(4));
        }
    }
    let s = canvas
        .render(&p, cursor, 1, false, [320, 180], None)
        .unwrap();
    for (id, position, seeking, _) in &s.positions {
        let expected = cursor as f64 / 1e6 + if *id == 2 { 0.3 } else { 0. };
        assert!(!seeking);
        assert!(
            (position - expected).abs() < 0.12,
            "clock mismatch: {s:?}, expected {expected}"
        );
    }
    eprintln!("Play/pause: no decoder restarts; longest render call {longest:?}");
}

#[test]
#[ignore = "Requires X11/EGL, libmpv and FFmpeg tools"]
fn scrub_bursts_coalesce_and_release_wins() {
    use slicer::engine::gl_canvas::Preview;
    let dir = tempfile::tempdir().unwrap();
    let path = fixture(dir.path());
    let p = std::sync::Arc::new(project(&path));
    let preview = Preview::new(0);
    preview.request(p.clone(), 0, 1, false, [320, 180], None, false);
    let deadline = Instant::now() + Duration::from_secs(10);
    while !preview.status().ready {
        assert!(Instant::now() < deadline, "{:?}", preview.status());
        std::thread::sleep(Duration::from_millis(2));
    }
    for i in 0..80 {
        let time = if i < 40 {
            i * 20_000
        } else {
            (80 - i) * 20_000
        };
        preview.request(p.clone(), time, i as u64 + 2, false, [320, 180], None, true);
        std::thread::sleep(Duration::from_millis(2));
    }
    preview.request(p, 350_000, 100, false, [320, 180], None, false);
    loop {
        let status = preview.status();
        assert!(status.error.is_none(), "{status:?}");
        if status.generation == 100 && status.ready {
            for (id, position, seeking, _) in &status.positions {
                let target = if *id == 1 { 0.35 } else { 0.65 };
                assert!(!seeking);
                assert!((position - target).abs() < 0.04, "{status:?}");
            }
            assert!(
                status.seeks < 160,
                "drag requests should coalesce: {status:?}"
            );
            break;
        }
        assert!(
            Instant::now() < deadline,
            "release not completed: {status:?}"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
}

#[test]
#[ignore = "Requires X11/EGL, libmpv and FFmpeg tools"]
fn separated_clips_prewarm_and_never_publish_unready_frames() {
    use slicer::engine::gl_canvas::Renderer;
    let dir = tempfile::tempdir().unwrap();
    let path = fixture(dir.path());
    let mut p = project(&path);
    p.tracks[0].clips[0].duration = 500_000;
    p.tracks[1].clips[0].start = 1_000_000;
    p.tracks[1].clips[0].duration = 500_000;
    let mut r = Renderer::new(0, 320, 180).unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let s = r.render(&p, 100_000, 1, false, [320, 180], None).unwrap();
        if s.ready {
            break;
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(1));
    }
    // Prepare the next source while the current clip remains on screen.
    r.prewarm(&p, 100_000, [320, 180]).unwrap();
    for _ in 0..100 {
        r.render(&p, 100_000, 1, false, [320, 180], None).unwrap();
        std::thread::sleep(Duration::from_millis(2));
    }
    let begin = Instant::now();
    let s = r.render(&p, 1_000_000, 2, false, [320, 180], None).unwrap();
    assert!(s.ready, "preloaded boundary should already be ready: {s:?}");
    assert_eq!(s.opens, 2);
    assert_eq!(s.seeks, 0, "preloaded entry frame must not seek again");
    eprintln!("Preloaded separated clip switch: {:?}", begin.elapsed());
    let previous = r.read_pixels([320, 180]);
    // A playing jump to another source must not publish a blank/old source texture.
    let pending = r.render(&p, 300_000, 3, true, [320, 180], None).unwrap();
    if !pending.ready {
        assert!(!pending.redrawn);
        assert_eq!(previous, r.read_pixels([320, 180]));
    }
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let s = r.render(&p, 300_000, 3, false, [320, 180], None).unwrap();
        if s.ready {
            assert_eq!(s.opens, 2);
            break;
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(1));
    }
    // Empty space is intentionally black, rather than a stale previous clip.
    let gap = r.render(&p, 700_000, 4, false, [320, 180], None).unwrap();
    assert!(gap.ready);
    assert!(
        r.read_pixels([320, 180])
            .chunks_exact(4)
            .all(|p| p[..3] == [0, 0, 0])
    );
}

#[test]
#[ignore = "Requires X11/EGL, libmpv and FFmpeg tools"]
fn crossing_into_image_only_scene_does_not_wait_for_obsolete_video_seek() {
    use slicer::engine::gl_canvas::Preview;
    use std::sync::Arc;
    let dir = tempfile::tempdir().unwrap();
    let path = fixture(dir.path());
    let image_path = dir.path().join("still.png");
    image::RgbaImage::from_pixel(32, 18, image::Rgba([255, 200, 0, 255]))
        .save(&image_path)
        .unwrap();
    let mut p = project(&path);
    for clip in p.tracks.iter_mut().flat_map(|t| &mut t.clips) {
        clip.start = 1_000_000;
    }
    let mut still = p.tracks[0].clips[0].clone();
    still.id = 3;
    still.path = image_path;
    still.start = 0;
    still.duration = 3_000_000;
    still.still = true;
    still.audio = false;
    p.tracks[2].clips.push(still);
    let p = Arc::new(p);
    let preview = Preview::new(0);
    preview.request(p.clone(), 1_100_000, 1, false, [320, 180], None, false);
    let deadline = Instant::now() + Duration::from_secs(10);
    while !preview.status().ready {
        assert!(Instant::now() < deadline, "{:?}", preview.status());
        std::thread::sleep(Duration::from_millis(1));
    }
    let mut worst = Duration::ZERO;
    for i in 0..12 {
        let generation = 2 + i * 2;
        preview.request(
            p.clone(),
            1_100_000 + (i % 2) as i64 * 650_000,
            generation,
            false,
            [320, 180],
            None,
            true,
        );
        // Ensure the old sample has reached the worker before crossing the boundary.
        while preview.status().generation != generation {
            assert!(Instant::now() < deadline, "{:?}", preview.status());
            std::thread::sleep(Duration::from_millis(1));
        }
        let frames = preview.status().frames;
        let start = Instant::now();
        preview.request(
            p.clone(),
            500_000,
            generation + 1,
            false,
            [320, 180],
            None,
            true,
        );
        loop {
            let status = preview.status();
            assert!(status.error.is_none(), "{status:?}");
            if status.generation == generation + 1 && status.ready {
                assert!(status.positions.is_empty(), "obsolete videos: {status:?}");
                assert!(
                    status.frames > frames,
                    "new scene was not presented: {status:?}"
                );
                break;
            }
            assert!(
                start.elapsed() < Duration::from_millis(200),
                "image-only scene blocked behind obsolete video work: {status:?}"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
        worst = worst.max(start.elapsed());
    }
    eprintln!("Video overlap -> image-only request-to-present worst: {worst:?}");
}

#[test]
#[ignore = "Requires X11/EGL, libmpv and FFmpeg tools"]
fn releasing_at_completed_drag_target_reuses_exact_frame() {
    use slicer::engine::gl_canvas::Renderer;
    let dir = tempfile::tempdir().unwrap();
    let path = fixture(dir.path());
    let p = project(&path);
    let mut r = Renderer::new(0, 320, 180).unwrap();
    r.set_scrubbing(true);
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut generation = 0;
    for target in [0, 600_000, 200_000] {
        generation += 1;
        loop {
            let s = r
                .render(&p, target, generation, false, [320, 180], None)
                .unwrap();
            if s.ready {
                break;
            }
            assert!(Instant::now() < deadline, "{s:?}");
            std::thread::sleep(Duration::from_millis(1));
        }
    }
    let before = r
        .render(&p, 200_000, generation, false, [320, 180], None)
        .unwrap();
    let pixels = r.read_pixels([320, 180]);
    r.set_scrubbing(false);
    let released = r
        .render(&p, 200_000, generation + 1, false, [320, 180], None)
        .unwrap();
    assert!(
        released.ready,
        "release restarted an already exact seek: {released:?}"
    );
    assert_eq!(released.seeks, before.seeks);
    assert_eq!(pixels, r.read_pixels([320, 180]));
}

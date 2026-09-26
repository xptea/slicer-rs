#![cfg(feature = "desktop")]
use slicer::engine::{
    export::{ExportEvent, ExportHandle},
    gl_canvas::Renderer,
    project::*,
};
use std::time::{Duration, Instant};
#[test]
fn graphics_roundtrip_and_canvas_preserve_media_shape() {
    let mut p = Project::default();
    let id = p.add_graphic(Graphic::Text(TextStyle::default()), 0);
    let before = p.clip(id).unwrap().transform.width * p.width as f32
        / (p.clip(id).unwrap().transform.height * p.height as f32);
    p.resize_canvas(1080, 1920).unwrap();
    let after = p.clip(id).unwrap().transform.width * p.width as f32
        / (p.clip(id).unwrap().transform.height * p.height as f32);
    assert!((before - after).abs() < 0.001);
    assert!(p.resize_canvas(1001, 1000).is_err());
    p.clip_mut(id).unwrap().fade_in = SECOND;
    assert_eq!(p.clip(id).unwrap().fade(0), 0.);
    assert!((p.clip(id).unwrap().fade(SECOND / 2) - 0.5).abs() < 0.001);
    assert!(p.split(id, 2 * SECOND));
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("title.slicer");
    p.save(&path).unwrap();
    assert_eq!(Project::load(&path).unwrap(), p);
}
#[test]
#[ignore = "Requires GPU and bundled FFmpeg/mpv"]
fn text_background_fades_and_portrait_export_match_preview() {
    let mut p = Project::default();
    p.resize_canvas(320, 480).unwrap();
    p.fps_num = 10;
    p.add_graphic(
        Graphic::Color {
            color: [16, 40, 96, 255],
        },
        0,
    );
    let id = p.add_graphic(
        Graphic::Text(TextStyle {
            text: "TITLE\nHello!".into(),
            bold: true,
            ..TextStyle::default()
        }),
        0,
    );
    for c in p.tracks.iter_mut().flat_map(|t| &mut t.clips) {
        c.duration = 500_000;
    }
    p.clip_mut(id).unwrap().fade_in = 100_000;
    p.validate().unwrap();
    let mut r = Renderer::new(0, 320, 480).unwrap();
    assert!(
        r.render(&p, 200_000, 1, false, [320, 480], None)
            .unwrap()
            .ready
    );
    let expected = r.read_pixels([320, 480]);
    assert!(
        expected
            .chunks_exact(4)
            .filter(|p| p[0] > 180 && p[1] > 180 && p[2] > 180)
            .count()
            > 100
    );
    r.render(&p, 0, 2, false, [320, 480], None).unwrap();
    let faded = r.read_pixels([320, 480]);
    assert_ne!(expected, faded);
    assert!(faded.chunks_exact(4).all(|p| p[0] < 40));
    if let Some(Graphic::Text(t)) = &mut p.clip_mut(id).unwrap().graphic {
        t.text = "CHANGED".into();
    }
    r.render(&p, 200_000, 3, false, [320, 480], None).unwrap();
    assert_ne!(expected, r.read_pixels([320, 480]));
    if let Some(Graphic::Text(t)) = &mut p.clip_mut(id).unwrap().graphic {
        t.text = "TITLE\nHello!".into();
    }
    // Editing content while paused must repaint with no time/layout change.
    r.render(&p, 200_000, 3, false, [320, 480], None).unwrap();
    assert_eq!(expected, r.read_pixels([320, 480]));
    let dir = tempfile::tempdir().unwrap();
    let output = dir.path().join("text-portrait.mp4");
    let job = ExportHandle::spawn(p, output.clone()).unwrap();
    let began = Instant::now();
    loop {
        match job.events.recv_timeout(Duration::from_secs(1)) {
            Ok(ExportEvent::Finished(result)) => {
                result.unwrap();
                break;
            }
            _ => assert!(began.elapsed() < Duration::from_secs(20)),
        }
    }
    let mut decoder = slicer::engine::decoder::Decoder::open_preview(&output).unwrap();
    assert_eq!((decoder.info.width, decoder.info.height), (320, 480));
    let frame = decoder.video(200_000).unwrap();
    let mean = expected
        .iter()
        .zip(frame.pixels.iter())
        .map(|(a, b)| (*a as f64 - *b as f64).abs())
        .sum::<f64>()
        / expected.len() as f64;
    println!("Portrait text export mean pixel error: {mean:.3}");
    assert!(mean < 8.);
}

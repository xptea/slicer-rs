use slicer::engine::{decoder::Decoder, gl_canvas::Renderer, project::*};
use std::time::{Duration, Instant};
fn main() -> anyhow::Result<()> {
    let path = std::path::PathBuf::from(std::env::args_os().nth(1).expect("video path"));
    let mut d = Decoder::open_preview(&path)?;
    let f = d.video(1_000_000)?;
    let size = [f.width, f.height];
    let out = std::path::PathBuf::from("build/validation/scrub-visual");
    std::fs::create_dir_all(&out)?;
    image::save_buffer(
        out.join("decoded-original.png"),
        &f.pixels,
        size[0],
        size[1],
        image::ColorType::Rgba8,
    )?;
    let mut p = Project::default();
    p.tracks[0].clips.push(Clip {
        id: 1,
        path: path.clone(),
        start: 0,
        source_in: 0,
        duration: d.info.duration_us,
        source_duration: d.info.duration_us,
        visual: true,
        audio: false,
        still: false,
        transform: Transform::default(),
        gain: 1.,
        graphic: None,
        fade_in: 0,
        fade_out: 0,
    });
    let mut r = Renderer::new(0, size[0], size[1])?;
    r.enable_scrub_cache();
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let s = r.render(&p, 0, 0, false, size, None)?;
        if s.ready && s.scrub_status.contains("1 previews ready") {
            break;
        }
        anyhow::ensure!(Instant::now() < deadline, "{s:?}");
        std::thread::sleep(Duration::from_millis(2));
    }
    r.set_scrubbing(true);
    for (i, time) in [1_000_000, 2_000_000, 300_000, 1_000_000]
        .into_iter()
        .enumerate()
    {
        loop {
            let s = r.render(&p, time, i as u64 + 1, false, size, None)?;
            if s.ready {
                image::save_buffer(
                    out.join(format!("scrub-{i}.png")),
                    &r.read_pixels(size),
                    size[0],
                    size[1],
                    image::ColorType::Rgba8,
                )?;
                println!("{s:?}");
                break;
            }
            anyhow::ensure!(Instant::now() < deadline, "{s:?}");
            std::thread::sleep(Duration::from_millis(2));
        }
    }
    let proxy = std::fs::read_dir(std::env::var("SLICER_PROXY_DIR")?)?
        .flatten()
        .map(|e| e.path())
        .find(|p| {
            p.extension().is_some_and(|e| e == "mov") && !p.to_string_lossy().contains("partial")
        })
        .unwrap();
    let f = Decoder::open_preview(&proxy)?.video(1_000_000)?;
    image::save_buffer(
        out.join("decoded-proxy.png"),
        &f.pixels,
        f.width,
        f.height,
        image::ColorType::Rgba8,
    )?;
    let expected = image::open(out.join("decoded-proxy.png"))?.into_rgba8();
    for name in ["scrub-0.png", "scrub-3.png"] {
        let actual = image::open(out.join(name))?.into_rgba8();
        anyhow::ensure!(
            actual.dimensions() == expected.dimensions(),
            "frame dimensions differ"
        );
        let error = actual
            .as_raw()
            .iter()
            .zip(expected.as_raw())
            .map(|(a, b)| (*a as f64 - *b as f64).abs())
            .sum::<f64>()
            / expected.as_raw().len() as f64;
        anyhow::ensure!(error < 2., "GPU pixels differ from decoded pixels: {error}");
        println!("{name}: mean pixel error {error:.3}/255");
    }
    Ok(())
}

//! Run with: cargo run --example editor_engine_check -- /absolute/video.mp4
use slicer::engine::{decoder::Decoder, gl_canvas::Renderer, project::*};
use std::time::{Duration, Instant};
fn main() -> anyhow::Result<()> {
    let path = std::env::args_os()
        .nth(1)
        .map(std::path::PathBuf::from)
        .ok_or_else(|| anyhow::anyhow!("Pass a video file"))?;
    let d = Decoder::open(&path, false)?;
    let duration = d.info.duration_us;
    drop(d);
    let mut project = Project::default();
    project.next_id = 3;
    let layers = std::env::var("SLICER_BENCH_LAYERS")
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(2)
        .clamp(1, 2);
    for i in 0..layers {
        project.tracks[i].clips.push(Clip {
            id: i as u64 + 1,
            path: path.clone(),
            start: 0,
            source_in: 0,
            duration,
            source_duration: duration,
            visual: true,
            audio: false,
            still: false,
            transform: Transform::default(),
            gain: 1.,
            graphic: None,
            fade_in: 0,
            fade_out: 0,
        });
    }
    let mut canvas = Renderer::new(0, 1920, 1080)?;
    if std::env::var_os("SLICER_BENCH_PLAYBACK").is_some() {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let status = canvas.render(&project, 100_000, 1, false, [1920, 1080], None)?;
            if status.ready {
                break;
            }
            if Instant::now() > deadline {
                anyhow::bail!("warmup timed out");
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        let start = Instant::now();
        let mut durations = vec![];
        let mut last = Default::default();
        while start.elapsed() < Duration::from_secs(2) {
            let target = 100_000 + start.elapsed().as_micros() as i64;
            let began = Instant::now();
            last = canvas.render(&project, target, 1, true, [1920, 1080], None)?;
            durations.push(began.elapsed().as_secs_f64() * 1000.);
            std::thread::sleep(Duration::from_millis(4));
        }
        durations.sort_by(f64::total_cmp);
        println!(
            "{layers} layers, playback render calls: p95={:.2}ms max={:.2}ms; {:?}",
            durations[durations.len() * 95 / 100],
            durations.last().unwrap(),
            last
        );
        return Ok(());
    }
    let mut times = vec![];
    for i in 0..100 {
        let time = ((i * 7919) % 100) as i64 * (duration - 1000) / 100;
        let start = Instant::now();
        loop {
            let status = canvas.render(&project, time, i as u64 + 1, false, [1920, 1080], None)?;
            if status.ready {
                break;
            }
            if start.elapsed() > Duration::from_secs(10) {
                anyhow::bail!("Seek {i} at {time} timed out: {status:?}");
            }
            std::thread::sleep(Duration::from_micros(100));
        }
        times.push(start.elapsed().as_secs_f64() * 1000.);
    }
    times.sort_by(f64::total_cmp);
    println!(
        "{layers}-layer libmpv/OpenGL seek-to-render completion (excludes screen presentation): p50={:.2}ms p95={:.2}ms p99={:.2}ms",
        times[50], times[95], times[99]
    );
    Ok(())
}

//! Compare random scrub targets against the original exact libmpv seek path.
use slicer::engine::{decoder::Decoder, gl_canvas::Renderer, project::*};
use std::time::{Duration, Instant};
fn main() -> anyhow::Result<()> {
    let path = std::path::PathBuf::from(
        std::env::args_os()
            .nth(1)
            .expect("video or .slicer project path"),
    );
    let project_input = path
        .extension()
        .is_some_and(|e| e == "slicer" || e == "json");
    let duration = if project_input {
        Project::load(&path)?.duration()
    } else {
        Decoder::open(&path, false)?.info.duration_us
    };
    let layers = std::env::var("SLICER_BENCH_LAYERS")
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(1)
        .clamp(1, 2);
    let mut p = Project::default();
    for i in 0..layers {
        p.tracks[i].clips.push(Clip {
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
    if project_input {
        p = Project::load(&path)?;
    }
    let label = if project_input {
        format!(
            "{}-clip project",
            p.tracks.iter().map(|t| t.clips.len()).sum::<usize>()
        )
    } else {
        format!("{layers} layers")
    };
    let proxy_count = p
        .active(0)
        .filter(|(t, c, _)| !t.hidden && c.visual && !c.still)
        .map(|(_, c, _)| &c.path)
        .collect::<std::collections::HashSet<_>>()
        .len();
    let mut r = Renderer::new(0, 1920, 1080)?;
    r.enable_scrub_cache();
    let preparing = Instant::now();
    let deadline = preparing + Duration::from_secs(300);
    loop {
        let s = r.render(&p, 0, 0, false, [1920, 1080], None)?;
        if s.ready
            && s.scrub_status
                .contains(&format!("{proxy_count} previews ready"))
        {
            break;
        }
        anyhow::ensure!(Instant::now() < deadline, "proxy timeout: {s:?}");
        std::thread::sleep(Duration::from_millis(10));
    }
    println!(
        "Preview preparation: {:.2}s",
        preparing.elapsed().as_secs_f64()
    );
    r.set_scrubbing(true);
    for pass in 0..2 {
        let mut times = vec![];
        for i in 0..100 {
            let time = ((i * 7919) % 100) as i64 * (duration - 1000) / 100;
            let start = Instant::now();
            loop {
                let s = r.render(&p, time, i as u64 + 1, false, [1920, 1080], None)?;
                if s.ready {
                    break;
                }
                anyhow::ensure!(
                    start.elapsed() < Duration::from_secs(10),
                    "scrub timeout {time}: {s:?}"
                );
                std::thread::sleep(Duration::from_micros(100));
            }
            times.push(start.elapsed().as_secs_f64() * 1000.);
        }
        times.sort_by(f64::total_cmp);
        println!(
            "{label}, pass {pass} scrub-to-render (excludes presentation): p50={:.2}ms p95={:.2}ms p99={:.2}ms",
            times[50], times[95], times[99]
        );
    }
    r.set_scrubbing(false);
    let release_time = 3_123_456.min((duration - 1).max(0));
    let began = Instant::now();
    loop {
        let s = r.render(&p, release_time, 999, false, [1920, 1080], None)?;
        if s.ready {
            println!(
                "Release resolved original in {:.2}ms; {:?}",
                began.elapsed().as_secs_f64() * 1000.,
                s.decoders
            );
            anyhow::ensure!(s.positions.iter().all(|(id, t, _, _)| {
                p.clip(*id)
                    .and_then(|c| c.source_time(release_time))
                    .is_some_and(|target| (*t - target as f64 / 1e6).abs() < 0.05)
            }));
            break;
        }
        anyhow::ensure!(
            began.elapsed() < Duration::from_secs(10),
            "release timeout: {s:?}"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
    Ok(())
}

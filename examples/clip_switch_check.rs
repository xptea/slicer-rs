use slicer::engine::{gl_canvas::Renderer, project::*};
use std::time::{Duration, Instant};
fn main() -> anyhow::Result<()> {
    let path = std::env::args().nth(1).expect("project path");
    let p = Project::load(std::path::Path::new(&path))?;
    let mut r = Renderer::new(0, 1920, 1080)?;
    if std::env::var_os("SLICER_BENCH_PREWARM").is_some() {
        for _ in 0..150 {
            r.render(&p, 0, 0, false, [1920, 1080], None)?;
            r.prewarm(&p, 0, [1920, 1080])?;
            std::thread::sleep(Duration::from_millis(2));
        }
    }
    let mut targets = vec![0];
    let mut clips: Vec<_> = p
        .tracks
        .iter()
        .flat_map(|t| &t.clips)
        .filter(|c| c.visual)
        .collect();
    clips.sort_by_key(|c| c.start);
    for c in clips {
        targets.extend([c.start, c.start + c.duration / 2]);
    }
    let reverse: Vec<_> = targets.iter().rev().copied().collect();
    targets.extend(reverse);
    for (i, time) in targets.into_iter().enumerate() {
        let start = Instant::now();
        loop {
            let s = r.render(&p, time, i as u64 + 1, false, [1920, 1080], None)?;
            if s.ready {
                println!(
                    "{time}: {:.1}ms {:?}",
                    start.elapsed().as_secs_f64() * 1000.,
                    s.positions
                );
                break;
            }
            if start.elapsed() > Duration::from_secs(5) {
                anyhow::bail!("timeout at {time}: {s:?}");
            }
            std::thread::sleep(Duration::from_millis(1));
        }
    }
    Ok(())
}

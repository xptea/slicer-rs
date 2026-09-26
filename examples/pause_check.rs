use slicer::engine::{decoder::Decoder, gl_canvas::Renderer, project::*};
use std::time::{Duration, Instant};
fn main() -> anyhow::Result<()> {
    let path = std::path::PathBuf::from(std::env::args_os().nth(1).unwrap());
    let duration = Decoder::open(&path, false)?.info.duration_us;
    let mut p = Project::default();
    p.tracks[0].clips.push(Clip {
        id: 1,
        path,
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
    let mut r = Renderer::new(0, 640, 360)?;
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let s = r.render(&p, 0, 1, false, [640, 360], None)?;
        if s.ready {
            break;
        }
        anyhow::ensure!(Instant::now() < deadline, "{s:?}");
        std::thread::sleep(Duration::from_millis(2));
    }
    let mut time = 0;
    for round in 0..3 {
        let start = Instant::now();
        while start.elapsed() < Duration::from_millis(400) {
            let t = time + start.elapsed().as_micros() as i64;
            let s = r.render(&p, t, 1, true, [640, 360], None)?;
            if start.elapsed() > Duration::from_millis(350) {
                println!("round {round} playing target={t} {:?}", s.positions);
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        time += 400_000;
        let paused = Instant::now();
        while paused.elapsed() < Duration::from_secs(2) {
            let s = r.render(&p, time, 1, false, [640, 360], None)?;
            if paused.elapsed() > Duration::from_millis(1900) {
                println!("round {round} paused target={time} {:?}", s.positions);
            }
            std::thread::sleep(Duration::from_millis(25));
        }
    }
    Ok(())
}

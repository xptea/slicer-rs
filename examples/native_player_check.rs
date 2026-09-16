//! Short, disposable X11 smoke test for the native libmpv player.
//!
//! This example owns a real X11 window so it exercises the same `wid` path as
//! the GPUI preview. It intentionally uses `NativePlayer::new`, which resolves
//! the packaged runtime or the explicit `SLICER_MPV_LIBRARY` development
//! override. No image extraction or subprocess playback is involved.

#[cfg(target_os = "linux")]
mod linux {
    use slicer::native_player::{NativePlayer, NativePlayerSnapshot};
    use std::error::Error;
    use std::path::{Path, PathBuf};
    use std::thread;
    use std::time::{Duration, Instant};
    use x11rb::COPY_DEPTH_FROM_PARENT;
    use x11rb::connection::Connection;
    use x11rb::protocol::xproto::{ConnectionExt, CreateWindowAux, EventMask, Window, WindowClass};
    use x11rb::rust_connection::RustConnection;

    const WIDTH: u16 = 1280;
    const HEIGHT: u16 = 720;
    const POLL_INTERVAL: Duration = Duration::from_millis(15);
    const SCRUB_STRESS_DURATION: Duration = Duration::from_secs(32);
    const SCRUB_STRESS_BURST_SEEKS: usize = 48;

    struct DisposableWindow {
        connection: RustConnection,
        window: Window,
    }

    impl DisposableWindow {
        fn create() -> Result<Self, Box<dyn Error>> {
            let (connection, screen_index) = x11rb::connect(None)?;
            let screen = connection
                .setup()
                .roots
                .get(screen_index)
                .ok_or("X11 screen index is unavailable")?;
            let window = connection.generate_id()?;
            connection
                .create_window(
                    COPY_DEPTH_FROM_PARENT,
                    window,
                    screen.root,
                    0,
                    0,
                    WIDTH,
                    HEIGHT,
                    0,
                    WindowClass::INPUT_OUTPUT,
                    0,
                    &CreateWindowAux::new()
                        .background_pixel(screen.black_pixel)
                        .event_mask(EventMask::EXPOSURE | EventMask::STRUCTURE_NOTIFY),
                )?
                .check()?;
            connection.map_window(window)?.check()?;
            connection.flush()?;
            Ok(Self { connection, window })
        }

        fn id(&self) -> u64 {
            u64::from(self.window)
        }
    }

    impl Drop for DisposableWindow {
        fn drop(&mut self) {
            // The caller keeps NativePlayer in an inner scope, so this XID is
            // destroyed only after libmpv has joined its worker and detached.
            let _ = self.connection.destroy_window(self.window);
            let _ = self.connection.flush();
        }
    }

    fn wait_for(
        player: &NativePlayer,
        timeout: Duration,
        predicate: impl Fn(&NativePlayerSnapshot) -> bool,
    ) -> Result<NativePlayerSnapshot, Box<dyn Error>> {
        let began = Instant::now();
        loop {
            let snapshot = player.poll();
            if let Some(error) = snapshot.error.as_deref() {
                return Err(format!("libmpv playback error: {error}").into());
            }
            if predicate(&snapshot) {
                return Ok(snapshot);
            }
            if began.elapsed() >= timeout {
                return Err(format!("timed out waiting for native player: {snapshot:?}").into());
            }
            thread::sleep(POLL_INTERVAL);
        }
    }

    fn print_snapshot(label: &str, snapshot: &NativePlayerSnapshot) {
        println!(
            "{label}: position={:.3}s duration={:.3}s paused={} seeking={} loaded={} eof={} \
             source={}x{} display_fps={:?} estimated_vf_fps={:?} hwdec={:?} vo={:?} \
             audio_codec={:?} audio_params={:?} audio_active={:?} decoder_drops={:?}",
            snapshot.position,
            snapshot.duration,
            snapshot.paused,
            snapshot.seeking,
            snapshot.loaded,
            snapshot.eof,
            snapshot
                .video_width
                .map_or_else(|| "?".to_owned(), |value| value.to_string()),
            snapshot
                .video_height
                .map_or_else(|| "?".to_owned(), |value| value.to_string()),
            snapshot.display_fps,
            snapshot.estimated_vf_fps,
            snapshot.hwdec,
            snapshot.video_output,
            snapshot.audio_codec,
            snapshot.audio_params,
            snapshot.audio_active,
            snapshot.decoder_frame_drop_count,
        );
    }

    #[derive(Clone, Copy)]
    struct StressRng(u64);

    impl StressRng {
        fn next(&mut self) -> u64 {
            // A deterministic generator keeps a failing sequence reproducible
            // without adding a runtime dependency to this diagnostic.
            self.0 = self
                .0
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            self.0
        }

        fn unit(&mut self) -> f64 {
            (self.next() >> 11) as f64 / (1_u64 << 53) as f64
        }
    }

    fn flush_stdout() {
        use std::io::Write;
        let _ = std::io::stdout().flush();
    }

    fn run_scrub_stress(
        player: &NativePlayer,
        loaded: &NativePlayerSnapshot,
    ) -> Result<(), Box<dyn Error>> {
        let max_target = (loaded.duration - 0.02).max(0.0);
        if max_target <= 0.2 {
            return Err("scrub stress requires a media file longer than 0.2s".into());
        }
        let fps = loaded
            .estimated_vf_fps
            .or(loaded.display_fps)
            .filter(|value| value.is_finite() && *value > 1.0)
            .unwrap_or(30.0);
        // VFR recordings can have a larger timestamp gap than 1/fps. Keep
        // this diagnostic aligned with the UI's 250 ms VFR settle allowance,
        // while still rejecting keyframe-only jumps across this short file.
        let tolerance = (1.0 / fps + 0.01).max(0.25);
        let began = Instant::now();
        let mut rng = StressRng(0x6e617469_76655f73);
        let mut bursts = 0_u64;
        let mut total_seeks = 0_u64;
        let mut latency_sum = Duration::ZERO;
        let mut latency_max = Duration::ZERO;

        println!(
            "scrub_stress: duration_s={:.3} target_tolerance_s={tolerance:.3} runtime_s={}",
            loaded.duration,
            SCRUB_STRESS_DURATION.as_secs()
        );
        flush_stdout();

        while began.elapsed() < SCRUB_STRESS_DURATION {
            let origin = rng.unit() * max_target;
            let target = rng.unit() * max_target;
            player.set_paused(true)?;

            let burst_started = Instant::now();
            for index in 0..SCRUB_STRESS_BURST_SEEKS {
                let progress = index as f64 / (SCRUB_STRESS_BURST_SEEKS - 1) as f64;
                // Add a small deterministic wobble around the forward or
                // backward path to exercise both directions and latest-wins
                // replacement rather than replaying a linear sequence.
                let wobble = (rng.unit() - 0.5) * (max_target * 0.08);
                let seek_target =
                    (origin + (target - origin) * progress + wobble).clamp(0.0, max_target);
                player.seek(seek_target, false)?;
                total_seeks += 1;
                thread::sleep(Duration::from_millis(4));
            }

            // The release target is authoritative and bypasses the drag
            // cadence. The worker must finish this target before the next
            // randomized burst begins.
            let exact_started = Instant::now();
            player.seek(target, true)?;
            let exact = wait_for(player, Duration::from_secs(3), |snapshot| {
                snapshot.loaded
                    && snapshot.paused
                    && !snapshot.seeking
                    && (snapshot.position - target).abs() <= tolerance
            })?;
            let latency = exact_started.elapsed();
            latency_sum += latency;
            latency_max = latency_max.max(latency);
            bursts += 1;

            if bursts.is_multiple_of(5) {
                println!(
                    "scrub_stress: burst={bursts} seeks={total_seeks} elapsed_s={:.1} burst_ms={} exact_ms={} position={:.3}",
                    began.elapsed().as_secs_f64(),
                    burst_started.elapsed().as_millis(),
                    latency.as_millis(),
                    exact.position
                );
                flush_stdout();
            }

            // Briefly run between drag gestures so transport pause/resume and
            // decoder restart paths are exercised too.
            player.set_paused(false)?;
            thread::sleep(Duration::from_millis(80));
        }

        let average_ms = if bursts == 0 {
            0.0
        } else {
            latency_sum.as_secs_f64() * 1000.0 / bursts as f64
        };
        println!(
            "scrub_stress: completed bursts={bursts} approximate_seeks={total_seeks} elapsed_s={:.1} average_exact_ms={average_ms:.1} max_exact_ms={} \
             player_running={}",
            began.elapsed().as_secs_f64(),
            latency_max.as_millis(),
            player.is_running()
        );
        flush_stdout();
        Ok(())
    }

    fn run_end_check(
        player: &NativePlayer,
        path: &Path,
        loaded: &NativePlayerSnapshot,
    ) -> Result<(), Box<dyn Error>> {
        let duration = loaded.duration;
        if !duration.is_finite() || duration <= 0.5 {
            return Err("end check requires a media file longer than 0.5s".into());
        }
        let fps = loaded
            .estimated_vf_fps
            .or(loaded.display_fps)
            .filter(|value| value.is_finite() && *value > 1.0)
            .unwrap_or(30.0);
        let tolerance = (1.0 / fps + 0.01).max(0.03);

        // The player was loaded through `load`, so this is the unrestricted
        // full-length path that emits MPV_EVENT_END_FILE at natural EOF.
        let eof_started = Instant::now();
        player.set_paused(false)?;
        let ended = wait_for(
            player,
            Duration::from_secs_f64(duration + 5.0),
            |snapshot| {
                snapshot.loaded
                    && snapshot.paused
                    && snapshot.eof
                    && !snapshot.seeking
                    && (snapshot.position - duration).abs() <= tolerance
            },
        )?;
        print_snapshot("natural_eof", &ended);
        println!(
            "natural_eof_latency_ms={} terminal_error={:?}",
            eof_started.elapsed().as_millis(),
            ended.error
        );

        // A second play at EOF must visibly leave the terminal state from the
        // beginning, rather than briefly unpausing and jumping back to EOF.
        let replay_started = Instant::now();
        player.set_paused(false)?;
        let replaying = wait_for(player, Duration::from_secs(3), |snapshot| {
            snapshot.loaded
                && !snapshot.paused
                && !snapshot.eof
                && !snapshot.seeking
                && snapshot.position < duration - 0.1
        })?;
        print_snapshot("natural_replay", &replaying);
        println!(
            "natural_replay_latency_ms={}",
            replay_started.elapsed().as_millis()
        );
        player.set_paused(true)?;
        let _ = wait_for(player, Duration::from_secs(2), |snapshot| {
            snapshot.loaded && snapshot.paused && !snapshot.seeking
        })?;

        // Repeat the assertion with a selected subrange. This covers the
        // editor's range stop and verifies replay starts at that range's
        // beginning as well as at absolute zero for natural EOF.
        let range_start = (duration * 0.2).min(duration - 0.3);
        let range_end = (duration * 0.6).max(range_start + 0.2).min(duration);
        player.load_file(path, range_start, range_end)?;
        let _ = wait_for(player, Duration::from_secs(3), |snapshot| {
            snapshot.loaded
                && snapshot.paused
                && !snapshot.seeking
                && (snapshot.position - range_start).abs() <= tolerance
        })?;
        player.set_paused(false)?;
        let range_ended = wait_for(player, Duration::from_secs(5), |snapshot| {
            snapshot.loaded
                && snapshot.paused
                && snapshot.eof
                && !snapshot.seeking
                && (snapshot.position - range_end).abs() <= tolerance
        })?;
        print_snapshot("range_eof", &range_ended);
        println!(
            "range_eof_target_s={range_end:.3} range_error_s={:.6}",
            range_ended.position - range_end
        );
        player.set_paused(false)?;
        let range_replaying = wait_for(player, Duration::from_secs(3), |snapshot| {
            snapshot.loaded
                && !snapshot.paused
                && !snapshot.eof
                && !snapshot.seeking
                && snapshot.position < range_end - 0.1
                && snapshot.position >= range_start - tolerance
        })?;
        print_snapshot("range_replay", &range_replaying);
        player.set_paused(true)?;
        Ok(())
    }

    fn run(path: &Path, stress: bool, end_check: bool) -> Result<(), Box<dyn Error>> {
        if !path.is_file() {
            return Err(format!("fixture is not a regular file: {}", path.display()).into());
        }
        println!(
            "native_player_check: DISPLAY={:?} SLICER_MPV_LIBRARY={:?} fixture={}",
            std::env::var_os("DISPLAY"),
            std::env::var_os("SLICER_MPV_LIBRARY"),
            path.display()
        );

        let window = DisposableWindow::create()?;
        // Keep the player in this inner scope. Its Drop joins the worker before
        // DisposableWindow drops and destroys the X11 child drawable.
        {
            let player = NativePlayer::new(window.id())?;
            player.load(path)?;
            let loaded = wait_for(&player, Duration::from_secs(5), |snapshot| {
                snapshot.loaded && snapshot.duration > 0.0
            })?;
            print_snapshot("loaded", &loaded);

            if stress {
                run_scrub_stress(&player, &loaded)?;
            } else if end_check {
                run_end_check(&player, path, &loaded)?;
            } else {
                player.set_paused(false)?;
                let before_play = player.poll();
                let advanced = wait_for(&player, Duration::from_secs(3), |snapshot| {
                    snapshot.loaded
                        && !snapshot.paused
                        && snapshot.position > before_play.position + 0.05
                })?;
                print_snapshot("advanced", &advanced);

                // Let the native transport run for roughly two seconds from the
                // first observed advancing point, then pause before seeking.
                thread::sleep(Duration::from_secs(2));
                player.set_paused(true)?;
                let paused = wait_for(&player, Duration::from_secs(2), |snapshot| {
                    snapshot.loaded && snapshot.paused && !snapshot.seeking
                })?;
                print_snapshot("paused_after_two_seconds", &paused);

                let fps = paused
                    .estimated_vf_fps
                    .or(paused.display_fps)
                    .filter(|value| value.is_finite() && *value > 1.0)
                    .unwrap_or(60.0);
                let tolerance = 1.0 / fps + 0.01;
                let interactive_target = 0.75_f64.min(paused.duration / 2.0);
                let interactive_started = Instant::now();
                player.seek(interactive_target, false)?;
                let interactive = wait_for(&player, Duration::from_secs(3), |snapshot| {
                    !snapshot.seeking && (snapshot.position - interactive_target).abs() <= tolerance
                })?;
                println!(
                    "interactive_seek_latency_ms={}",
                    interactive_started.elapsed().as_millis()
                );
                print_snapshot("interactive_non_keyframe", &interactive);

                // Simulate a fast timeline drag. These requests are high
                // resolution too, but the native worker keeps only the newest
                // target and paces decoder restarts. The release request below
                // must supersede the entire burst immediately.
                let drag_started = Instant::now();
                let drag_end = (paused.duration - 0.02).max(0.0);
                for index in 0..48 {
                    let fraction = f64::from(index) / 47.0;
                    player.seek(drag_end * fraction, false)?;
                    thread::sleep(Duration::from_millis(4));
                }
                let drag_elapsed = drag_started.elapsed();
                let exact_target = 1.25_f64.min(drag_end);
                let seek_started = Instant::now();
                player.seek(exact_target, true)?;
                let exact = wait_for(&player, Duration::from_secs(3), |snapshot| {
                    snapshot.loaded
                        && snapshot.paused
                        && !snapshot.seeking
                        && (snapshot.position - exact_target).abs() <= tolerance
                })?;
                println!(
                    "drag_requests=48 drag_elapsed_ms={} exact_seek_latency_ms={}",
                    drag_elapsed.as_millis(),
                    seek_started.elapsed().as_millis()
                );
                print_snapshot(&format!("exact_seek_{exact_target:.2}"), &exact);
                if (exact.position - exact_target).abs() > tolerance {
                    return Err(format!(
                        "exact seek landed at {:.6}s, target {:.6}s, outside tolerance {:.6}s",
                        exact.position, exact_target, tolerance
                    )
                    .into());
                }
            }
        }
        println!("native_player_check: player dropped cleanly");
        Ok(())
    }

    pub fn main() -> Result<(), Box<dyn Error>> {
        let mut stress = false;
        let mut end_check = false;
        let mut path = None;
        for argument in std::env::args_os().skip(1) {
            if argument == "--stress" {
                stress = true;
            } else if argument == "--end-check" {
                end_check = true;
            } else if path.is_none() {
                path = Some(PathBuf::from(argument));
            } else {
                return Err(
                    "usage: cargo run --example native_player_check -- [--stress|--end-check] <media-file>"
                        .into(),
                );
            }
        }
        if stress && end_check {
            return Err("choose either --stress or --end-check".into());
        }
        let path = path.ok_or(
            "usage: cargo run --example native_player_check -- [--stress|--end-check] <media-file>",
        )?;
        run(&path, stress, end_check)
    }
}

#[cfg(target_os = "linux")]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    linux::main()
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("native_player_check currently requires Linux/X11");
}

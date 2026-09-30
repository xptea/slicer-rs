use slicer::native_player::NativePlayer;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

fn explicit_library() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("SLICER_MPV_LIBRARY") {
        // An explicit override is an acceptance-test input. Keep an invalid
        // path visible to the constructor so the test fails with the actual
        // initialization error instead of silently skipping.
        return Some(PathBuf::from(path));
    }
    [
        Path::new("/usr/lib/x86_64-linux-gnu/libmpv.so.2"),
        Path::new("/usr/lib/aarch64-linux-gnu/libmpv.so.2"),
        Path::new("/usr/local/lib/libmpv.so.2"),
    ]
    .iter()
    .find(|path| path.is_file())
    .map(|path| path.to_path_buf())
}

fn fixture() -> Option<PathBuf> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("build/validation/native-library/Native 1080p60 日本.mp4");
    path.is_file().then_some(path)
}

fn native_test_player() -> Option<(NativePlayer, PathBuf)> {
    let library = explicit_library()?;
    if std::env::var_os("SLICER_MPV_LIBRARY").is_some() {
        assert!(
            library.is_file(),
            "SLICER_MPV_LIBRARY must point to a regular file: {}",
            library.display()
        );
    }
    let fixture = fixture()?;
    let player = NativePlayer::new_with_library(0, &library)
        .unwrap_or_else(|error| panic!("libmpv initialization failed: {error}"));
    Some((player, fixture))
}

fn wait_until(
    player: &NativePlayer,
    timeout: Duration,
    predicate: impl Fn(&slicer::native_player::NativePlayerSnapshot) -> bool,
) -> bool {
    let began = Instant::now();
    while began.elapsed() < timeout {
        let snapshot = player.poll();
        if predicate(&snapshot) {
            return true;
        }
        if snapshot.error.is_some() {
            return false;
        }
        thread::sleep(Duration::from_millis(15));
    }
    false
}

#[test]
fn persistent_player_loads_unicode_file_and_seeks_without_restart() {
    let Some((player, path)) = native_test_player() else {
        eprintln!("libmpv or native fixture unavailable; skipping");
        return;
    };
    player
        .load_file(&path, 0.4, 2.0)
        .expect("valid fixture should enqueue");
    assert!(wait_until(&player, Duration::from_secs(5), |snapshot| {
        // Metadata observations can arrive after FILE_LOADED.
        snapshot.loaded
            && snapshot.position >= 0.25
            && snapshot.video_width.is_some()
            && snapshot.video_height.is_some()
    }));
    let first = player.poll();
    assert!(first.error.is_none(), "{first:?}");
    assert_eq!(first.video_width, Some(1920));
    assert_eq!(first.video_height, Some(1080));

    player.set_mute(true).expect("mute should enqueue");
    assert!(player.poll().muted);
    player.set_mute(false).expect("unmute should enqueue");

    // Capture the loaded position before resuming. A fixed `position > 0.05`
    // assertion would pass immediately because this load intentionally starts
    // at 0.4 seconds, even if the decoder never advances.
    let loaded_position = first.position;
    player.set_paused(false).expect("resume should enqueue");
    assert!(
        wait_until(&player, Duration::from_secs(2), |snapshot| {
            snapshot.loaded && !snapshot.paused && snapshot.position > loaded_position + 0.05
        }),
        "native playback did not advance from {loaded_position:.3}s: {:?}",
        player.poll()
    );

    // Pause and verify that transport remains stable for several event-loop
    // ticks. This catches a command that only updates the optimistic snapshot
    // while mpv continues decoding.
    player.set_paused(true).expect("pause should enqueue");
    assert!(
        wait_until(&player, Duration::from_secs(2), |snapshot| {
            snapshot.loaded && snapshot.paused && !snapshot.seeking
        }),
        "native playback did not pause: {:?}",
        player.poll()
    );
    let paused_position = player.poll().position;
    thread::sleep(Duration::from_millis(180));
    let still_paused = player.poll();
    assert!(
        still_paused.paused,
        "paused playback resumed: {still_paused:?}"
    );
    let pause_tolerance = 1.0 / 60.0 + 0.01;
    assert!(
        (still_paused.position - paused_position).abs() <= pause_tolerance,
        "paused playback moved from {paused_position:.6} to {:.6} (tolerance {pause_tolerance:.4})",
        still_paused.position,
    );

    // A drag request must land between keyframes too, not just the final release.
    player.seek(0.75, false).expect("drag seek should enqueue");
    assert!(
        wait_until(&player, Duration::from_secs(3), |snapshot| {
            !snapshot.seeking && (snapshot.position - 0.75).abs() < 0.027
        }),
        "drag seek did not decode the requested frame: {:?}",
        player.poll()
    );

    // A burst of interactive requests remains bounded in the one pending
    // seek slot. The final exact request supersedes the burst.
    for index in 0..80 {
        player
            .seek(0.2 + index as f64 * 0.005, false)
            .expect("coalesced seek should not fill a queue");
    }
    player.seek(1.25, true).expect("exact seek should enqueue");
    let seek_snapshot = player.poll();
    let fps = seek_snapshot
        .estimated_vf_fps
        .or(seek_snapshot.display_fps)
        .filter(|fps| fps.is_finite() && *fps > 1.0)
        .unwrap_or(60.0);
    let exact_tolerance = 1.0 / fps + 0.01;
    assert!(
        wait_until(&player, Duration::from_secs(3), |snapshot| {
            snapshot.loaded
                && snapshot.paused
                && !snapshot.seeking
                && (snapshot.position - 1.25).abs() <= exact_tolerance
        }),
        "exact seek missed target 1.25s within {exact_tolerance:.4}s: {:?}",
        player.poll()
    );
    let exact = player.poll();
    assert!(
        (exact.position - 1.25).abs() <= exact_tolerance,
        "{exact:?}"
    );

    // Reuse the same mpv handle for another load, then change the active range
    // before resuming. This exercises reload and range updates without making
    // a second worker or a second native renderer.
    player
        .load_file(&path, 0.8, 1.4)
        .expect("fixture reload should enqueue");
    assert!(
        wait_until(&player, Duration::from_secs(5), |snapshot| {
            snapshot.loaded
                && snapshot.paused
                && !snapshot.seeking
                && (snapshot.position - 0.8).abs() <= exact_tolerance
        }),
        "reloaded fixture did not reach its start position: {:?}",
        player.poll()
    );
    assert!(
        player.poll().error.is_none(),
        "reloaded fixture failed: {:?}",
        player.poll()
    );
    player
        .set_range(0.85, 1.0)
        .expect("range update should enqueue");
    player
        .set_paused(false)
        .expect("resume after reload should enqueue");
    assert!(
        wait_until(&player, Duration::from_secs(3), |snapshot| {
            snapshot.paused && snapshot.position >= 0.985
        }),
        "updated range did not stop playback: {:?}",
        player.poll()
    );
    assert!(player.is_running());
}

#[test]
fn range_stop_pauses_native_playback_and_drop_is_bounded() {
    let Some((player, path)) = native_test_player() else {
        eprintln!("libmpv or native fixture unavailable; skipping");
        return;
    };
    player
        .load_file(&path, 0.0, 0.35)
        .expect("valid fixture should enqueue");
    assert!(wait_until(&player, Duration::from_secs(5), |snapshot| {
        snapshot.loaded
    }));
    player.set_paused(false).expect("resume should enqueue");
    assert!(wait_until(&player, Duration::from_secs(3), |snapshot| {
        snapshot.paused && snapshot.position >= 0.30
    }));
    assert!(player.poll().error.is_none());

    let began = Instant::now();
    drop(player);
    assert!(
        began.elapsed() < Duration::from_secs(2),
        "worker did not join promptly"
    );
}

#[test]
fn range_end_latches_exact_position_and_replays_from_range_start() {
    let Some((player, path)) = native_test_player() else {
        eprintln!("libmpv or native fixture unavailable; skipping");
        return;
    };
    const START: f64 = 0.15;
    const END: f64 = 0.35;
    player
        .load_file(&path, START, END)
        .expect("valid fixture should enqueue");
    assert!(
        wait_until(&player, Duration::from_secs(5), |snapshot| {
            snapshot.loaded
                && snapshot.paused
                && !snapshot.seeking
                && (snapshot.position - START).abs() <= 0.03
        }),
        "fixture did not settle at its selected start: {:?}",
        player.poll()
    );

    player.set_paused(false).expect("resume should enqueue");
    assert!(
        wait_until(&player, Duration::from_secs(3), |snapshot| {
            snapshot.loaded
                && snapshot.paused
                && snapshot.eof
                && !snapshot.seeking
                && (snapshot.position - END).abs() <= 0.01
        }),
        "range end did not latch the exact terminal position: {:?}",
        player.poll()
    );
    let ended = player.poll();
    assert!(ended.paused && ended.eof);
    assert!((ended.position - END).abs() <= 0.01, "{ended:?}");

    // A second play command at the boundary must restart the selected range,
    // instead of briefly unpausing at END and immediately jumping back there.
    player.set_paused(false).expect("replay should enqueue");
    assert!(
        wait_until(&player, Duration::from_secs(3), |snapshot| {
            snapshot.loaded && !snapshot.paused && !snapshot.eof && snapshot.position < END - 0.05
        }),
        "replay did not leave the selected end from its start: {:?}",
        player.poll()
    );
    let replaying = player.poll();
    assert!(replaying.position >= START - 0.03, "{replaying:?}");
    player
        .set_paused(true)
        .expect("pause after replay should enqueue");
    assert!(
        wait_until(&player, Duration::from_secs(2), |snapshot| {
            snapshot.loaded && snapshot.paused && !snapshot.seeking
        }),
        "replay did not pause before the serial regression seek: {:?}",
        player.poll()
    );
    // The replay seek consumed a worker-internal serial. The next public seek
    // must still be accepted instead of being mistaken for that same serial.
    player
        .seek(0.25, true)
        .expect("post-replay seek should enqueue");
    assert!(
        wait_until(&player, Duration::from_secs(3), |snapshot| {
            snapshot.loaded
                && snapshot.paused
                && !snapshot.seeking
                && (snapshot.position - 0.25).abs() <= 0.03
        }),
        "post-replay seek was dropped as stale: {:?}",
        player.poll()
    );
}

#[test]
fn invalid_input_is_rejected_before_mpv_command() {
    let Some(library) = explicit_library() else {
        eprintln!("libmpv unavailable; skipping");
        return;
    };
    let player = NativePlayer::new_with_library(0, library).expect("libmpv should initialize");
    let error = player
        .load_file("/definitely/missing/slicer-input.mp4", 0.0, 1.0)
        .expect_err("missing input must be rejected synchronously");
    assert!(error.contains("unable to read media file"));
    assert!(!player.poll().loaded);
}

#[test]
fn rapid_scrubbing_settles_at_the_latest_release_target() {
    let Some((player, path)) = native_test_player() else {
        eprintln!("libmpv or native fixture unavailable; skipping");
        return;
    };
    player.load_file(&path, 0.0, 4.5).unwrap();
    assert!(wait_until(&player, Duration::from_secs(5), |s| s.loaded));
    for i in 0..40 {
        player.seek(0.2 + (i % 11) as f64 * 0.25, false).unwrap();
        thread::sleep(Duration::from_millis(12));
    }
    player.seek(3.2, true).unwrap();
    assert!(
        wait_until(&player, Duration::from_secs(4), |s| {
            !s.seeking && (s.position - 3.2).abs() < 0.08
        }),
        "latest release did not settle: {:?}",
        player.snapshot()
    );
    // Releasing again at the same target must preserve that presented frame.
    player.seek(3.2, true).unwrap();
    assert!(wait_until(&player, Duration::from_secs(1), |s| !s.seeking));
    assert!(player.snapshot().paused);
    assert!(player.snapshot().error.is_none());
}

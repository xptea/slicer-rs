//! Real FFmpeg/ffprobe integration checks.
//!
//! These tests intentionally require an explicit test-only override so that
//! they never make the application depend on a user's `PATH` or silently use
//! a system encoder. Run them with, for example:
//!
//! ```text
//! SLICER_TEST_FFMPEG_DIR=/usr/bin cargo test --test core_integration -- --ignored
//! ```

#![allow(dead_code)]

use slicer::job::{CropRect, ExportRequest, JobEvent, JobHandle, OutputFormat, TrimMode};
use slicer::media::{self, Binaries};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

fn test_binaries() -> Option<Binaries> {
    let directory = std::env::var_os("SLICER_TEST_FFMPEG_DIR")
        .or_else(|| std::env::var_os("SLICER_FFMPEG_DIR"))?;
    Binaries::from_dir(PathBuf::from(directory)).ok()
}

fn explicit_test_binaries() -> Option<Binaries> {
    let directory = std::env::var_os("SLICER_TEST_FFMPEG_DIR")?;
    Binaries::from_dir(PathBuf::from(directory)).ok()
}

#[cfg(unix)]
fn shell_quote(path: &Path) -> String {
    format!("'{}'", path.to_string_lossy().replace('\'', "'\\''"))
}

fn create_fixture(binaries: &Binaries, directory: &Path) -> PathBuf {
    create_fixture_with_audio_tracks(binaries, directory, 1, "source café 日本.mp4")
}

fn create_fixture_with_audio_tracks(
    binaries: &Binaries,
    directory: &Path,
    audio_tracks: usize,
    filename: &str,
) -> PathBuf {
    let input = directory.join(filename);
    let mut audio_paths = Vec::with_capacity(audio_tracks);
    for track in 0..audio_tracks {
        let path = directory.join(format!("audio-track-{track}.wav"));
        write_silence_wav(&path, 3);
        audio_paths.push(path);
    }

    let mut command = Command::new(&binaries.ffmpeg);
    command.args([
        "-hide_banner",
        "-loglevel",
        "error",
        "-f",
        "lavfi",
        "-i",
        "testsrc2=size=160x90:rate=24:duration=3",
    ]);
    for path in &audio_paths {
        command.arg("-i").arg(path);
    }
    command.args(["-shortest", "-map", "0:v:0"]);
    for track in 0..audio_tracks {
        command.arg("-map").arg(format!("{}:a:0", track + 1));
    }
    command.args([
        "-c:v", "mpeg4", "-q:v", "5", "-c:a", "aac", "-f", "mp4", "-y",
    ]);
    let status = command.arg(&input).status().expect("launch test ffmpeg");
    assert!(status.success(), "test fixture generation failed");
    input
}

fn write_silence_wav(path: &Path, seconds: u32) {
    let sample_rate = 16_000_u32;
    let channels = 1_u16;
    let bytes_per_sample = 2_u16;
    let data_len = sample_rate * seconds * u32::from(channels) * u32::from(bytes_per_sample);
    let mut bytes = Vec::with_capacity(44 + data_len as usize);
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&(36 + data_len).to_le_bytes());
    bytes.extend_from_slice(b"WAVEfmt ");
    bytes.extend_from_slice(&16_u32.to_le_bytes());
    bytes.extend_from_slice(&1_u16.to_le_bytes());
    bytes.extend_from_slice(&channels.to_le_bytes());
    bytes.extend_from_slice(&sample_rate.to_le_bytes());
    bytes.extend_from_slice(
        &(sample_rate * u32::from(channels) * u32::from(bytes_per_sample)).to_le_bytes(),
    );
    bytes.extend_from_slice(&(channels * bytes_per_sample).to_le_bytes());
    bytes.extend_from_slice(&16_u16.to_le_bytes());
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&data_len.to_le_bytes());
    bytes.resize(44 + data_len as usize, 0);
    std::fs::write(path, bytes).expect("write WAV fixture");
}

fn create_vfr_fixture(binaries: &Binaries, directory: &Path) -> PathBuf {
    let input = directory.join("variable frame rate 日本.mp4");
    let status = Command::new(&binaries.ffmpeg)
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=160x90:rate=24:duration=3",
            "-vf",
            "setpts=(N+floor(N/3))*1/(24*TB)",
            "-fps_mode",
            "vfr",
            "-c:v",
            "mpeg4",
            "-q:v",
            "5",
            "-an",
            "-f",
            "mp4",
            "-y",
        ])
        .arg(&input)
        .status()
        .expect("launch test ffmpeg");
    assert!(status.success(), "VFR fixture generation failed");
    input
}

fn create_tone_fixture(binaries: &Binaries, directory: &Path) -> PathBuf {
    let input = directory.join("tone source.mp4");
    let status = Command::new(&binaries.ffmpeg)
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=160x90:rate=24:duration=3",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:duration=3",
            "-shortest",
            "-map",
            "0:v:0",
            "-map",
            "1:a:0",
            "-c:v",
            "mpeg4",
            "-q:v",
            "5",
            "-c:a",
            "aac",
            "-f",
            "mp4",
            "-y",
        ])
        .arg(&input)
        .status()
        .expect("launch test ffmpeg");
    assert!(status.success(), "tone fixture generation failed");
    input
}

fn wait_for_terminal(handle: JobHandle) -> (JobEvent, bool) {
    let mut saw_progress = false;
    loop {
        match handle
            .events
            .recv_timeout(Duration::from_secs(30))
            .expect("FFmpeg job did not produce a terminal event")
        {
            event @ JobEvent::Completed(_)
            | event @ JobEvent::Cancelled
            | event @ JobEvent::Failed(_) => return (event, saw_progress),
            JobEvent::Progress(_) => saw_progress = true,
        }
    }
}

fn wav_data(path: &Path) -> Vec<u8> {
    let bytes = std::fs::read(path).expect("read WAV output");
    assert!(bytes.len() >= 12, "WAV output is too short");
    assert_eq!(&bytes[0..4], b"RIFF");
    assert_eq!(&bytes[8..12], b"WAVE");
    let mut offset = 12;
    while offset + 8 <= bytes.len() {
        let size = u32::from_le_bytes(bytes[offset + 4..offset + 8].try_into().unwrap()) as usize;
        let data_start = offset + 8;
        let data_end = data_start.saturating_add(size).min(bytes.len());
        if &bytes[offset..offset + 4] == b"data" {
            return bytes[data_start..data_end].to_vec();
        }
        offset = data_end + (size & 1);
    }
    panic!("WAV output has no data chunk");
}

#[test]
#[ignore = "requires explicit SLICER_TEST_FFMPEG_DIR"]
fn inspect_and_exact_trim_work_with_unicode_paths() {
    let binaries = test_binaries().expect("set SLICER_TEST_FFMPEG_DIR");
    let directory = tempfile::tempdir().expect("temporary fixture directory");
    let input = create_fixture(&binaries, directory.path());

    let info = media::inspect(&binaries, &input).expect("inspect fixture");
    assert!(info.duration > 2.5, "duration={}", info.duration);
    assert!(info.size > 0);
    let video = info
        .streams
        .iter()
        .find(|stream| stream.kind == "video")
        .expect("video stream");
    assert_eq!(video.width, Some(160));
    assert_eq!(video.height, Some(90));
    assert!(info.streams.iter().any(|stream| stream.kind == "audio"));

    let output = directory.path().join("trimmed result 日本.mp4");
    let handle = JobHandle::spawn(
        binaries.clone(),
        ExportRequest {
            input: input.clone(),
            output: output.clone(),
            start: 0.75,
            end: 1.75,
            mode: TrimMode::Exact,
            format: OutputFormat::Mp4,
            crop: None,
            quality: 80,
            mute_audio: false,
        },
    )
    .expect("start export");
    let (terminal, saw_progress) = wait_for_terminal(handle);
    assert!(
        saw_progress,
        "export should report progress before completion"
    );
    let completed = match terminal {
        JobEvent::Completed(path) => path,
        other => panic!("expected completed export, got {other:?}"),
    };
    assert_eq!(completed, output.canonicalize().expect("canonical output"));
    let trimmed = media::inspect(&binaries, &output).expect("inspect export");
    assert!(trimmed.duration > 0.6, "duration={}", trimmed.duration);
    assert!(trimmed.duration < 1.5, "duration={}", trimmed.duration);
}

#[test]
#[ignore = "requires explicit SLICER_TEST_FFMPEG_DIR"]
fn export_refuses_to_replace_an_existing_destination() {
    let binaries = test_binaries().expect("set SLICER_TEST_FFMPEG_DIR");
    let directory = tempfile::tempdir().expect("temporary fixture directory");
    let input = create_fixture(&binaries, directory.path());
    let output = directory.path().join("already exists.mp4");
    std::fs::write(&output, b"keep me").expect("seed destination");

    let result = JobHandle::spawn(
        binaries,
        ExportRequest {
            input,
            output: output.clone(),
            start: 0.0,
            end: 1.0,
            mode: TrimMode::Fast,
            format: OutputFormat::Mp4,
            crop: None,
            quality: 75,
            mute_audio: false,
        },
    );
    let error = match result {
        Ok(_) => panic!("existing destination must be rejected"),
        Err(error) => error,
    };
    assert!(error.to_string().contains("overwrite"));
    assert_eq!(
        std::fs::read(&output).expect("destination still exists"),
        b"keep me"
    );
}

#[test]
#[ignore = "requires explicit SLICER_TEST_FFMPEG_DIR"]
fn inspection_handles_missing_and_multiple_audio_tracks() {
    let binaries = test_binaries().expect("set SLICER_TEST_FFMPEG_DIR");
    let directory = tempfile::tempdir().expect("temporary fixture directory");

    let video_only =
        create_fixture_with_audio_tracks(&binaries, directory.path(), 0, "video only.mkv");
    let video_info = media::inspect(&binaries, &video_only).expect("inspect video-only fixture");
    assert!(
        video_info
            .streams
            .iter()
            .all(|stream| stream.kind != "audio")
    );

    let multi_audio =
        create_fixture_with_audio_tracks(&binaries, directory.path(), 2, "two audio tracks.mp4");
    let multi_info = media::inspect(&binaries, &multi_audio).expect("inspect multi-audio fixture");
    assert_eq!(
        multi_info
            .streams
            .iter()
            .filter(|stream| stream.kind == "audio")
            .count(),
        2
    );
}

#[test]
#[ignore = "requires explicit SLICER_TEST_FFMPEG_DIR"]
fn exact_trim_preserves_a_variable_frame_rate_timeline() {
    let binaries = test_binaries().expect("set SLICER_TEST_FFMPEG_DIR");
    let directory = tempfile::tempdir().expect("temporary fixture directory");
    let input = create_vfr_fixture(&binaries, directory.path());
    let output = directory.path().join("vfr trimmed.mkv");

    let handle = JobHandle::spawn(
        binaries.clone(),
        ExportRequest {
            input,
            output: output.clone(),
            start: 0.6,
            end: 1.8,
            mode: TrimMode::Exact,
            format: OutputFormat::Mkv,
            crop: None,
            quality: 75,
            mute_audio: false,
        },
    )
    .expect("start VFR export");
    let (terminal, saw_progress) = wait_for_terminal(handle);
    assert!(saw_progress);
    assert!(
        matches!(terminal, JobEvent::Completed(_)),
        "got {terminal:?}"
    );

    let output_info = media::inspect(&binaries, &output).expect("inspect VFR export");
    assert!(
        output_info.duration > 0.8,
        "duration={}",
        output_info.duration
    );
    assert!(
        output_info.duration < 1.6,
        "duration={}",
        output_info.duration
    );
}

#[test]
#[ignore = "requires explicit SLICER_TEST_FFMPEG_DIR"]
fn fast_crop_reencodes_video_and_preserves_audio() {
    let binaries = test_binaries().expect("set SLICER_TEST_FFMPEG_DIR");
    let directory = tempfile::tempdir().expect("temporary fixture directory");
    let input = create_fixture(&binaries, directory.path());
    let output = directory.path().join("cropped result.mp4");

    let handle = JobHandle::spawn(
        binaries.clone(),
        ExportRequest {
            input,
            output: output.clone(),
            start: 0.25,
            end: 1.75,
            // Crop requires decoding, even when the trim itself is requested
            // in Fast mode.
            mode: TrimMode::Fast,
            format: OutputFormat::Mp4,
            crop: Some(CropRect {
                x: 20,
                y: 10,
                width: 80,
                height: 40,
            }),
            quality: 75,
            mute_audio: false,
        },
    )
    .expect("start cropped export");
    let (terminal, saw_progress) = wait_for_terminal(handle);
    assert!(saw_progress, "crop export should report progress");
    assert!(
        matches!(terminal, JobEvent::Completed(_)),
        "got {terminal:?}"
    );

    let cropped = media::inspect(&binaries, &output).expect("inspect cropped export");
    let video = cropped
        .streams
        .iter()
        .find(|stream| stream.kind == "video")
        .expect("cropped export should contain video");
    assert_eq!(video.width, Some(80));
    assert_eq!(video.height, Some(40));
    assert!(
        cropped.streams.iter().any(|stream| stream.kind == "audio"),
        "cropped export should preserve audio"
    );
    assert!(cropped.duration > 0.9, "duration={}", cropped.duration);
    assert!(cropped.duration < 1.8, "duration={}", cropped.duration);
}

#[cfg(unix)]
#[test]
fn cancellation_removes_an_active_job_without_publishing_output() {
    use std::os::unix::fs::PermissionsExt;

    let directory = tempfile::tempdir().expect("temporary job directory");
    let input = directory.path().join("input.bin");
    std::fs::write(&input, b"input").expect("write input");
    let fake_ffmpeg = directory.path().join("fake-ffmpeg");
    std::fs::write(
        &fake_ffmpeg,
        "#!/bin/sh\nlast=\nfor arg\ndo last=\"$arg\"\ndone\nwhile :\ndo\n  printf 'out_time_ms=0\\nprogress=continue\\n'\n  sleep 0.05\ndone\n",
    )
    .expect("write fake FFmpeg");
    let mut permissions = std::fs::metadata(&fake_ffmpeg)
        .expect("fake FFmpeg metadata")
        .permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&fake_ffmpeg, permissions).expect("make fake FFmpeg executable");

    let handle = JobHandle::spawn(
        Binaries {
            ffmpeg: fake_ffmpeg,
            ffprobe: PathBuf::from("/bin/sh"),
        },
        ExportRequest {
            input,
            output: directory.path().join("cancelled.mp4"),
            start: 0.0,
            end: 30.0,
            mode: TrimMode::Exact,
            format: OutputFormat::Mp4,
            crop: None,
            quality: 75,
            mute_audio: false,
        },
    )
    .expect("start cancellable job");

    match handle
        .events
        .recv_timeout(Duration::from_secs(5))
        .expect("cancellable job did not report progress")
    {
        JobEvent::Progress(_) => {}
        JobEvent::Completed(path) => panic!("job completed before cancellation: {path:?}"),
        JobEvent::Cancelled | JobEvent::Failed(_) => {
            panic!("job ended before cancellation")
        }
    }
    handle.cancel();

    let terminal = loop {
        match handle
            .events
            .recv_timeout(Duration::from_secs(5))
            .expect("cancelled job did not terminate")
        {
            JobEvent::Progress(_) => {}
            event @ JobEvent::Completed(_)
            | event @ JobEvent::Cancelled
            | event @ JobEvent::Failed(_) => break event,
        }
    };
    assert!(matches!(terminal, JobEvent::Cancelled), "got {terminal:?}");
    assert!(!directory.path().join("cancelled.mp4").exists());
    let leftovers = std::fs::read_dir(directory.path())
        .expect("read cancellation directory")
        .filter_map(Result::ok)
        .filter(|entry| entry.file_name().to_string_lossy().contains(".slicer-"))
        .count();
    assert_eq!(leftovers, 0, "cancelled temp output remains");
}

#[cfg(unix)]
#[test]
fn crop_dimension_probe_does_not_block_job_spawn() {
    use std::os::unix::fs::PermissionsExt;
    use std::time::Instant;

    let directory = tempfile::tempdir().expect("temporary fixture directory");
    let input = directory.path().join("input.mp4");
    std::fs::write(&input, b"input").expect("write input");

    let ffprobe = directory.path().join("slow-ffprobe");
    std::fs::write(
        &ffprobe,
        "#!/bin/sh\nsleep 1\nprintf '%s\\n' '{\"streams\":[{\"codec_type\":\"video\",\"width\":160,\"height\":90}]}'\n",
    )
    .expect("write slow ffprobe");
    std::fs::set_permissions(&ffprobe, std::fs::Permissions::from_mode(0o700))
        .expect("make slow ffprobe executable");

    let began = Instant::now();
    let handle = JobHandle::spawn(
        Binaries {
            ffmpeg: PathBuf::from("/bin/false"),
            ffprobe,
        },
        ExportRequest {
            input,
            output: directory.path().join("cropped.mp4"),
            start: 0.0,
            end: 1.0,
            mode: TrimMode::Fast,
            format: OutputFormat::Mp4,
            crop: Some(CropRect {
                x: 0,
                y: 0,
                width: 80,
                height: 40,
            }),
            quality: 75,
            mute_audio: false,
        },
    )
    .expect("queue crop export");

    assert!(
        began.elapsed() < Duration::from_millis(700),
        "crop ffprobe ran synchronously during spawn"
    );

    let terminal = loop {
        match handle
            .events
            .recv_timeout(Duration::from_secs(4))
            .expect("slow crop probe did not produce a terminal event")
        {
            JobEvent::Progress(_) => {}
            event @ JobEvent::Completed(_)
            | event @ JobEvent::Cancelled
            | event @ JobEvent::Failed(_) => break event,
        }
    };
    assert!(matches!(terminal, JobEvent::Failed(_)), "got {terminal:?}");
    let leftovers = std::fs::read_dir(directory.path())
        .expect("read crop probe directory")
        .filter_map(Result::ok)
        .filter(|entry| entry.file_name().to_string_lossy().contains(".slicer-"))
        .count();
    assert_eq!(leftovers, 0, "failed crop probe left a temporary output");
}

#[cfg(unix)]
#[test]
#[ignore = "requires explicit SLICER_TEST_FFMPEG_DIR"]
fn real_bundled_ffmpeg_cancellation_terminates_and_cleans_up() {
    use std::os::unix::fs::PermissionsExt;

    let binaries = explicit_test_binaries().expect("set SLICER_TEST_FFMPEG_DIR");
    let directory = tempfile::tempdir().expect("temporary fixture directory");
    let input = create_fixture(&binaries, directory.path());

    // The wrapper adds an input loop while still executing the selected
    // bundled binary. This keeps the real FFmpeg process active long enough
    // to exercise JobHandle::cancel deterministically.
    let wrapper = directory.path().join("ffmpeg-loop-wrapper");
    std::fs::write(
        &wrapper,
        format!(
            "#!/bin/sh\nexec {} -stream_loop -1 \"$@\"\n",
            shell_quote(&binaries.ffmpeg)
        ),
    )
    .expect("write FFmpeg loop wrapper");
    let mut permissions = std::fs::metadata(&wrapper)
        .expect("wrapper metadata")
        .permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&wrapper, permissions).expect("make wrapper executable");

    let output = directory.path().join("real cancellation.mp4");
    let handle = JobHandle::spawn(
        Binaries {
            ffmpeg: wrapper,
            ffprobe: binaries.ffprobe,
        },
        ExportRequest {
            input,
            output: output.clone(),
            start: 0.0,
            end: 3_600.0,
            mode: TrimMode::Exact,
            format: OutputFormat::Mp4,
            crop: None,
            quality: 75,
            mute_audio: false,
        },
    )
    .expect("start real bundled FFmpeg job");

    match handle
        .events
        .recv_timeout(Duration::from_secs(10))
        .expect("bundled FFmpeg did not report progress")
    {
        JobEvent::Progress(progress) => assert!((0.0..=1.0).contains(&progress)),
        JobEvent::Completed(path) => panic!("looped job completed unexpectedly: {path:?}"),
        JobEvent::Cancelled | JobEvent::Failed(_) => {
            panic!("looped FFmpeg ended before cancellation")
        }
    }
    handle.cancel();

    let terminal = loop {
        match handle
            .events
            .recv_timeout(Duration::from_secs(10))
            .expect("cancelled bundled FFmpeg did not terminate")
        {
            JobEvent::Progress(_) => {}
            event @ JobEvent::Completed(_)
            | event @ JobEvent::Cancelled
            | event @ JobEvent::Failed(_) => break event,
        }
    };
    assert!(matches!(terminal, JobEvent::Cancelled), "got {terminal:?}");
    assert!(!output.exists(), "cancelled export was published");
    let leftovers = std::fs::read_dir(directory.path())
        .expect("read cancellation directory")
        .filter_map(Result::ok)
        .filter(|entry| entry.file_name().to_string_lossy().contains(".slicer-"))
        .count();
    assert_eq!(leftovers, 0, "cancelled temp output remains");
}

#[test]
fn invalid_input_is_rejected_before_starting_a_process() {
    let executable = std::env::current_exe().expect("current test executable");
    let result = JobHandle::spawn(
        Binaries {
            ffmpeg: executable.clone(),
            ffprobe: executable,
        },
        ExportRequest {
            input: PathBuf::from("this input does not exist.mp4"),
            output: std::env::temp_dir().join("slicer-invalid-input-output.mp4"),
            start: 0.0,
            end: 1.0,
            mode: TrimMode::Fast,
            format: OutputFormat::Mp4,
            crop: None,
            quality: 75,
            mute_audio: false,
        },
    );
    let error = match result {
        Ok(_) => panic!("missing input must be rejected"),
        Err(error) => error,
    };
    assert!(error.to_string().contains("input"));
}

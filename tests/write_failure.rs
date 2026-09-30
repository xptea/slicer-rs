//! Inject a deterministic storage failure without filling the user's disk.
#[cfg(unix)]
#[test]
fn failed_output_write_never_publishes_or_leaves_partial_file() {
    use slicer::{
        job::{ExportRequest, JobEvent, JobHandle, OutputFormat, TrimMode},
        media::Binaries,
    };
    use std::{fs, os::unix::fs::PermissionsExt, time::Duration};
    let dir = tempfile::tempdir().unwrap();
    let fake = dir.path().join("ffmpeg");
    // Production uses direct process arguments; this executable is only a
    // failure-injection fixture. The last argument is the reserved temp file.
    fs::write(&fake, "#!/bin/sh\nfor output do :; done\nprintf partial > \"$output\"\nprintf 'No space left on device\\n' >&2\nexit 1\n").unwrap();
    fs::set_permissions(&fake, fs::Permissions::from_mode(0o700)).unwrap();
    let probe = dir.path().join("ffprobe");
    fs::write(&probe, "#!/bin/sh\nprintf '%s' '{\"streams\":[{\"codec_type\":\"video\",\"bit_rate\":\"200000\"}]}'\n").unwrap();
    fs::set_permissions(&probe, fs::Permissions::from_mode(0o700)).unwrap();
    let input = dir.path().join("original.mp4");
    fs::write(&input, b"original data").unwrap();
    let output = dir.path().join("result.mp4");
    let job = JobHandle::spawn(
        Binaries {
            ffmpeg: fake.clone(),
            ffprobe: probe,
        },
        ExportRequest {
            input: input.clone(),
            output: output.clone(),
            start: 0.0,
            end: 1.0,
            mode: TrimMode::Exact,
            format: OutputFormat::Mp4,
            crop: None,
            quality: 75,
            mute_audio: false,
        },
    )
    .unwrap();
    loop {
        match job.events.recv_timeout(Duration::from_secs(5)).unwrap() {
            JobEvent::Progress(_) => {}
            JobEvent::Failed(error) => {
                assert!(error.contains("No space left on device"), "{error}");
                break;
            }
            other => panic!("expected storage failure, got {other:?}"),
        }
    }
    assert!(!output.exists());
    assert_eq!(fs::read(input).unwrap(), b"original data");
    assert_eq!(
        fs::read_dir(dir.path()).unwrap().count(),
        3,
        "partial output leaked"
    );
}

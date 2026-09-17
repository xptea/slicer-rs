use slicer::composition::InMemoryMedia;
use slicer::composition::frame::{Rgba8, RgbaFrame};
use slicer::export::{
    CompositionExportRequest, CompositionFormat, ExportControl, ExportError, export_project,
};
use slicer::export::{FrameSchedule, render_range};
use slicer::media::Binaries;
use slicer::project::{
    Asset, AssetId, Canvas, Clip, ClipId, FrameRate, Project, Time, TimeRange, Track, TrackId,
};
use std::io::Write;
use std::process::Command;

fn range(start: i64, end: i64) -> TimeRange {
    TimeRange::new(Time::from_integer(start), Time::from_integer(end)).unwrap()
}

#[test]
fn frame_schedule_is_exact_and_end_exclusive() {
    let schedule = FrameSchedule::new(
        TimeRange::new(Time::ZERO, Time::new(1, 2).unwrap()).unwrap(),
        FrameRate::FPS_30,
    )
    .unwrap();
    assert_eq!(schedule.len(), 15);
    assert_eq!(schedule.frame_at(0).unwrap().unwrap().time, Time::ZERO);
    assert_eq!(
        schedule.frame_at(14).unwrap().unwrap().time,
        Time::new(7, 15).unwrap()
    );
    assert!(schedule.frame_at(15).unwrap().is_none());
}

#[test]
fn render_range_streams_frames_in_timestamp_order() {
    let mut project = Project::new(Canvas::new(1, 1).unwrap(), FrameRate::new(2, 1).unwrap());
    project.duration_policy = slicer::project::DurationPolicy::explicit(Time::from_integer(1));
    let media = InMemoryMedia::new();
    let mut observed = Vec::new();
    let report = render_range(
        &project,
        range(0, 1),
        &media,
        Default::default(),
        |scheduled, frame: &RgbaFrame| {
            observed.push((scheduled.index, scheduled.time, frame.pixel(0, 0)));
            Ok(())
        },
    )
    .unwrap();
    assert_eq!(report.frames, 2);
    assert_eq!(observed.len(), 2);
    assert_eq!(observed[0].0, 0);
    assert_eq!(observed[1].0, 1);
    assert_eq!(observed[0].1, Time::ZERO);
    assert_eq!(observed[1].1, Time::new(1, 2).unwrap());
    assert_eq!(observed[0].2, Some(Rgba8::new(0, 0, 0, 255)));
}

#[test]
#[ignore = "requires the explicitly configured bundled FFmpeg runtime"]
fn bundled_ffmpeg_can_render_a_project_snapshot() {
    let directory = tempfile::tempdir().unwrap();
    let binaries = Binaries::from_dir(std::env::var("SLICER_FFMPEG_DIR").unwrap()).unwrap();
    let input = directory.path().join("source.mp4");
    let status = Command::new(&binaries.ffmpeg)
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-f",
            "lavfi",
            "-i",
            "color=c=red:s=2x2:r=2:d=1",
            "-c:v",
            "mpeg4",
            "-y",
        ])
        .arg(&input)
        .status()
        .unwrap();
    assert!(status.success());

    let asset_id = AssetId::new(501);
    let track_id = TrackId::new(502);
    let clip_id = ClipId::new(503);
    let mut project = Project::new(Canvas::new(2, 2).unwrap(), FrameRate::new(2, 1).unwrap());
    project
        .add_asset(Asset::video(asset_id, &input, Time::from_integer(1), 2, 2).unwrap())
        .unwrap();
    project.add_track(Track::new(track_id, "Video", 0)).unwrap();
    project
        .add_clip(
            track_id,
            Clip::video(clip_id, range(0, 1), asset_id, range(0, 1)),
        )
        .unwrap();
    let output = directory.path().join("rendered.mkv");
    let request = CompositionExportRequest {
        project,
        binaries,
        output: output.clone(),
        range: None,
        format: CompositionFormat::Mkv,
        quality: 75,
        render_options: Default::default(),
    };
    let report = export_project(&request, &ExportControl::default(), |_, _| {}).unwrap();
    assert_eq!(report.frames, 2);
    assert!(output.is_file());
}

#[test]
#[ignore = "requires the explicitly configured bundled FFmpeg runtime"]
fn composition_export_rejects_a_source_replaced_mid_render() {
    let directory = tempfile::tempdir().unwrap();
    let binaries = Binaries::from_dir(std::env::var("SLICER_FFMPEG_DIR").unwrap()).unwrap();
    let input = directory.path().join("source.mp4");
    let status = Command::new(&binaries.ffmpeg)
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-f",
            "lavfi",
            "-i",
            "color=c=red:s=2x2:r=2:d=1",
            "-c:v",
            "mpeg4",
            "-y",
        ])
        .arg(&input)
        .status()
        .unwrap();
    assert!(status.success());

    let asset_id = AssetId::new(701);
    let track_id = TrackId::new(702);
    let clip_id = ClipId::new(703);
    let mut project = Project::new(Canvas::new(2, 2).unwrap(), FrameRate::new(2, 1).unwrap());
    project
        .add_asset(Asset::video(asset_id, &input, Time::from_integer(1), 2, 2).unwrap())
        .unwrap();
    project.add_track(Track::new(track_id, "Video", 0)).unwrap();
    project
        .add_clip(
            track_id,
            Clip::video(clip_id, range(0, 1), asset_id, range(0, 1)),
        )
        .unwrap();
    let output = directory.path().join("should-not-publish.mkv");
    let request = CompositionExportRequest {
        project,
        binaries,
        output: output.clone(),
        range: None,
        format: CompositionFormat::Mkv,
        quality: 75,
        render_options: Default::default(),
    };
    let mut changed = false;
    let result = export_project(&request, &ExportControl::default(), |completed, _| {
        if completed == 1 && !changed {
            changed = true;
            std::fs::OpenOptions::new()
                .append(true)
                .open(&input)
                .unwrap()
                .write_all(b"source changed")
                .unwrap();
        }
    });
    assert!(matches!(
        result,
        Err(ExportError::SourceChanged(path)) if path == input
    ));
    assert!(!output.exists());
}

#[test]
#[ignore = "requires the explicitly configured bundled FFmpeg runtime"]
fn bundled_ffmpeg_can_mix_a_project_audio_clip_to_wav() {
    let directory = tempfile::tempdir().unwrap();
    let binaries = Binaries::from_dir(std::env::var("SLICER_FFMPEG_DIR").unwrap()).unwrap();
    let input = directory.path().join("tone.wav");
    let status = Command::new(&binaries.ffmpeg)
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:duration=1",
            "-y",
        ])
        .arg(&input)
        .status()
        .unwrap();
    assert!(status.success());

    let asset_id = AssetId::new(601);
    let track_id = TrackId::new(602);
    let clip_id = ClipId::new(603);
    let mut project = Project::new(Canvas::new(1, 1).unwrap(), FrameRate::FPS_30);
    project
        .add_asset(Asset::audio(asset_id, &input, Time::from_integer(1), 44_100, 1).unwrap())
        .unwrap();
    project.add_track(Track::new(track_id, "Audio", 0)).unwrap();
    project
        .add_clip(
            track_id,
            Clip::audio(clip_id, range(0, 1), asset_id, range(0, 1)),
        )
        .unwrap();
    let output = directory.path().join("mix.wav");
    let request = CompositionExportRequest {
        project,
        binaries,
        output: output.clone(),
        range: None,
        format: CompositionFormat::Wav,
        quality: 75,
        render_options: Default::default(),
    };
    let report = export_project(&request, &ExportControl::default(), |_, _| {}).unwrap();
    assert_eq!(report.frames, 1);
    assert!(output.is_file());
}

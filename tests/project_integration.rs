use slicer::project::{
    Asset, AssetId, AssetMetadata, Clip, ClipId, Color, DurationPolicy, EditCommand, FrameRate,
    Project, ProjectHistory, ProjectStorageError, Rational, SceneKind, SingleVideoAdapter, Time,
    TimeRange, Track, TrackId, VideoMetadata, load_project, save_atomic,
};
use std::fs;

fn range(start: i64, end: i64) -> TimeRange {
    TimeRange::new(Time::from_integer(start), Time::from_integer(end)).unwrap()
}

fn video_project(path: &std::path::Path) -> (Project, AssetId, TrackId, ClipId) {
    let mut project = Project::new_empty();
    let asset_id = AssetId::new(10);
    let track_id = TrackId::new(20);
    let clip_id = ClipId::new(30);
    project
        .add_asset(Asset::video(asset_id, path, Time::from_integer(8), 320, 180).unwrap())
        .unwrap();
    project.add_track(Track::new(track_id, "Video", 0)).unwrap();
    project
        .add_clip(
            track_id,
            Clip::video(clip_id, range(0, 4), asset_id, range(1, 5)),
        )
        .unwrap();
    (project, asset_id, track_id, clip_id)
}

#[test]
fn rational_and_frame_rate_operations_remain_exact() {
    let one_thousandth = Rational::new(1, 1_000).unwrap();
    let three = one_thousandth
        .checked_mul_integer(3)
        .expect("small multiplication");
    assert_eq!(three, Rational::new(3, 1_000).unwrap());
    assert_eq!(
        FrameRate::NTSC_24.frame_duration().unwrap(),
        Rational::new(1_001, 24_000).unwrap()
    );
    assert_eq!(
        FrameRate::FPS_30
            .frame_index_at(Rational::new(1, 30).unwrap())
            .unwrap(),
        1
    );
    assert!(Rational::from_seconds(f64::NAN).is_err());
}

#[test]
fn vfr_frame_selection_uses_half_open_presentation_intervals() {
    let mut metadata = VideoMetadata::new(320, 180, Time::from_integer(3)).unwrap();
    metadata.time_base = Rational::new(1, 1_000).unwrap();
    metadata.frames = vec![
        slicer::project::SourceFrame::new(0, 0, Some(400)),
        slicer::project::SourceFrame::new(1, 400, Some(900)),
        slicer::project::SourceFrame::new(2, 1_300, None),
    ];
    assert_eq!(
        metadata
            .frame_at(Rational::new(2, 5).unwrap())
            .unwrap()
            .unwrap()
            .index,
        1
    );
    assert_eq!(
        metadata
            .frame_at(Rational::new(13, 10).unwrap())
            .unwrap()
            .unwrap()
            .index,
        2
    );
    assert!(metadata.frame_at(Time::from_integer(3)).unwrap().is_none());
}

#[test]
fn scene_maps_current_time_and_orders_overlaps_deterministically() {
    let path = std::path::PathBuf::from("media.mp4");
    let (mut project, asset_id, track_id, first_id) = video_project(&path);
    let second_id = ClipId::new(31);
    let mut second = Clip::video(second_id, range(0, 4), asset_id, range(0, 4));
    second.order = 10;
    project.add_clip(track_id, second).unwrap();

    let scene = project.evaluate_scene(Time::from_integer(1)).unwrap();
    assert_eq!(scene.draw_items.len(), 2);
    assert_eq!(scene.draw_items[0].clip_id, first_id);
    assert_eq!(scene.draw_items[1].clip_id, second_id);
    match &scene.draw_items[0].kind {
        SceneKind::Video { source_time, .. } => assert_eq!(*source_time, Time::from_integer(2)),
        other => panic!("expected video, got {other:?}"),
    }
}

#[test]
fn commands_are_atomic_and_undo_redo_are_bounded() {
    let (project, _, track_id, clip_id) = video_project(std::path::Path::new("input.mp4"));
    let mut history = ProjectHistory::with_capacity(project, 2);
    history
        .execute(EditCommand::SetBackground {
            background: Color::new(0.1, 0.2, 0.3, 1.0).unwrap(),
        })
        .unwrap();
    history
        .execute(EditCommand::MoveClip {
            clip_id,
            new_start: Time::from_integer(2),
        })
        .unwrap();
    history
        .execute(EditCommand::SetTrackOrder { track_id, order: 4 })
        .unwrap();
    assert_eq!(history.undo_len(), 2);
    assert!(history.undo().unwrap());
    assert!(history.redo().unwrap());
    assert_eq!(history.project().track(track_id).unwrap().order, 4);
    let before = history.project().clone();
    assert!(
        history
            .execute(EditCommand::SetClipRange {
                clip_id,
                range: range(0, 3),
            })
            .is_err()
    );
    assert_eq!(history.project(), &before);
}

#[test]
fn project_storage_round_trips_relative_assets_and_reports_missing_media() {
    let directory = tempfile::tempdir().unwrap();
    let media = directory.path().join("clip café.mp4");
    fs::write(&media, b"fixture").unwrap();
    let project_path = directory.path().join("edit.slicer.json");
    let (project, _, _, _) = video_project(&media);
    save_atomic(&project, &project_path).unwrap();
    let stored: serde_json::Value =
        serde_json::from_slice(&fs::read(&project_path).unwrap()).unwrap();
    let stored_path = stored["assets"]["10"]["path"].as_str().unwrap();
    assert_eq!(stored_path, "clip café.mp4");
    let loaded = load_project(&project_path).unwrap();
    assert!(loaded.is_complete());
    assert_eq!(loaded.project.assets.values().next().unwrap().path, media);

    let missing_path = directory.path().join("missing.json");
    let missing_media = directory.path().join("gone.mp4");
    let (missing_project, _, _, _) = video_project(&missing_media);
    save_atomic(&missing_project, &missing_path).unwrap();
    let loaded = load_project(&missing_path).unwrap();
    assert_eq!(loaded.missing_assets.len(), 1);
    assert!(matches!(
        slicer::project::load_project(directory.path()),
        Err(ProjectStorageError::InvalidPath(_)) | Err(ProjectStorageError::Io(_))
    ));
}

#[test]
fn legacy_adapter_preserves_trim_and_crop() {
    let mut project =
        SingleVideoAdapter::from_media("movie.mp4", Time::from_integer(10), 640, 360).unwrap();
    let mut edit = SingleVideoAdapter::to_legacy(&project).unwrap();
    edit.start = Time::from_integer(2);
    edit.end = Time::from_integer(5);
    edit.crop = Some(slicer::project::CropRect::new(0, 0, 640, 360).unwrap());
    SingleVideoAdapter::apply_legacy(&mut project, &edit).unwrap();
    let round_trip = SingleVideoAdapter::to_legacy(&project).unwrap();
    assert_eq!(round_trip.start, Time::from_integer(2));
    assert_eq!(round_trip.end, Time::from_integer(5));
    assert_eq!(round_trip.crop, edit.crop);
    assert!(matches!(
        project.duration_policy,
        DurationPolicy::LongestClip
    ));
    assert!(matches!(
        project.assets.values().next().unwrap().metadata,
        AssetMetadata::Video(_)
    ));
}

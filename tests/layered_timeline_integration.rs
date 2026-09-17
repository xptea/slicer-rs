#![allow(dead_code)]

#[path = "../src/ui/layered_timeline.rs"]
mod layered_timeline;

use layered_timeline::{
    ClipEdge, LayerTarget, SnapOptions, SnapTarget, TimelineScale, TimelineViewport,
    TrackRowLayout, VisibilityTarget, build_grouped_drag_transaction, build_move_command,
    build_set_clip_order_command, build_set_clip_visibility_command, build_set_layer_order_command,
    build_set_track_locked_command, build_set_track_order_command,
    build_set_track_visibility_command, build_set_visibility_command, build_split_command,
    build_trim_command, hit_test, snap_time, visible_clips,
};
use slicer::project::{
    Asset, AssetId, Clip, ClipId, EditCommand, Project, ProjectHistory, Rational, Time, TimeRange,
    Track, TrackId,
};

fn time(numerator: i64, denominator: u32) -> Time {
    Rational::new(numerator, denominator).unwrap()
}

fn range(start: i64, end: i64) -> TimeRange {
    TimeRange::new(Time::from_integer(start), Time::from_integer(end)).unwrap()
}

fn text_project() -> Project {
    let mut project = Project::new_empty();
    project
        .add_track(Track::new(TrackId::new(1), "background", 0))
        .unwrap();
    project
        .add_track(Track::new(TrackId::new(2), "foreground", 10))
        .unwrap();
    project
        .add_track(Track::new(TrackId::new(3), "middle", 5))
        .unwrap();
    project
        .add_clip(
            TrackId::new(2),
            Clip::text(ClipId::new(20), range(2, 4), "top"),
        )
        .unwrap();
    project
        .add_clip(TrackId::new(2), {
            let mut clip = Clip::text(ClipId::new(21), range(1, 3), "under");
            clip.order = -1;
            clip
        })
        .unwrap();
    project
        .add_clip(
            TrackId::new(3),
            Clip::text(ClipId::new(30), range(6, 8), "middle"),
        )
        .unwrap();
    project
}

fn video_project() -> (Project, TrackId, ClipId, AssetId) {
    let mut project = Project::new_empty();
    let asset_id = AssetId::new(100);
    let track_id = TrackId::new(10);
    let clip_id = ClipId::new(11);
    project
        .add_asset(
            Asset::video(asset_id, "source.mp4", Time::from_integer(10), 1_920, 1_080).unwrap(),
        )
        .unwrap();
    project.add_track(Track::new(track_id, "video", 0)).unwrap();
    project
        .add_clip(
            track_id,
            Clip::video(clip_id, range(2, 5), asset_id, range(1, 4)),
        )
        .unwrap();
    (project, track_id, clip_id, asset_id)
}

#[test]
fn rational_coordinates_round_trip_and_zoom_keeps_anchor_time() {
    let scale = TimelineScale::new(100.0, Rational::new(120, 1).unwrap(), time(1, 2)).unwrap();
    assert_eq!(scale.time_to_pixel(time(5, 2)).unwrap(), 340.0);
    assert_eq!(scale.pixel_to_time(340.0).unwrap(), time(5, 2));

    let anchor_time = scale.pixel_to_time(250.0).unwrap();
    let zoomed = scale.zoom_about(2.0, 250.0).unwrap();
    assert_eq!(zoomed.pixel_to_time(250.0).unwrap(), anchor_time);
    assert_eq!(zoomed.pixels_per_second, Rational::new(240, 1).unwrap());

    let scrolled = scale.scroll_by_pixels(60.0).unwrap();
    assert_eq!(scrolled.scroll_time, Time::from_integer(1));
}

#[test]
fn rows_are_layer_ordered_virtualized_and_have_end_exclusive_bounds() {
    let project = text_project();
    let rows = TrackRowLayout::new(10.0, 0.0, 20.0, 5.0).unwrap();
    let all = rows.rows(&project);
    assert_eq!(
        all.iter()
            .map(|row| row.track_id.value())
            .collect::<Vec<_>>(),
        [2, 3, 1]
    );
    assert_eq!(all[0].top, 10.0);
    assert_eq!(all[0].bottom, 30.0);
    assert_eq!(
        rows.row_at(&project, 10.0).unwrap().track_id,
        TrackId::new(2)
    );
    assert!(rows.row_at(&project, 30.0).is_none());
    assert_eq!(
        rows.row_at(&project, 35.0).unwrap().track_id,
        TrackId::new(3)
    );

    let visible = rows.visible_rows(&project, 25.0);
    assert_eq!(visible.len(), 1);
    assert_eq!(visible[0].track_id, TrackId::new(2));
    assert_eq!(rows.total_height(&project), 70.0);
}

#[test]
fn visible_clip_hit_testing_respects_hidden_state_overlap_and_clip_end() {
    let project = text_project();
    let viewport = TimelineViewport::new(
        0.0,
        0.0,
        900.0,
        70.0,
        Rational::new(100, 1).unwrap(),
        Time::ZERO,
        0.0,
    )
    .unwrap();
    let rows = viewport.row_layout(20.0, 5.0).unwrap();

    let hit = hit_test(&project, viewport, &rows, 250.0, 15.0)
        .unwrap()
        .unwrap();
    assert_eq!(hit.clip_id, ClipId::new(20));
    assert_eq!(hit.time, time(5, 2));

    let exact_end = hit_test(&project, viewport, &rows, 400.0, 15.0).unwrap();
    assert!(exact_end.is_none());
    assert!(
        hit_test(&project, viewport, &rows, 250.0, 35.0)
            .unwrap()
            .is_none()
    );

    let clips = visible_clips(&project, viewport, &rows).unwrap();
    assert_eq!(
        clips.iter().map(|clip| clip.clip_id).collect::<Vec<_>>(),
        [ClipId::new(21), ClipId::new(20), ClipId::new(30)]
    );

    let mut hidden = project.clone();
    hidden.track_mut(TrackId::new(2)).unwrap().visible = false;
    assert!(
        hit_test(&hidden, viewport, &rows, 250.0, 15.0)
            .unwrap()
            .is_none()
    );
}

#[test]
fn snapping_prefers_clip_boundaries_then_uses_the_exact_frame_grid() {
    let project = text_project();
    let scale = TimelineScale::new(0.0, Rational::new(100, 1).unwrap(), Time::ZERO).unwrap();

    let boundary = snap_time(&project, time(299, 100), scale, &SnapOptions::default()).unwrap();
    assert_eq!(boundary.time, Time::from_integer(3));
    assert_eq!(
        boundary.target,
        Some(SnapTarget::ClipBoundary {
            track_id: TrackId::new(2),
            clip_id: ClipId::new(21),
            edge: ClipEdge::End,
        })
    );

    let frame = snap_time(
        &project,
        time(157, 150),
        scale,
        &SnapOptions::default().with_threshold_pixels(3.0),
    )
    .unwrap();
    assert_eq!(frame.time, time(31, 30));
    assert_eq!(frame.target, Some(SnapTarget::Frame { frame_index: 31 }));

    let unsnapped = snap_time(&project, time(299, 100), scale, &SnapOptions::disabled()).unwrap();
    assert_eq!(unsnapped.time, time(299, 100));
    assert!(!unsnapped.is_snapped());
}

#[test]
fn move_and_trim_builders_keep_source_mapping_exact_and_bound_trim() {
    let (project, _track_id, clip_id, _asset_id) = video_project();

    let move_command = build_move_command(&project, clip_id, Time::from_integer(4)).unwrap();
    assert!(matches!(move_command, EditCommand::MoveClip { .. }));
    let mut moved = project.clone();
    move_command.apply(&mut moved).unwrap();
    let moved_clip = moved.clip(clip_id).unwrap();
    assert_eq!(moved_clip.range, range(4, 7));
    assert_eq!(moved_clip.source_range_ref().unwrap(), &range(1, 4));

    let trim_start =
        build_trim_command(&project, clip_id, ClipEdge::Start, Time::from_integer(3)).unwrap();
    let mut start_trimmed = project.clone();
    trim_start.apply(&mut start_trimmed).unwrap();
    let start_clip = start_trimmed.clip(clip_id).unwrap();
    assert_eq!(start_clip.range, range(3, 5));
    assert_eq!(start_clip.source_range_ref().unwrap(), &range(2, 4));

    let trim_end =
        build_trim_command(&project, clip_id, ClipEdge::End, Time::from_integer(4)).unwrap();
    let mut end_trimmed = project.clone();
    trim_end.apply(&mut end_trimmed).unwrap();
    let end_clip = end_trimmed.clip(clip_id).unwrap();
    assert_eq!(end_clip.range, range(2, 4));
    assert_eq!(end_clip.source_range_ref().unwrap(), &range(1, 3));

    assert!(build_trim_command(&project, clip_id, ClipEdge::End, Time::from_integer(12),).is_err());
    assert!(build_trim_command(&project, clip_id, ClipEdge::End, Time::from_integer(2),).is_err());
}

#[test]
fn split_builder_preserves_half_open_source_halves() {
    let (project, _track_id, clip_id, _asset_id) = video_project();
    let command =
        build_split_command(&project, clip_id, Time::from_integer(3), ClipId::new(12)).unwrap();
    let mut split = project.clone();
    command.apply(&mut split).unwrap();

    let left = split.clip(clip_id).unwrap();
    let right = split.clip(ClipId::new(12)).unwrap();
    assert_eq!(left.range, range(2, 3));
    assert_eq!(right.range, range(3, 5));
    assert_eq!(left.source_range_ref().unwrap(), &range(1, 2));
    assert_eq!(right.source_range_ref().unwrap(), &range(2, 4));
    assert!(
        build_split_command(&project, clip_id, Time::from_integer(2), ClipId::new(13)).is_err()
    );
    assert!(
        build_split_command(&project, clip_id, Time::from_integer(5), ClipId::new(13)).is_err()
    );
}

#[test]
fn layer_visibility_and_lock_builders_use_existing_commands() {
    let (mut project, track_id, clip_id, _) = video_project();

    build_set_layer_order_command(&project, LayerTarget::Clip(clip_id), 7)
        .unwrap()
        .apply(&mut project)
        .unwrap();
    assert_eq!(project.clip(clip_id).unwrap().order, 7);

    build_set_track_order_command(&project, track_id, 4)
        .unwrap()
        .apply(&mut project)
        .unwrap();
    assert_eq!(project.track(track_id).unwrap().order, 4);

    build_set_clip_visibility_command(&project, clip_id, false)
        .unwrap()
        .apply(&mut project)
        .unwrap();
    assert!(!project.clip(clip_id).unwrap().visible);
    build_set_track_visibility_command(&project, track_id, false)
        .unwrap()
        .apply(&mut project)
        .unwrap();
    assert!(!project.track(track_id).unwrap().visible);

    build_set_track_locked_command(&project, track_id, true)
        .unwrap()
        .apply(&mut project)
        .unwrap();
    assert!(build_move_command(&project, clip_id, Time::from_integer(1)).is_err());
    assert!(build_set_clip_order_command(&project, clip_id, 1).is_err());
    build_set_layer_order_command(&project, LayerTarget::Track(track_id), 8)
        .unwrap()
        .apply(&mut project)
        .unwrap();
    assert_eq!(project.track(track_id).unwrap().order, 8);

    build_set_track_locked_command(&project, track_id, false)
        .unwrap()
        .apply(&mut project)
        .unwrap();
    build_set_clip_visibility_command(&project, clip_id, true)
        .unwrap()
        .apply(&mut project)
        .unwrap();
    assert!(project.clip(clip_id).unwrap().visible);

    build_set_layer_order_command(&project, LayerTarget::Clip(clip_id), 9)
        .unwrap()
        .apply(&mut project)
        .unwrap();
    assert_eq!(project.clip(clip_id).unwrap().order, 9);

    build_set_visibility_command(&project, VisibilityTarget::Clip(clip_id), true)
        .unwrap()
        .apply(&mut project)
        .unwrap();
}

#[test]
fn grouped_drag_is_one_batch_and_one_undo_step() {
    let (mut project, track_id, first_id, asset_id) = video_project();
    let second_id = ClipId::new(12);
    project
        .add_clip(
            track_id,
            Clip::video(second_id, range(6, 8), asset_id, range(0, 2)),
        )
        .unwrap();

    let before = project.clone();
    let transaction =
        build_grouped_drag_transaction(&project, &[second_id, first_id], time(1, 2)).unwrap();
    assert_eq!(transaction.commands().len(), 2);
    assert!(matches!(transaction.command(), EditCommand::Batch { .. }));

    let mut history = ProjectHistory::new(project.clone());
    history.execute(transaction.command()).unwrap();
    assert_eq!(history.undo_len(), 1);
    assert_eq!(
        history.project().clip(first_id).unwrap().range,
        TimeRange::new(time(5, 2), time(11, 2)).unwrap()
    );
    assert_eq!(
        history.project().clip(second_id).unwrap().range,
        TimeRange::new(time(13, 2), time(17, 2)).unwrap()
    );
    assert_eq!(
        history.project().clip(first_id).unwrap().source_range_ref(),
        Some(&range(1, 4))
    );
    assert_eq!(
        history
            .project()
            .clip(second_id)
            .unwrap()
            .source_range_ref(),
        Some(&range(0, 2))
    );

    assert!(history.undo().unwrap());
    assert_eq!(
        history.project().clip(first_id).unwrap().range,
        before.clip(first_id).unwrap().range
    );
    assert_eq!(
        history
            .project()
            .clip(second_id)
            .unwrap()
            .source_range_ref(),
        before.clip(second_id).unwrap().source_range_ref()
    );
    assert!(build_grouped_drag_transaction(&project, &[first_id, first_id], Time::ZERO).is_err());
    assert!(build_grouped_drag_transaction(&project, &[], Time::ZERO).is_err());

    project.track_mut(track_id).unwrap().locked = true;
    assert!(build_grouped_drag_transaction(&project, &[first_id], Time::ZERO).is_err());
}

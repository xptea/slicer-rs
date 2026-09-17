#[allow(dead_code)]
#[path = "../src/ui/canvas_tools.rs"]
mod canvas_tools;
#[allow(dead_code)]
#[path = "../src/ui/layer_inspector.rs"]
mod layer_inspector;

use canvas_tools::{
    CanvasDisplayTransform, CropOptions, HandleKind, ResizeOptions, SelectionGeometry, Size,
    SourceGeometry, VisualGeometry, crop_transform_from_canvas_drag, drag_transform,
    resize_transform, rotate_transform, snap_to_guides,
};
use layer_inspector::{
    InspectorError, TextProperties, crop_command, layer_rows, lock_command, set_text_command,
    text_properties_command, transform_command, visibility_command, z_order_command,
};
use slicer::project::{
    Asset, Canvas, Clip, ClipId, ClipKind, CropRect, EditCommand, FrameRate, Point, Project,
    ProjectHistory, TextAlignment, TextStyle, Time, TimeRange, Track, TrackId, Transform,
};

fn close(left: f64, right: f64) {
    assert!((left - right).abs() < 1e-9, "{left} != {right}");
}

fn range() -> TimeRange {
    TimeRange::new(Time::ZERO, Time::from_integer(1)).unwrap()
}

#[test]
fn display_mapping_round_trips_through_scale_and_letterbox() {
    let display = canvas_tools::Rect::new(10.0, 20.0, 1920.0, 1200.0).unwrap();
    let mapping =
        CanvasDisplayTransform::fit(display, Canvas::new(1920, 1080).unwrap(), 1.5, 1.0).unwrap();
    let canvas_rect = mapping.letterbox_rect().unwrap();
    close(canvas_rect.x, 10.0);
    close(canvas_rect.y, 80.0);
    close(canvas_rect.width, 1920.0);
    close(canvas_rect.height, 1080.0);

    let canvas_point = Point::new(960.0, 540.0);
    let display_point = mapping.canvas_to_display(canvas_point).unwrap();
    close(display_point.x, 970.0);
    close(display_point.y, 620.0);
    let round_trip = mapping.display_to_canvas(display_point).unwrap();
    close(round_trip.x, canvas_point.x);
    close(round_trip.y, canvas_point.y);
    assert!(!mapping.is_on_canvas(Point::new(100.0, 30.0)).unwrap());
    assert!(
        mapping
            .display_to_canvas_checked(Point::new(100.0, 30.0))
            .is_err()
    );
}

#[test]
fn affine_hit_test_matches_cropped_rotated_transform() {
    let source = SourceGeometry::new(100, 60, 1.0).unwrap();
    let transform = Transform {
        position: Point::new(300.0, 200.0),
        anchor: Point::new(0.5, 0.5),
        scale: Point::new(1.75, 0.8),
        rotation_degrees: 37.0,
        crop: Some(CropRect::new(20, 10, 40, 30).unwrap()),
        opacity: 1.0,
    };
    let geometry = VisualGeometry::media(source);
    let inside = geometry
        .forward_point(transform, Point::new(20.0, 15.0))
        .unwrap();
    assert!(geometry.contains_canvas_point(transform, inside).unwrap());
    let outside = geometry
        .forward_point(transform, Point::new(41.0, 15.0))
        .unwrap();
    assert!(!geometry.contains_canvas_point(transform, outside).unwrap());
    assert_eq!(
        geometry.content_extent(transform).unwrap(),
        Size::new(40.0, 30.0).unwrap()
    );

    let invalid = Transform {
        crop: Some(CropRect::new(90, 0, 20, 10).unwrap()),
        ..Transform::identity()
    };
    assert!(
        geometry
            .contains_canvas_point(invalid, Point::ZERO)
            .is_err()
    );
}

#[test]
fn selection_handles_resize_rotation_and_guides_share_canvas_geometry() {
    let geometry = VisualGeometry::shape(100.0, 50.0).unwrap();
    let transform = Transform {
        position: Point::new(100.0, 100.0),
        anchor: Point::ZERO,
        scale: Point::new(1.0, 1.0),
        rotation_degrees: 0.0,
        crop: None,
        opacity: 1.0,
    };
    let selection = SelectionGeometry::new(transform, geometry, 5.0, 20.0).unwrap();
    assert_eq!(selection.corners[0], Point::new(100.0, 100.0));
    assert_eq!(selection.corners[2], Point::new(200.0, 150.0));
    assert_eq!(
        selection.hit_handle(Point::new(200.0, 150.0), 0.0).unwrap(),
        Some(HandleKind::BottomRight)
    );

    let moved =
        drag_transform(transform, Point::new(120.0, 120.0), Point::new(145.0, 80.0)).unwrap();
    assert_eq!(moved.position, Point::new(125.0, 60.0));

    let resized = resize_transform(
        transform,
        geometry,
        HandleKind::BottomRight,
        Point::new(300.0, 200.0),
        ResizeOptions {
            lock_aspect: true,
            ..ResizeOptions::default()
        },
    )
    .unwrap();
    close(resized.scale.x, 2.0);
    close(resized.scale.y, 2.0);
    assert_eq!(
        geometry
            .forward_point(resized, Point::new(0.0, 0.0))
            .unwrap(),
        Point::new(100.0, 100.0)
    );
    assert_eq!(
        geometry
            .forward_point(resized, Point::new(100.0, 50.0))
            .unwrap(),
        Point::new(300.0, 200.0)
    );

    let rotated = rotate_transform(
        transform,
        geometry,
        Point::new(250.0, 125.0),
        Point::new(150.0, 225.0),
    )
    .unwrap();
    close(rotated.rotation_degrees, 90.0);
    close(
        geometry
            .forward_point(rotated, Point::new(50.0, 25.0))
            .unwrap()
            .x,
        150.0,
    );
    close(
        geometry
            .forward_point(rotated, Point::new(50.0, 25.0))
            .unwrap()
            .y,
        125.0,
    );

    let guides = canvas_tools::canvas_guides(Canvas::new(400, 200).unwrap()).unwrap();
    let snapped = snap_to_guides(Point::new(201.0, 99.5), &guides, 2.0).unwrap();
    assert_eq!(snapped, Point::new(200.0, 100.0));
}

#[test]
fn crop_drag_updates_source_rect_and_preserves_transform_fields() {
    let source = SourceGeometry::new(100, 80, 1.0).unwrap();
    let transform = Transform {
        position: Point::new(5.0, 6.0),
        anchor: Point::new(0.5, 0.5),
        scale: Point::new(1.2, 0.8),
        rotation_degrees: 15.0,
        crop: Some(CropRect::new(10, 10, 80, 60).unwrap()),
        opacity: 0.75,
    };
    let geometry = VisualGeometry::media(source);
    let start_canvas = geometry
        .forward_point(transform, Point::new(80.0, 30.0))
        .unwrap();
    let current_canvas = geometry
        .forward_point(transform, Point::new(50.0, 30.0))
        .unwrap();
    let updated = crop_transform_from_canvas_drag(
        transform,
        geometry,
        start_canvas,
        current_canvas,
        HandleKind::Right,
        CropOptions::default(),
    )
    .unwrap();
    assert_eq!(updated.crop, Some(CropRect::new(10, 10, 50, 60).unwrap()));
    assert_eq!(updated.position, transform.position);
    assert_eq!(updated.rotation_degrees, transform.rotation_degrees);
    assert_eq!(updated.opacity, transform.opacity);
}

#[test]
fn inspector_commands_are_model_commands_and_text_edits_are_undoable() {
    let track_id = TrackId::new(2);
    let clip_id = ClipId::new(3);
    let mut project = Project::new(Canvas::new(640, 360).unwrap(), FrameRate::FPS_30);
    project
        .add_track(Track::new(track_id, "Graphics", 2))
        .unwrap();
    let text_clip = Clip::text(clip_id, range(), "before");
    project.add_clip(track_id, text_clip.clone()).unwrap();

    let transform = Transform {
        position: Point::new(20.0, 30.0),
        ..Transform::identity()
    };
    assert!(matches!(
        transform_command(clip_id, transform).unwrap(),
        EditCommand::SetClipTransform { .. }
    ));
    assert!(matches!(
        z_order_command(clip_id, 7).unwrap(),
        EditCommand::SetClipOrder { order: 7, .. }
    ));
    assert!(matches!(
        visibility_command(clip_id, false).unwrap(),
        EditCommand::SetClipVisibility { visible: false, .. }
    ));
    assert!(matches!(
        lock_command(track_id, true).unwrap(),
        EditCommand::SetTrackLocked { locked: true, .. }
    ));

    let text_command = text_properties_command(
        track_id,
        &text_clip,
        TextProperties {
            text: Some("after".to_owned()),
            font_size: Some(72.0),
            alignment: Some(TextAlignment::Center),
            ..TextProperties::default()
        },
    )
    .unwrap();
    assert!(matches!(text_command, EditCommand::Batch { .. }));
    let mut history = ProjectHistory::try_new(project).unwrap();
    history.execute(text_command).unwrap();
    let updated = history.project().clip(clip_id).unwrap();
    let ClipKind::Text(text) = &updated.kind else {
        panic!("expected text clip");
    };
    assert_eq!(text.text, "after");
    assert_eq!(text.style.font_size, 72.0);
    assert_eq!(text.style.alignment, TextAlignment::Center);
    assert!(history.undo().unwrap());
    let ClipKind::Text(text) = &history.project().clip(clip_id).unwrap().kind else {
        panic!("expected text clip after undo");
    };
    assert_eq!(text.text, "before");

    let text_geometry = VisualGeometry::text("hello", &TextStyle::default()).unwrap();
    let layout = slicer::composition::text_layout("hello", &TextStyle::default());
    close(text_geometry.width, layout.width);
    close(text_geometry.height, layout.height);

    let wrong_kind = Clip::shape(ClipId::new(4), range(), Default::default());
    assert!(matches!(
        set_text_command(track_id, &wrong_kind, "nope"),
        Err(InspectorError::WrongClipKind(_))
    ));
}

#[test]
fn crop_and_layer_rows_use_explicit_validation_and_total_order() {
    let track_low = TrackId::new(10);
    let track_high = TrackId::new(11);
    let image_id = ClipId::new(12);
    let top_id = ClipId::new(13);
    let mut project = Project::new_empty();
    let asset_id = slicer::project::AssetId::new(14);
    project
        .add_asset(Asset::image(asset_id, "still.png", 100, 50).unwrap())
        .unwrap();
    project.add_track(Track::new(track_low, "Back", 0)).unwrap();
    project
        .add_track(Track::new(track_high, "Front", 1))
        .unwrap();
    project
        .add_clip(track_low, Clip::image(image_id, range(), asset_id))
        .unwrap();
    project
        .add_clip(track_high, Clip::text(top_id, range(), "top"))
        .unwrap();

    let crop = CropRect::new(5, 5, 40, 20).unwrap();
    let command =
        crop_command(image_id, Transform::identity(), Some(crop), Some((100, 50))).unwrap();
    let mut history = ProjectHistory::try_new(project).unwrap();
    history.execute(command).unwrap();
    assert_eq!(
        history.project().clip(image_id).unwrap().transform.crop,
        Some(crop)
    );
    assert!(
        crop_command(
            image_id,
            Transform::identity(),
            Some(CropRect::new(95, 0, 10, 10).unwrap()),
            Some((100, 50))
        )
        .is_err()
    );

    let rows = layer_rows(history.project());
    assert_eq!(rows[0].clip_id, top_id);
    assert_eq!(rows[1].clip_id, image_id);
}

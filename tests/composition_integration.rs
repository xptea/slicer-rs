use slicer::composition::frame::{Rgba8, RgbaFrame};
use slicer::composition::{InMemoryMedia, render, text_layout};
use slicer::project::{
    Asset, AssetId, Canvas, Clip, ClipId, Color, FrameRate, Project, ShapeClip, ShapeKind,
    TextStyle, Time, TimeRange, Track, TrackId, Transform,
};

fn range(start: i64, end: i64) -> TimeRange {
    TimeRange::new(Time::from_integer(start), Time::from_integer(end)).unwrap()
}

fn project(canvas: Canvas) -> Project {
    Project::new(canvas, FrameRate::FPS_30)
}

#[test]
fn image_pixels_are_composited_back_to_front() {
    let mut project = project(Canvas::new(2, 2).unwrap());
    let asset_id = AssetId::new(1);
    project
        .add_asset(Asset::image(asset_id, "image.png", 2, 2).unwrap())
        .unwrap();
    let track_id = TrackId::new(2);
    project
        .add_track(Track::new(track_id, "Images", 0))
        .unwrap();
    project
        .add_clip(track_id, Clip::image(ClipId::new(3), range(0, 1), asset_id))
        .unwrap();

    let mut frame = RgbaFrame::new(2, 2).unwrap();
    frame.set_pixel(0, 0, Rgba8::new(255, 0, 0, 255));
    frame.set_pixel(1, 0, Rgba8::new(0, 255, 0, 255));
    frame.set_pixel(0, 1, Rgba8::new(0, 0, 255, 255));
    frame.set_pixel(1, 1, Rgba8::new(255, 255, 255, 255));
    let media = InMemoryMedia::new().with_frame(asset_id, frame);
    let output = render(&project, Time::ZERO, &media).unwrap();
    assert_eq!(output.pixel(0, 0), Some(Rgba8::new(255, 0, 0, 255)));
    assert_eq!(output.pixel(1, 0), Some(Rgba8::new(0, 255, 0, 255)));
    assert_eq!(output.pixel(0, 1), Some(Rgba8::new(0, 0, 255, 255)));
    assert_eq!(output.pixel(1, 1), Some(Rgba8::new(255, 255, 255, 255)));
}

#[test]
fn transform_opacity_and_crop_follow_model_semantics() {
    let mut project = project(Canvas::new(2, 1).unwrap());
    let asset_id = AssetId::new(11);
    project
        .add_asset(Asset::image(asset_id, "image.png", 2, 1).unwrap())
        .unwrap();
    let track_id = TrackId::new(12);
    project
        .add_track(Track::new(track_id, "Images", 0))
        .unwrap();
    let mut clip = Clip::image(ClipId::new(13), range(0, 1), asset_id);
    clip.transform = Transform {
        position: slicer::project::Point::new(0.0, 0.0),
        anchor: slicer::project::Point::ZERO,
        scale: slicer::project::Point::new(1.0, 1.0),
        rotation_degrees: 0.0,
        crop: Some(slicer::project::CropRect::new(1, 0, 1, 1).unwrap()),
        opacity: 0.5,
    };
    project.add_clip(track_id, clip).unwrap();
    let mut frame = RgbaFrame::new(2, 1).unwrap();
    frame.set_pixel(0, 0, Rgba8::new(255, 0, 0, 255));
    frame.set_pixel(1, 0, Rgba8::new(0, 0, 255, 255));
    let media = InMemoryMedia::new().with_frame(asset_id, frame);
    let output = render(&project, Time::ZERO, &media).unwrap();
    assert_eq!(output.pixel(0, 0), Some(Rgba8::new(0, 0, 128, 255)));
    assert_eq!(output.pixel(1, 0), Some(Rgba8::new(0, 0, 0, 255)));
}

#[test]
fn shapes_and_text_render_without_platform_dependencies() {
    let mut project = project(Canvas::new(8, 8).unwrap());
    let track_id = TrackId::new(20);
    project
        .add_track(Track::new(track_id, "Graphics", 0))
        .unwrap();
    let mut shape = ShapeClip {
        shape: ShapeKind::Ellipse,
        fill: Color::new(1.0, 0.0, 0.0, 1.0).unwrap(),
        width: 6.0,
        height: 6.0,
        ..ShapeClip::default()
    };
    shape.corner_radius = 0.0;
    project
        .add_clip(track_id, Clip::shape(ClipId::new(21), range(0, 1), shape))
        .unwrap();
    let mut text = Clip::text(ClipId::new(22), range(0, 1), "A");
    text.transform.position = slicer::project::Point::new(0.0, 0.0);
    text.transform.opacity = 1.0;
    text.kind = slicer::project::ClipKind::Text(slicer::project::TextClip {
        text: "A".to_owned(),
        style: TextStyle {
            font_size: 7.0,
            line_height: 1.0,
            ..TextStyle::default()
        },
    });
    text.order = 10;
    project.add_clip(track_id, text).unwrap();
    let output = render(&project, Time::ZERO, &InMemoryMedia::new()).unwrap();
    assert!(output.pixel(3, 3).unwrap().r > 0);
    assert!(output.pixels().iter().any(|channel| *channel > 0));
    let layout = text_layout("hello world", &TextStyle::default());
    assert!(!layout.glyphs.is_empty());
}

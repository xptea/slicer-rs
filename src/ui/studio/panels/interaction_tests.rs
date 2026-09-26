use super::super::{Clip, Event, SlicerApp, Studio, Transform};
use gpui_kit::component::v_flex;
use gpui_kit::gpui::{
    self, AppContext, Context, Entity, Focusable, InteractiveElement, IntoElement, KeyDownEvent,
    Modifiers, MouseButton, MouseMoveEvent, MouseUpEvent, ParentElement, Render, ScrollDelta,
    ScrollWheelEvent, Styled, TestAppContext, Window, point, px, size,
};

struct WorkspaceProbe {
    app: Entity<SlicerApp>,
}
impl Render for WorkspaceProbe {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (content, focus) = self.app.update(cx, |app, cx| {
            app.studio_inputs(window, cx);
            (app.studio_view(cx), app.focus_handle.clone())
        });
        v_flex()
            .size_full()
            .track_focus(&focus)
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                this.app
                    .update(cx, |app, cx| app.studio_keyboard(event, window, cx))
            }))
            .on_mouse_move(cx.listener(|this, e: &MouseMoveEvent, _, cx| {
                this.app.update(cx, |app, cx| {
                    app.studio_drag(
                        e.position,
                        matches!(
                            e.pressed_button,
                            Some(MouseButton::Left | MouseButton::Middle)
                        ),
                        cx,
                    )
                });
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, e: &MouseUpEvent, _, cx| {
                    this.app
                        .update(cx, |app, cx| app.studio_drag(e.position, false, cx));
                }),
            )
            .on_mouse_up(
                MouseButton::Middle,
                cx.listener(|this, e: &MouseUpEvent, _, cx| {
                    this.app
                        .update(cx, |app, cx| app.studio_drag(e.position, false, cx))
                }),
            )
            .child(content)
    }
}

#[gpui::test]
fn panels_resize_and_media_drops_create_a_selected_timeline_clip(cx: &mut TestAppContext) {
    cx.update(|cx| {
        gpui_kit::init(cx);
    });
    let (probe, cx) = cx.add_window_view(|window, cx| {
        let app = cx.new(|cx| {
            let mut app = SlicerApp::new(window, cx, None, None);
            let mut studio = Studio::new();
            let (tx, rx) = std::sync::mpsc::channel();
            tx.send(Event::Imported(vec![Ok(Clip {
                id: 0,
                path: "test.wav".into(),
                start: 0,
                source_in: 0,
                duration: 4_000_000,
                source_duration: 4_000_000,
                visual: false,
                audio: true,
                still: false,
                transform: Transform::default(),
                gain: 1.,
                graphic: None,
                fade_in: 0,
                fade_out: 0,
            })]))
            .unwrap();
            studio.pending = Some(rx);
            studio.poll();
            assert_eq!(studio.project.duration(), 0);
            assert_eq!(studio.project.media.len(), 1);
            app.studio = Some(studio);
            app
        });
        WorkspaceProbe { app }
    });
    cx.simulate_resize(size(px(1280.), px(840.)));
    cx.run_until_parked();
    cx.update(|window, cx| {
        window.draw(cx).clear(cx);
    });
    cx.update(|window, cx| {
        window.draw(cx).clear(cx);
    });
    let app = cx.update(|_, cx| probe.read(cx).app.clone());
    let (bounds, sizes) = cx.update(|_, cx| {
        let studio = app.read(cx).studio.as_ref().unwrap();
        (studio.workspace.unwrap(), studio.panel_sizes)
    });
    let left = point(
        bounds.origin.x + px(sizes[0] + 2.5),
        bounds.origin.y + px(100.),
    );
    cx.simulate_mouse_down(left, MouseButton::Left, Modifiers::none());
    cx.simulate_mouse_move(
        left + point(px(60.), px(0.)),
        MouseButton::Left,
        Modifiers::none(),
    );
    cx.simulate_mouse_up(
        left + point(px(60.), px(0.)),
        MouseButton::Left,
        Modifiers::none(),
    );
    cx.run_until_parked();
    cx.update(|window, cx| {
        window.draw(cx).clear(cx);
    });
    cx.update(|_, cx| {
        let studio = app.read(cx).studio.as_ref().unwrap();
        assert!((studio.panel_sizes[0] - (sizes[0] + 60.)).abs() < 1.);
        assert_eq!(studio.panel_sizes[1], sizes[1]);
    });
    for (index, from, delta) in [
        (
            1,
            point(
                bounds.origin.x + bounds.size.width - px(sizes[1] + 2.5),
                bounds.origin.y + px(100.),
            ),
            point(px(-40.), px(0.)),
        ),
        (
            2,
            point(
                bounds.origin.x + px(100.),
                bounds.origin.y + bounds.size.height - px(sizes[2] + 2.5),
            ),
            point(px(0.), px(-50.)),
        ),
    ] {
        cx.simulate_mouse_down(from, MouseButton::Left, Modifiers::none());
        cx.simulate_mouse_move(from + delta, MouseButton::Left, Modifiers::none());
        cx.simulate_mouse_up(from + delta, MouseButton::Left, Modifiers::none());
        cx.run_until_parked();
        cx.update(|window, cx| {
            window.draw(cx).clear(cx);
        });
        cx.update(|_, cx| {
            let studio = app.read(cx).studio.as_ref().unwrap();
            let expected = sizes[index] + if index == 1 { 40. } else { 50. };
            assert!((studio.panel_sizes[index] - expected).abs() < 1.);
        });
    }
    let start = point(bounds.origin.x + px(50.), bounds.origin.y + px(55.));
    let target = cx.update(|_, cx| {
        let timeline = app.read(cx).studio.as_ref().unwrap().timeline.unwrap();
        point(timeline.origin.x + px(220.), timeline.origin.y + px(44.))
    });
    cx.simulate_mouse_down(start, MouseButton::Left, Modifiers::none());
    cx.simulate_mouse_move(
        start + point(px(10.), px(0.)),
        MouseButton::Left,
        Modifiers::none(),
    );
    cx.simulate_mouse_move(target, MouseButton::Left, Modifiers::none());
    cx.simulate_mouse_up(target, MouseButton::Left, Modifiers::none());
    cx.run_until_parked();
    cx.update(|_, cx| {
        let studio = app.read(cx).studio.as_ref().unwrap();
        assert_eq!(studio.project.media.len(), 1);
        assert_eq!(studio.project.tracks[0].clips.len(), 1);
        let clip = &studio.project.tracks[0].clips[0];
        assert_eq!(studio.selected, Some(clip.id));
        assert!((clip.start - 2_000_000).abs() < 30_000);
    });
    cx.update(|window, cx| {
        window.draw(cx).clear(cx);
    });
    let properties = cx
        .debug_bounds("clip-property-6")
        .expect("Selected clip has volume properties");
    cx.simulate_click(
        point(
            properties.origin.x + properties.size.width - px(12.),
            properties.origin.y + properties.size.height / 2.,
        ),
        Modifiers::none(),
    );
    cx.run_until_parked();
    cx.update(|_, cx| {
        let studio = app.read(cx).studio.as_ref().unwrap();
        assert!((studio.project.tracks[0].clips[0].gain - 1.05).abs() < 0.001);
        assert_eq!(studio.project.media[0].gain, 1.);
    });
}

#[gpui::test]
fn text_typing_clipboard_wheel_and_canvas_fields_work_together(cx: &mut TestAppContext) {
    use slicer::engine::project::Graphic;
    cx.update(|cx| gpui_kit::init(cx));
    let (probe, cx) = cx.add_window_view(|window, cx| {
        let app = cx.new(|cx| {
            let mut app = SlicerApp::new(window, cx, None, None);
            app.studio = Some(Studio::new());
            app.screen = super::super::super::Screen::Studio;
            app
        });
        WorkspaceProbe { app }
    });
    cx.simulate_resize(size(px(1280.), px(1100.)));
    cx.run_until_parked();
    let app = cx.update(|window, cx| {
        window.draw(cx).clear(cx);
        probe.read(cx).app.clone()
    });
    cx.update(|window, cx| app.update(cx, |app, cx| app.focus_handle.focus(window, cx)));
    cx.simulate_keystrokes("t");
    cx.run_until_parked();
    let id = cx.update(|window, cx| {
        window.draw(cx).clear(cx);
        app.read(cx).studio.as_ref().unwrap().selected.unwrap()
    });
    cx.update(|window, cx| {
        app.update(cx, |app, cx| {
            app.studio
                .as_ref()
                .unwrap()
                .inputs
                .as_ref()
                .unwrap()
                .text
                .read(cx)
                .focus_handle(cx)
                .focus(window, cx);
        })
    });
    cx.simulate_keystrokes("ctrl-a");
    cx.simulate_input("Hello world");
    cx.run_until_parked();
    cx.update(|window,cx|app.update(cx,|app,cx| {
        let s=app.studio.as_ref().unwrap();
        assert!(matches!(&s.project.clip(id).unwrap().graphic,Some(Graphic::Text(t)) if t.text=="Hello world"));
        assert!(!s.playing,"typing spaces must not start playback");
        app.focus_handle.focus(window,cx);
    }));
    cx.simulate_keystrokes("ctrl-c shift-right shift-right ctrl-v");
    cx.run_until_parked();
    cx.update(|_, cx| {
        let s = app.read(cx).studio.as_ref().unwrap();
        assert_eq!(
            s.project
                .tracks
                .iter()
                .map(|t| t.clips.len())
                .sum::<usize>(),
            2
        );
        assert_eq!(
            s.project.clip(s.selected.unwrap()).unwrap().start,
            2_000_000
        );
    });
    cx.simulate_keystrokes("ctrl-z");
    cx.run_until_parked();
    cx.update(|_, cx| {
        assert_eq!(
            app.read(cx)
                .studio
                .as_ref()
                .unwrap()
                .project
                .tracks
                .iter()
                .map(|t| t.clips.len())
                .sum::<usize>(),
            1
        )
    });
    cx.simulate_keystrokes("ctrl-shift-z");
    cx.run_until_parked();
    let (bounds, before) = cx.update(|window, cx| {
        window.draw(cx).clear(cx);
        let s = app.read(cx).studio.as_ref().unwrap();
        (s.timeline.unwrap(), s.zoom)
    });
    cx.update(|window, cx| {
        window.dispatch_event(
            gpui::PlatformInput::ScrollWheel(ScrollWheelEvent {
                position: bounds.origin + point(px(300.), px(40.)),
                delta: ScrollDelta::Lines(point(0., 2.)),
                modifiers: Modifiers {
                    control: true,
                    ..Default::default()
                },
                ..Default::default()
            }),
            cx,
        );
    });
    cx.run_until_parked();
    cx.update(|_, cx| {
        assert!(
            app.read(cx).studio.as_ref().unwrap().zoom > before,
            "Ctrl+wheel must zoom even over a track"
        )
    });
    cx.update(|window, cx| {
        app.update(cx, |app, cx| {
            let s = app.studio.as_mut().unwrap();
            s.canvas_settings = true;
            let fields = s.inputs.as_ref().unwrap().fields.clone();
            for (index, value) in [(8, "720"), (9, "1280"), (10, "30")] {
                fields[index].update(cx, |input, cx| input.set_value(value, window, cx));
            }
            app.apply_studio_fields(true, window, cx);
            assert_eq!(
                (
                    app.studio.as_ref().unwrap().project.width,
                    app.studio.as_ref().unwrap().project.height
                ),
                (720, 1280)
            );
        })
    });
    cx.run_until_parked();
    cx.update(|window, cx| {
        window.draw(cx).clear(cx);
        let s = app.read(cx).studio.as_ref().unwrap();
        let b = s.bounds.unwrap();
        assert!((f32::from(b.size.width) / f32::from(b.size.height) - 720. / 1280.).abs() < 0.001);
        s.project.validate().unwrap();
    });
}

#[gpui::test]
fn mouse_pan_zoom_group_drag_and_locked_keyboard_edits(cx: &mut TestAppContext) {
    use slicer::engine::project::{Graphic, SECOND, TextStyle, Track};
    cx.update(gpui_kit::init);
    let (probe, cx) = cx.add_window_view(|window, cx| {
        let app = cx.new(|cx| {
            let mut app = SlicerApp::new(window, cx, None, None);
            let mut s = Studio::new();
            s.project.tracks.clear();
            s.project
                .add_graphic(Graphic::Text(TextStyle::default()), SECOND);
            s.project
                .add_graphic(Graphic::Text(TextStyle::default()), 3 * SECOND);
            for _ in 0..10 {
                s.project.tracks.push(Track::new("Empty"));
            }
            s.snapping = false;
            app.studio = Some(s);
            app.screen = super::super::super::Screen::Studio;
            app
        });
        WorkspaceProbe { app }
    });
    cx.simulate_resize(size(px(1280.), px(840.)));
    cx.run_until_parked();
    let app = cx.update(|window, cx| {
        window.draw(cx).clear(cx);
        probe.read(cx).app.clone()
    });
    let bounds = cx.update(|window, cx| {
        window.draw(cx).clear(cx);
        app.read(cx).studio.as_ref().unwrap().timeline.unwrap()
    });
    let first = bounds.origin + point(px(220.), px(60.));
    let second = bounds.origin + point(px(340.), px(134.));
    cx.simulate_mouse_down(first, MouseButton::Left, Modifiers::none());
    cx.simulate_mouse_up(first, MouseButton::Left, Modifiers::none());
    cx.simulate_mouse_down(
        second,
        MouseButton::Left,
        Modifiers {
            control: true,
            ..Default::default()
        },
    );
    cx.simulate_mouse_up(
        second,
        MouseButton::Left,
        Modifiers {
            control: true,
            ..Default::default()
        },
    );
    cx.run_until_parked();
    cx.update(|_, cx| assert_eq!(app.read(cx).studio.as_ref().unwrap().selection().len(), 2));
    cx.simulate_mouse_down(first, MouseButton::Left, Modifiers::none());
    cx.simulate_mouse_move(
        first + point(px(60.), px(0.)),
        MouseButton::Left,
        Modifiers::none(),
    );
    cx.simulate_mouse_up(
        first + point(px(60.), px(0.)),
        MouseButton::Left,
        Modifiers::none(),
    );
    cx.run_until_parked();
    cx.update(|_, cx| {
        let s = app.read(cx).studio.as_ref().unwrap();
        assert_eq!(s.project.tracks[0].clips[0].start, 2 * SECOND);
        assert_eq!(s.project.tracks[1].clips[0].start, 4 * SECOND);
    });
    let ruler = bounds.origin + point(px(400.), px(12.));
    cx.simulate_mouse_down(ruler, MouseButton::Middle, Modifiers::none());
    cx.simulate_mouse_move(
        ruler - point(px(60.), px(0.)),
        MouseButton::Middle,
        Modifiers::none(),
    );
    cx.simulate_mouse_up(
        ruler - point(px(60.), px(0.)),
        MouseButton::Middle,
        Modifiers::none(),
    );
    cx.run_until_parked();
    cx.update(|_, cx| {
        let s = app.read(cx).studio.as_ref().unwrap();
        assert_eq!(s.offset, SECOND);
        assert_eq!(s.time, 0, "panning must not scrub");
    });
    for shift in [false, true] {
        cx.update(|window, cx| {
            window.dispatch_event(
                gpui::PlatformInput::ScrollWheel(ScrollWheelEvent {
                    position: first,
                    delta: ScrollDelta::Lines(point(0., -2.)),
                    modifiers: Modifiers {
                        shift,
                        ..Default::default()
                    },
                    ..Default::default()
                }),
                cx,
            );
        });
        cx.run_until_parked();
    }
    cx.update(|window, cx| {
        window.draw(cx).clear(cx);
        app.update(cx, |app, cx| {
            let s = app.studio.as_mut().unwrap();
            assert!(s.offset > SECOND, "wheel pans horizontally");
            assert!(
                f32::from(s.track_scroll.offset().y) < 0.,
                "Shift+wheel scrolls tracks"
            );
            s.project.tracks[1].locked = true;
            app.focus_handle.focus(window, cx);
        });
    });
    cx.simulate_keystrokes("alt-right");
    cx.run_until_parked();
    cx.update(|_, cx| {
        let s = app.read(cx).studio.as_ref().unwrap();
        assert!(s.project.tracks[0].clips[0].start > 2 * SECOND);
        assert_eq!(
            s.project.tracks[1].clips[0].start,
            4 * SECOND,
            "locked clip cannot be nudged"
        );
    });
    cx.simulate_keystrokes("delete");
    cx.run_until_parked();
    cx.update(|_, cx| {
        let s = app.read(cx).studio.as_ref().unwrap();
        assert!(s.project.tracks[0].clips.is_empty());
        assert_eq!(
            s.project.tracks[1].clips.len(),
            1,
            "locked clip cannot be deleted"
        );
    });
}

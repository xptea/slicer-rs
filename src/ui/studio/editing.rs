use super::*;
use gpui_kit::component::input::{Textarea, TextareaState};
use slicer::engine::project::{Graphic, TextStyle};
use std::collections::BTreeSet;

pub(super) struct StudioInputs {
    pub(super) text: Entity<TextareaState>,
    pub(super) fields: Vec<Entity<InputState>>,
    selected: Option<u64>,
    _subscriptions: Vec<Subscription>,
}
impl Studio {
    pub(super) fn selection(&self) -> BTreeSet<u64> {
        if self.selected.is_some_and(|id| self.selection.contains(&id)) {
            self.selection
                .iter()
                .copied()
                .filter(|id| self.project.clip(*id).is_some())
                .collect()
        } else {
            self.selected.into_iter().collect()
        }
    }
    pub(super) fn timeline_width(&self) -> f32 {
        self.timeline
            .map_or(600., |b| f32::from(b.size.width) - 100.)
            .max(1.)
    }
    pub(super) fn pan(&mut self, pixels: f32) {
        self.offset = (self.offset + (pixels / self.zoom * SECOND as f32) as Time)
            .clamp(0, self.project.duration());
    }
    pub(super) fn zoom_at(&mut self, factor: f32, x: f32) {
        let x = x.clamp(0., self.timeline_width());
        let anchor = self.offset as f64 + x as f64 / self.zoom as f64 * SECOND as f64;
        self.zoom = (self.zoom * factor).clamp(
            (fitted_zoom(self.project.duration(), self.timeline_width()) / 16.).max(0.01),
            600.,
        );
        self.offset = (anchor - x as f64 / self.zoom as f64 * SECOND as f64)
            .round()
            .max(0.) as Time;
    }
    pub(super) fn reveal_playhead(&mut self) {
        let span = (self.timeline_width() / self.zoom * SECOND as f32) as Time;
        if self.time < self.offset {
            self.offset = self.time;
        }
        if self.time > self.offset + span {
            self.offset = (self.time - span * 9 / 10).max(0);
        }
    }
    pub(super) fn copy_selection(&mut self) {
        let selected = self.selection();
        self.clipboard = self
            .project
            .tracks
            .iter()
            .enumerate()
            .flat_map(|(t, track)| {
                track
                    .clips
                    .iter()
                    .filter(|c| selected.contains(&c.id))
                    .cloned()
                    .map(move |c| (t, c))
            })
            .collect();
    }
    pub(super) fn paste(&mut self, at: Time) {
        if self.clipboard.is_empty() {
            return;
        }
        let origin = self.clipboard.iter().map(|(_, c)| c.start).min().unwrap();
        let clips = self.clipboard.clone();
        if clips
            .iter()
            .all(|(t, _)| self.project.tracks.get(*t).is_none_or(|track| track.locked))
        {
            return;
        }
        self.checkpoint();
        self.selection.clear();
        for (t, mut clip) in clips {
            if self.project.tracks.get(t).is_none_or(|track| track.locked) {
                continue;
            }
            clip.id = self.project.next_id;
            self.project.next_id += 1;
            clip.start = at + clip.start - origin;
            self.selection.insert(clip.id);
            self.selected = Some(clip.id);
            self.project.tracks[t].clips.push(clip);
        }
        self.sync();
    }
    pub(super) fn delete_selection(&mut self, ripple: bool) {
        let ids = self.selection();
        let removable: Vec<_> = ids
            .iter()
            .copied()
            .filter(|id| self.project.editable(*id))
            .collect();
        if removable.is_empty() {
            return;
        }
        self.checkpoint();
        for track in &mut self.project.tracks {
            if track.locked {
                continue;
            }
            let mut ranges: Vec<_> = track
                .clips
                .iter()
                .filter(|c| removable.contains(&c.id))
                .map(|c| (c.start, c.end()))
                .collect();
            ranges.sort();
            let mut merged: Vec<(Time, Time)> = Vec::new();
            for (a, b) in ranges {
                if let Some(last) = merged.last_mut().filter(|last| a <= last.1) {
                    last.1 = last.1.max(b);
                } else {
                    merged.push((a, b));
                }
            }
            track.clips.retain(|c| !removable.contains(&c.id));
            if ripple {
                for c in &mut track.clips {
                    c.start -= merged
                        .iter()
                        .filter(|(_, b)| *b <= c.start)
                        .map(|(a, b)| b - a)
                        .sum::<Time>();
                }
            }
        }
        self.selection.clear();
        self.selected = None;
        self.sync();
    }
    pub(super) fn add_graphic(&mut self, text: bool) {
        self.checkpoint();
        let graphic = if text {
            Graphic::Text(TextStyle::default())
        } else {
            Graphic::Color {
                color: [32, 48, 64, 255],
            }
        };
        self.selected = Some(self.project.add_graphic(graphic, self.time));
        if text {
            self.track_scroll.scroll_to_bottom();
        } else {
            self.track_scroll.set_offset(point(px(0.), px(0.)));
        }
        self.selection.clear();
        self.canvas_settings = false;
        self.shortcuts_open = false;
        self.pause();
        self.sync();
    }
    pub(super) fn fit_clip(&mut self, fill: bool) {
        let Some(c) = self
            .selected
            .and_then(|id| self.project.clip(id))
            .cloned()
            .filter(|c| self.project.editable(c.id))
        else {
            return;
        };
        let aspect = self
            .project
            .media
            .iter()
            .find(|a| a.path == c.path)
            .map(|a| a.transform.width / a.transform.height * 1920. / 1080.)
            .unwrap_or(self.project.width as f32 / self.project.height as f32);
        let relative = aspect / (self.project.width as f32 / self.project.height as f32);
        let (w, h) = if (relative > 1.) == fill {
            (relative, 1.)
        } else {
            (1., 1. / relative)
        };
        self.checkpoint();
        let c = self.project.clip_mut(c.id).unwrap();
        c.transform = Transform {
            width: w,
            height: h,
            ..Transform::default()
        };
        self.sync();
    }
}

fn parse_color(value: &str) -> Option<[u8; 4]> {
    let value = value.trim().trim_start_matches('#');
    if value.len() != 6 && value.len() != 8 {
        return None;
    }
    let n = u32::from_str_radix(value, 16).ok()?;
    Some(if value.len() == 6 {
        [(n >> 16) as u8, (n >> 8) as u8, n as u8, 255]
    } else {
        [(n >> 24) as u8, (n >> 16) as u8, (n >> 8) as u8, n as u8]
    })
}
fn hex(c: [u8; 4]) -> String {
    format!("#{:02X}{:02X}{:02X}{:02X}", c[0], c[1], c[2], c[3])
}
impl SlicerApp {
    pub(in crate::ui) fn studio_inputs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.screen != Screen::Studio {
            return;
        }
        if self.studio.as_ref().unwrap().inputs.is_none() {
            let text = cx.new(|cx| TextareaState::new(window, cx).rows(3));
            let fields = (0..11)
                .map(|_| cx.new(|cx| InputState::new(window, cx)))
                .collect::<Vec<_>>();
            let subscription = cx.subscribe(&text, |this, input, event, cx| {
                if !matches!(event, InputEvent::Change) {
                    return;
                }
                let value = input.read(cx).value().to_string();
                let Some(s) = this.studio.as_mut() else {
                    return;
                };
                let Some(id) = s.selected.filter(|id| s.project.editable(*id)) else {
                    return;
                };
                if value.len() > 16384 {
                    return;
                }
                if s.project
                    .clip(id)
                    .is_some_and(|c| matches!(&c.graphic,Some(Graphic::Text(t)) if t.text != value))
                {
                    s.checkpoint();
                    if let Some(Graphic::Text(t)) = &mut s.project.clip_mut(id).unwrap().graphic {
                        t.text = value;
                    }
                    s.sync();
                    cx.notify();
                }
            });
            self.studio.as_mut().unwrap().inputs = Some(StudioInputs {
                text,
                fields,
                selected: None,
                _subscriptions: vec![subscription],
            });
        }
        let s = self.studio.as_mut().unwrap();
        let inputs = s.inputs.as_mut().unwrap();
        let changed = inputs.selected != s.selected;
        inputs.selected = s.selected;
        let clip = s.selected.and_then(|id| s.project.clip(id));
        let style = clip
            .and_then(|c| match &c.graphic {
                Some(Graphic::Text(t)) => Some(t),
                _ => None,
            })
            .cloned()
            .unwrap_or_default();
        let color = clip
            .and_then(|c| match c.graphic {
                Some(Graphic::Color { color }) => Some(color),
                _ => None,
            })
            .unwrap_or(style.color);
        if changed || !inputs.text.read(cx).focus_handle(cx).is_focused(window) {
            if inputs.text.read(cx).value().as_ref() != style.text {
                inputs.text.update(cx, |state, cx| {
                    state.set_value(style.text.clone(), window, cx)
                });
            }
        }
        let values = [
            style.size.to_string(),
            style.font,
            hex(color),
            hex(style.background),
            format!("{:.3}", clip.map_or(0, |c| c.start) as f64 / 1e6),
            format!(
                "{:.3}",
                clip.map_or(5 * SECOND, |c| c.duration) as f64 / 1e6
            ),
            format!("{:.3}", clip.map_or(0, |c| c.fade_in) as f64 / 1e6),
            format!("{:.3}", clip.map_or(0, |c| c.fade_out) as f64 / 1e6),
            s.project.width.to_string(),
            s.project.height.to_string(),
            format!("{:.3}", s.project.fps_num as f64 / s.project.fps_den as f64),
        ];
        // Numeric drafts survive focus changes until Apply. Refresh on selection,
        // undo, presets, or a successful Apply through an explicit invalidation.
        if changed || s.refresh_inputs {
            for (input, value) in inputs.fields.iter().zip(values) {
                input.update(cx, |state, cx| state.set_value(value, window, cx));
            }
            s.refresh_inputs = false;
        }
    }
    fn studio_typing(&self, window: &Window, cx: &App) -> bool {
        self.studio
            .as_ref()
            .and_then(|s| s.inputs.as_ref())
            .is_some_and(|i| {
                i.text.read(cx).focus_handle(cx).is_focused(window)
                    || i.fields
                        .iter()
                        .any(|field| field.read(cx).focus_handle(cx).is_focused(window))
            })
    }
    pub(super) fn apply_studio_fields(
        &mut self,
        canvas: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let s = self.studio.as_mut().unwrap();
        let Some(inputs) = &s.inputs else {
            return;
        };
        let values: Vec<String> = inputs
            .fields
            .iter()
            .map(|i| i.read(cx).value().to_string())
            .collect();
        let result = (|| -> anyhow::Result<()> {
            if canvas {
                let w = values[8].trim().parse::<u32>()?;
                let h = values[9].trim().parse::<u32>()?;
                let fps = values[10].trim().parse::<f64>()?;
                anyhow::ensure!(
                    fps.is_finite() && (1. ..=240.).contains(&fps),
                    "Frame rate must be 1–240"
                );
                let mut project = s.project.clone();
                project.resize_canvas(w, h)?;
                let (num, den) = if (fps - 29.97).abs() < 0.001 {
                    (30000, 1001)
                } else if (fps - 59.94).abs() < 0.001 {
                    (60000, 1001)
                } else if (fps - 23.976).abs() < 0.001 {
                    (24000, 1001)
                } else {
                    ((fps * 1000.).round() as u32, 1000)
                };
                project.fps_num = num;
                project.fps_den = den;
                project.validate()?;
                s.checkpoint();
                s.project = project;
            } else {
                let id = s
                    .selected
                    .filter(|id| s.project.editable(*id))
                    .ok_or_else(|| anyhow::anyhow!("Select an unlocked clip"))?;
                let mut clip = s.project.clip(id).unwrap().clone();
                let seconds = |index: usize| -> anyhow::Result<Time> {
                    let n = values[index].trim().parse::<f64>()?;
                    anyhow::ensure!(
                        n.is_finite() && (0. ..=86400.).contains(&n),
                        "Use a time between 0 and 86400 seconds"
                    );
                    Ok((n * 1e6).round() as Time)
                };
                clip.start = seconds(4)?;
                clip.duration = seconds(5)?;
                clip.fade_in = seconds(6)?;
                clip.fade_out = seconds(7)?;
                anyhow::ensure!(
                    clip.duration > 0
                        && (clip.still || clip.source_in + clip.duration <= clip.source_duration),
                    "Duration exceeds the available source"
                );
                match &mut clip.graphic {
                    Some(Graphic::Text(t)) => {
                        t.size = values[0].trim().parse()?;
                        t.font = values[1].trim().into();
                        t.color = parse_color(&values[2])
                            .ok_or_else(|| anyhow::anyhow!("Use #RRGGBB or #RRGGBBAA"))?;
                        t.background = parse_color(&values[3])
                            .ok_or_else(|| anyhow::anyhow!("Invalid background color"))?;
                    }
                    Some(Graphic::Color { color }) => {
                        *color = parse_color(&values[2])
                            .ok_or_else(|| anyhow::anyhow!("Invalid color"))?
                    }
                    _ => {}
                }
                let mut project = s.project.clone();
                *project.clip_mut(id).unwrap() = clip;
                project.validate()?;
                s.checkpoint();
                s.project = project;
            }
            Ok(())
        })();
        match result {
            Ok(()) => {
                s.form_error = None;
                s.refresh_inputs = true;
                s.sync();
                self.focus_handle.focus(window, cx);
            }
            Err(e) => s.form_error = Some(e.to_string()),
        }
        cx.notify();
    }
    fn field(&self, label: &'static str, index: usize) -> AnyElement {
        let input = &self
            .studio
            .as_ref()
            .unwrap()
            .inputs
            .as_ref()
            .unwrap()
            .fields[index];
        v_flex()
            .gap_1()
            .child(div().text_xs().text_color(ink(MUTED)).child(label))
            .child(Input::new(input).small())
            .into_any_element()
    }
    pub(super) fn studio_extra_properties(&self, cx: &mut Context<Self>) -> AnyElement {
        let s = self.studio.as_ref().unwrap();
        let mut view = v_flex().gap_2();
        if s.shortcuts_open {
            view = view.child(div().text_sm().font_semibold().child("Timeline controls"));
            for (keys, action) in [
                ("Wheel / middle drag", "Pan timeline"),
                ("Ctrl+wheel / pinch", "Zoom at pointer"),
                ("Shift+wheel", "Scroll tracks"),
                ("Trackpad", "Horizontal: pan; vertical: scroll tracks"),
                (
                    "Ctrl+click / Shift+click",
                    "Toggle selection / select range",
                ),
                ("Drag clips / edges", "Move selection / trim clip"),
                ("Space / T / B", "Play-pause / add text / split"),
                ("Ctrl+C / X / V / D", "Copy / cut / paste / duplicate"),
                ("Delete / Shift+Delete", "Delete / ripple delete"),
                ("Ctrl+Z / Ctrl+Shift+Z", "Undo / redo"),
                ("← / →", "One frame; Shift: one second"),
                ("Ctrl+← / →", "Previous / next clip edge"),
                ("Alt+← / →", "Nudge clips; Shift: ten frames"),
                ("Home / End", "Timeline start / end"),
                ("+ / − / F", "Zoom in / out / fit"),
                ("Ctrl+A / Escape / N", "Select all / clear / snap"),
            ] {
                view = view.child(
                    v_flex()
                        .text_xs()
                        .child(div().font_semibold().child(keys))
                        .child(div().text_color(ink(MUTED)).child(action)),
                );
            }
            return view
                .child(
                    Button::new("close-shortcuts")
                        .compact()
                        .label("Back to properties")
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.studio.as_mut().unwrap().shortcuts_open = false;
                            cx.notify();
                        })),
                )
                .into_any_element();
        }
        let canvas = s.canvas_settings || s.selected.is_none();
        view = view.child(
            h_flex()
                .gap_1()
                .child(
                    Button::new("properties-clip")
                        .ghost()
                        .compact()
                        .label("Clip")
                        .selected(!canvas)
                        .disabled(s.selected.is_none())
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.studio.as_mut().unwrap().canvas_settings = false;
                            cx.notify();
                        })),
                )
                .child(
                    Button::new("properties-canvas")
                        .ghost()
                        .compact()
                        .label("Canvas")
                        .selected(canvas)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.studio.as_mut().unwrap().canvas_settings = true;
                            cx.notify();
                        })),
                ),
        );
        let Some(inputs) = &s.inputs else {
            return view.into_any_element();
        };
        if canvas {
            view = view.child(div().text_sm().font_semibold().child("Output size"));
            for (i, (label, w, h)) in [
                ("16:9 · 1080p", 1920, 1080),
                ("9:16 · Vertical", 1080, 1920),
                ("1:1 · Square", 1080, 1080),
                ("4:5 · Portrait", 1080, 1350),
                ("16:9 · 4K", 3840, 2160),
            ]
            .into_iter()
            .enumerate()
            {
                view = view.child(
                    Button::new(("canvas-preset", i))
                        .ghost()
                        .compact()
                        .label(label)
                        .selected(s.project.width == w && s.project.height == h)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            let s = this.studio.as_mut().unwrap();
                            s.checkpoint();
                            let _ = s.project.resize_canvas(w, h);
                            s.refresh_inputs = true;
                            s.sync();
                            cx.notify();
                        })),
                );
            }
            view = view
                .child(self.field("Width (px)", 8))
                .child(self.field("Height (px)", 9))
                .child(self.field("Frames per second", 10));
        } else if let Some(clip) = s.selected.and_then(|id| s.project.clip(id)) {
            let editable = s.project.editable(clip.id);
            match &clip.graphic {
                Some(Graphic::Text(t)) => {
                    view = view.child(
                        Textarea::new(&inputs.text)
                            .h(px(90.))
                            .disabled(!editable)
                            .aria_label("Text content"),
                    );
                    view = view
                        .child(self.field("Font family (Sans, Serif, Mono, or installed name)", 1))
                        .child(self.field("Font size", 0));
                    let mut row = h_flex().gap_1();
                    for (i, label, icon, active) in [
                        (0, "Bold", IconName::Bold, t.bold),
                        (1, "Italic", IconName::Italic, t.italic),
                        (2, "Align left", IconName::TextAlignStart, t.align == 0),
                        (3, "Align center", IconName::TextAlignCenter, t.align == 1),
                        (4, "Align right", IconName::TextAlignEnd, t.align == 2),
                    ] {
                        row = row.child(
                            Button::new(("text-style", i as usize))
                                .ghost()
                                .compact()
                                .icon(icon)
                                .tooltip(label)
                                .accessibility_label(label)
                                .selected(active)
                                .disabled(!editable)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    let s = this.studio.as_mut().unwrap();
                                    let Some(id) = s.selected.filter(|id| s.project.editable(*id))
                                    else {
                                        return;
                                    };
                                    s.checkpoint();
                                    if let Some(Graphic::Text(t)) =
                                        &mut s.project.clip_mut(id).unwrap().graphic
                                    {
                                        match i {
                                            0 => t.bold = !t.bold,
                                            1 => t.italic = !t.italic,
                                            _ => t.align = (i - 2) as u8,
                                        }
                                    }
                                    s.sync();
                                    cx.notify();
                                })),
                        );
                    }
                    view = view
                        .child(row.flex_wrap())
                        .child(self.field("Text color", 2))
                        .child(self.field("Text box color (alpha 00 = transparent)", 3));
                }
                Some(Graphic::Color { .. }) => view = view.child(self.field("Background color", 2)),
                _ => {}
            }
            view = view
                .child(self.field("Start (seconds)", 4))
                .child(self.field("Duration (seconds)", 5))
                .child(self.field("Fade in (seconds)", 6))
                .child(self.field("Fade out (seconds)", 7));
            if clip.visual && clip.graphic.is_none() {
                view = view.child(
                    h_flex()
                        .gap_1()
                        .child(
                            Button::new("clip-fit")
                                .ghost()
                                .compact()
                                .label("Fit")
                                .disabled(!editable)
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.studio.as_mut().unwrap().fit_clip(false);
                                    cx.notify();
                                })),
                        )
                        .child(
                            Button::new("clip-fill")
                                .ghost()
                                .compact()
                                .label("Fill")
                                .disabled(!editable)
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.studio.as_mut().unwrap().fit_clip(true);
                                    cx.notify();
                                })),
                        ),
                );
            }
        }
        view = view.child(
            Button::new("properties-apply")
                .compact()
                .label(if canvas {
                    "Apply canvas"
                } else {
                    "Apply properties"
                })
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.apply_studio_fields(canvas, window, cx)
                })),
        );
        if let Some(error) = &s.form_error {
            view = view.child(div().text_xs().text_color(ink(BAD)).child(error.clone()));
        }
        view.into_any_element()
    }
    pub(super) fn studio_scroll(&mut self, event: &ScrollWheelEvent, cx: &mut Context<Self>) {
        let s = self.studio.as_mut().unwrap();
        let (x, y, lines) = match event.delta {
            ScrollDelta::Pixels(p) => (f32::from(p.x), f32::from(p.y), false),
            ScrollDelta::Lines(p) => (p.x * 40., p.y * 40., true),
        };
        if event.modifiers.control || event.modifiers.platform || event.modifiers.alt {
            let anchor = s
                .timeline
                .map_or(0., |b| f32::from(event.position.x - b.origin.x) - 100.);
            s.zoom_at((y * 0.005).exp(), anchor);
        } else if event.modifiers.shift {
            let offset = s.track_scroll.offset();
            s.track_scroll.set_offset(offset + point(px(0.), px(y)));
        } else if lines {
            s.pan(-if x.abs() > y.abs() { x } else { y });
        } else if x.abs() > y.abs() {
            s.pan(-x);
        } else {
            return;
        }
        cx.stop_propagation();
        cx.notify();
    }
    pub(in crate::ui) fn studio_keyboard(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.studio_typing(window, cx) {
            return;
        }
        let s = self.studio.as_mut().unwrap();
        let m = event.keystroke.modifiers;
        let ctrl = m.control || m.platform;
        let frame = SECOND * s.project.fps_den as i64 / s.project.fps_num as i64;
        match event.keystroke.key.as_str() {
            "space" => s.toggle(),
            "delete" | "backspace" => s.delete_selection(m.shift),
            "s" if ctrl => s.save(),
            "o" if ctrl => s.load(),
            "i" if ctrl => s.import_dialog(),
            "z" if ctrl => {
                if m.shift {
                    s.history.redo(&mut s.project)
                } else {
                    s.history.undo(&mut s.project)
                }
                s.dirty = true;
                s.refresh_inputs = true;
                s.sync();
            }
            "y" if ctrl => {
                s.history.redo(&mut s.project);
                s.dirty = true;
                s.refresh_inputs = true;
                s.sync();
            }
            "c" if ctrl => s.copy_selection(),
            "x" if ctrl => {
                s.copy_selection();
                s.delete_selection(false);
            }
            "v" if ctrl => s.paste(s.time),
            "a" if ctrl => {
                s.selection = s
                    .project
                    .tracks
                    .iter()
                    .flat_map(|t| &t.clips)
                    .map(|c| c.id)
                    .collect();
                s.selected = s.selection.iter().next().copied();
            }
            "d" if ctrl => s.duplicate(),
            "b" => s.split(),
            "t" if !ctrl => s.add_graphic(true),
            "escape" => {
                s.selected = None;
                s.selection.clear();
                s.drag = None;
            }
            "home" => {
                s.seek(0);
                s.reveal_playhead();
            }
            "end" => {
                s.seek(s.project.duration());
                s.reveal_playhead();
            }
            "left" | "arrowleft" | "right" | "arrowright" => {
                let direction = if event.keystroke.key.contains("left") {
                    -1
                } else {
                    1
                };
                if m.alt {
                    let ids = s.selection();
                    let min = ids
                        .iter()
                        .filter(|id| s.project.editable(**id))
                        .filter_map(|id| s.project.clip(*id))
                        .map(|c| c.start)
                        .min()
                        .unwrap_or(0);
                    let delta = (frame * direction * if m.shift { 10 } else { 1 }).max(-min);
                    s.checkpoint();
                    for id in ids {
                        if s.project.editable(id) {
                            if let Some(c) = s.project.clip_mut(id) {
                                c.start = (c.start + delta).max(0);
                            }
                        }
                    }
                    s.refresh_inputs = true;
                    s.sync();
                } else if ctrl {
                    let mut edges: Vec<_> = s
                        .project
                        .tracks
                        .iter()
                        .flat_map(|t| &t.clips)
                        .flat_map(|c| [c.start, c.end()])
                        .collect();
                    edges.sort();
                    let target = if direction < 0 {
                        edges.into_iter().rev().find(|t| *t < s.time)
                    } else {
                        edges.into_iter().find(|t| *t > s.time)
                    };
                    if let Some(t) = target {
                        s.seek(t);
                    }
                } else {
                    s.seek(s.time + direction * if m.shift { SECOND } else { frame });
                }
                s.reveal_playhead();
            }
            "+" | "=" => s.zoom_at(
                1.25,
                ((s.time - s.offset) as f32 / SECOND as f32 * s.zoom).clamp(0., s.timeline_width()),
            ),
            "-" => s.zoom_at(
                0.8,
                ((s.time - s.offset) as f32 / SECOND as f32 * s.zoom).clamp(0., s.timeline_width()),
            ),
            "f" if !ctrl => {
                s.zoom = fitted_zoom(s.project.duration(), s.timeline_width());
                s.offset = 0;
            }
            "pageup" => s.pan(-s.timeline_width() * 0.8),
            "pagedown" => s.pan(s.timeline_width() * 0.8),
            "n" if !ctrl => s.snapping = !s.snapping,
            _ => return,
        }
        cx.stop_propagation();
        cx.notify();
    }
}

#[cfg(test)]
mod tests {
    use super::{Graphic, SECOND, Studio, TextStyle};
    #[test]
    fn clipboard_ripple_and_pointer_zoom_preserve_relative_positions() {
        let mut s = Studio::new();
        let a = s
            .project
            .add_graphic(Graphic::Text(TextStyle::default()), 2 * SECOND);
        let b = s
            .project
            .add_graphic(Graphic::Text(TextStyle::default()), 4 * SECOND);
        s.selection = [a, b].into_iter().collect();
        s.selected = Some(a);
        s.copy_selection();
        s.paste(10 * SECOND);
        let starts: Vec<_> = s
            .selection()
            .iter()
            .map(|id| s.project.clip(*id).unwrap().start)
            .collect();
        assert_eq!(starts, vec![10 * SECOND, 12 * SECOND]);
        let anchor = s.offset as f64 + 250. / s.zoom as f64 * SECOND as f64;
        s.zoom_at(1.8, 250.);
        let after = s.offset as f64 + 250. / s.zoom as f64 * SECOND as f64;
        assert!((anchor - after).abs() < 2.);
        s.delete_selection(false);
        assert_eq!(
            s.project
                .tracks
                .iter()
                .map(|t| t.clips.len())
                .sum::<usize>(),
            2
        );
        s.history.undo(&mut s.project);
        assert_eq!(
            s.project
                .tracks
                .iter()
                .map(|t| t.clips.len())
                .sum::<usize>(),
            4
        );
        s.project.validate().unwrap();
    }
}

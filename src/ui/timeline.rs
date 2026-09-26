//! Draggable trim handles, playhead, and transport controls.
use super::*;

const HANDLE_WIDTH: f32 = 10.;

// Shared by the single-file and multitrack timelines.
pub(super) fn paint_trim_handle(window: &mut Window, center: Pixels, top: Pixels) {
    window.paint_quad(
        fill(
            Bounds::new(
                point(center - px(HANDLE_WIDTH / 2.), top),
                size(px(HANDLE_WIDTH), px(46.)),
            ),
            ink(ACCENT_STRONG),
        )
        .corner_radii(px(4.)),
    );
}
pub(super) fn paint_playhead(window: &mut Window, x: Pixels, top: Pixels) {
    window.paint_quad(
        fill(
            Bounds::new(point(x - px(1.), top + px(6.)), size(px(2.), px(66.))),
            ink(TEXT),
        )
        .corner_radii(px(5.)),
    );
    window.paint_quad(
        fill(
            Bounds::new(point(x - px(5.), top + px(2.)), size(px(10.), px(8.))),
            ink(TEXT),
        )
        .corner_radii(px(5.)),
    );
}

// Audio peaks are normalized to their real sample amplitude. Most spoken
// recordings sit well below full scale, which makes a linear waveform render
// collapse to a one-pixel line. These values only shape the visualization;
// they never modify playback or exported audio.
const WAVEFORM_AMPLITUDE: f32 = 17.;
const WAVEFORM_GAMMA: f32 = 0.65;
const WAVEFORM_GAIN: f32 = 1.2;
const WAVEFORM_MIN_EXTENT: f32 = 0.04;

pub(super) fn visible_waveform_extent(raw_extent: f32) -> f32 {
    if !raw_extent.is_finite() || raw_extent <= 0.0 {
        return 0.0;
    }
    (raw_extent.clamp(0.0, 1.0).powf(WAVEFORM_GAMMA) * WAVEFORM_GAIN)
        .clamp(WAVEFORM_MIN_EXTENT, 1.0)
}

fn time_bounds(bounds: Bounds<Pixels>) -> Bounds<Pixels> {
    Bounds::new(
        point(bounds.origin.x + px(HANDLE_WIDTH / 2.), bounds.origin.y),
        size(
            (bounds.size.width - px(HANDLE_WIDTH)).max(px(0.)),
            bounds.size.height,
        ),
    )
}

#[derive(Clone, Copy)]
pub(super) enum DragTarget {
    Start,
    End,
    Playhead,
}
impl SlicerApp {
    pub(super) fn trim_range(&self, cx: &App) -> (f64, f64) {
        let duration = self.media.as_ref().map_or(0.0, |media| {
            if media.duration.is_finite() {
                media.duration.max(0.0)
            } else {
                0.0
            }
        });
        let start = parse_timestamp(&self.start_input.read(cx).value())
            .filter(|seconds| seconds.is_finite())
            .unwrap_or(0.0)
            .clamp(0.0, duration);
        let end = parse_timestamp(&self.end_input.read(cx).value())
            .filter(|seconds| seconds.is_finite())
            .unwrap_or(duration)
            .clamp(start, duration);
        (start, end)
    }

    /// The trim end is exclusive, so seek just before it when selecting the
    /// final frame for preview or transport navigation.
    pub(super) fn trim_end_position(&self, cx: &App) -> f64 {
        let (start, end) = self.trim_range(cx);
        (end - 0.001).max(start)
    }

    /// Keep the displayed playhead inside the selected trim. The exclusive end
    /// remains a valid display position so the playhead can meet the end handle
    /// center when playback reaches the boundary; seek-to-end uses
    /// `trim_end_position` to request the final decodable frame.
    pub(super) fn clamp_playhead(&self, seconds: f64, cx: &App) -> f64 {
        let (start, end) = self.trim_range(cx);
        if seconds.is_finite() {
            seconds.clamp(start, end)
        } else {
            start
        }
    }
    pub(super) fn toggle_playback(&mut self, cx: &mut Context<Self>) {
        // Native libmpv may finish loading before the metadata worker. Keep transport disabled
        // until trim_range has a real duration; otherwise the provisional 0..0 range is rejected
        // and an early keyboard/shortcut play request can report a spurious error.
        if self.media.is_none() {
            return;
        }
        if !self.native.paused {
            self.native.pause();
            return;
        }
        let (start, end) = self.trim_range(cx);
        self.native.set_range(start, end);
        if self.preview_seconds < start || self.preview_seconds >= end - 0.02 {
            self.native.seek(start, true);
            self.preview_seconds = start;
        }
        self.native.play();
    }
    pub(super) fn poll_playback(&mut self, _cx: &mut Context<Self>) {
        if let Some(position) = self.native.poll()
            && self.timeline_drag.is_none()
        {
            self.preview_seconds = position;
        }
        self.preview_error = self.native.error.clone();
    }
    pub(super) fn finish_timeline_drag(&mut self, cx: &mut Context<Self>) {
        if let Some(target) = self.timeline_drag.take() {
            let (start, end) = self.trim_range(cx);
            self.native.set_range(start, end);
            let position = match target {
                DragTarget::Start => start,
                DragTarget::End => self.trim_end_position(cx),
                DragTarget::Playhead => self.clamp_playhead(self.preview_seconds, cx),
            };
            self.preview_seconds = position;
            self.native.seek(position, true);
        }
    }
    pub(super) fn transport(&self, cx: &mut Context<Self>) -> AnyElement {
        let controls = h_flex()
            .absolute()
            .inset_0()
            .items_center()
            .justify_center()
            .gap_3()
            .child(
                Button::new("step-back")
                    .ghost()
                    .icon(gpui_kit::assets::IconName::SkipBack)
                    .rounded_full()
                    .disabled(!self.native.ready || self.media.is_none() || self.crop.open)
                    .on_click(cx.listener(|this, _, _, cx| this.seek_to_trim_start(cx))),
            )
            .child(
                Button::new("play-pause")
                    .secondary()
                    .rounded_full()
                    .icon(if !self.native.paused {
                        gpui_kit::assets::IconName::Pause
                    } else {
                        gpui_kit::assets::IconName::Play
                    })
                    .disabled(
                        !self.native.ready
                            || self.media.is_none()
                            || self.export_job.is_some()
                            || self.crop.open,
                    )
                    .on_click(cx.listener(|this, _, _, cx| this.toggle_playback(cx))),
            )
            .child(
                Button::new("step-forward")
                    .ghost()
                    .icon(gpui_kit::assets::IconName::SkipForward)
                    .rounded_full()
                    .disabled(!self.native.ready || self.media.is_none() || self.crop.open)
                    .on_click(cx.listener(|this, _, _, cx| this.seek_to_trim_end(cx))),
            );
        let mute = Button::new("mute-preview")
            .ghost()
            .rounded_full()
            .accessibility_label(if self.native.muted {
                "Unmute audio in preview and export"
            } else {
                "Mute audio in preview and export"
            })
            .icon(if self.native.muted {
                gpui_kit::assets::IconName::VolumeX
            } else {
                gpui_kit::assets::IconName::Volume2
            })
            .disabled(!self.native.ready || self.media.is_none() || self.crop.open)
            .on_click(cx.listener(|this, _, _, _| this.native.toggle_mute()));

        div()
            .relative()
            .w_full()
            .h(px(40.))
            .when(!self.crop.open, |view| view.child(controls))
            .child(
                h_flex()
                    .absolute()
                    .left_0()
                    .top_0()
                    .gap_2()
                    .child(
                        Button::new("crop-video")
                            .ghost()
                            .rounded_full()
                            .icon(gpui_kit::assets::IconName::Crop)
                            .disabled(!self.native.ready || self.export_job.is_some())
                            .on_click(cx.listener(|this, _, _, cx| {
                                if this.crop.open {
                                    this.crop.open = false;
                                    this.finish_crop_drag();
                                } else {
                                    this.open_crop(cx);
                                }
                                cx.notify();
                            })),
                    )
                    .when(self.crop.open, |row| row.child(self.crop_tools(cx))),
            )
            .when_some(self.crop.error(), |view, error| {
                view.child(
                    div()
                        .absolute()
                        .bottom(px(44.))
                        .left_0()
                        .text_sm()
                        .text_color(ink(BAD))
                        .child(error.to_owned()),
                )
            })
            .when(!self.crop.open, |view| {
                view.child(
                    div()
                        .absolute()
                        .top_0()
                        .bottom_0()
                        .right_0()
                        .flex()
                        .items_center()
                        .child(mute),
                )
            })
            .into_any_element()
    }
    pub(super) fn drag_timeline(&mut self, x: Pixels, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(bounds), Some(target), Some(info)) = (
            self.timeline_bounds,
            self.timeline_drag,
            self.media.as_ref(),
        ) else {
            return;
        };
        let duration = info.duration;
        if duration <= 0.0 || bounds.size.width <= px(0.) {
            return;
        }
        let value = (((x - bounds.origin.x) / bounds.size.width) as f64).clamp(0.0, 1.0) * duration;
        let (start, end) = self.trim_range(cx);
        let gap = 0.001_f64.min(duration);
        match target {
            DragTarget::Start => self.set_input_value(
                &self.start_input.clone(),
                format_timestamp(value.min((end - gap).max(0.0))),
                window,
                cx,
            ),
            DragTarget::End => self.set_input_value(
                &self.end_input.clone(),
                format_timestamp(value.max(start + gap).min(duration)),
                window,
                cx,
            ),
            DragTarget::Playhead => {
                let value = self.clamp_playhead(value, cx);
                if let Some(path) = self.editor_path.clone() {
                    self.request_preview(path, value);
                }
            }
        }
        match target {
            DragTarget::Start | DragTarget::End => {
                let (start, _) = self.trim_range(cx);
                let position = if matches!(target, DragTarget::Start) {
                    start
                } else {
                    self.trim_end_position(cx)
                };
                self.preview_seconds = position;
                self.native.seek(position, false);
            }
            DragTarget::Playhead => {}
        }
        cx.notify();
    }
    pub(super) fn timeline(&self, cx: &mut Context<Self>) -> AnyElement {
        let duration = self.media.as_ref().map_or(0.0, |m| m.duration);
        let (start, end) = self.trim_range(cx);
        let ratio = |v: f64| {
            if duration > 0.0 {
                (v / duration).clamp(0.0, 1.0) as f32
            } else {
                0.0
            }
        };
        let playhead = self.clamp_playhead(self.preview_seconds, cx);
        let (a, b, head) = (ratio(start), ratio(end), ratio(playhead));
        let entity = cx.entity().downgrade();
        let waveform = self.waveform.clone();
        v_flex()
            .w_full()
            .gap_2()
            .child(
                div()
                    .id("edit-timeline")
                    .w_full()
                    .h(px(76.))
                    .relative()
                    .cursor_pointer()
                    .child(
                        canvas(
                            move |bounds, _, cx| {
                                let _ = entity.update(cx, |this, _| {
                                    this.timeline_bounds = Some(time_bounds(bounds))
                                });
                            },
                            move |bounds, _, window, _| {
                                let x = bounds.origin.x;
                                let y = bounds.origin.y;
                                let w = bounds.size.width;
                                let track = Bounds::new(point(x, y + px(22.)), size(w, px(42.)));
                                window.paint_quad(
                                    fill(track, ink(SURFACE_RAISED)).corner_radii(px(10.)),
                                );
                                let domain = time_bounds(bounds);
                                let left = domain.origin.x + domain.size.width * a;
                                let right = domain.origin.x + domain.size.width * b;
                                window.paint_quad(
                                    fill(
                                        Bounds::new(
                                            point(left, y + px(22.)),
                                            size(right - left, px(42.)),
                                        ),
                                        ink(0x505050ff),
                                    )
                                    .corner_radii(px(10.)),
                                );
                                if let Some(waveform) =
                                    waveform.as_ref().filter(|waveform| waveform.has_audio())
                                {
                                    // Draw a compact, symmetric peak envelope over the
                                    // complete source duration. Sampling at roughly two
                                    // pixels per column keeps the paint work bounded while
                                    // preserving transients when the window is resized.
                                    let columns =
                                        ((domain.size.width / px(2.)) as usize).clamp(128, 1_024);
                                    let center = y + px(43.);
                                    let amplitude = px(WAVEFORM_AMPLITUDE);
                                    for column in 0..columns {
                                        let peak = waveform.peak_for_column(column, columns);
                                        let extent = visible_waveform_extent(
                                            peak.min.abs().max(peak.max.abs()),
                                        );
                                        if extent <= 0.0 {
                                            continue;
                                        }
                                        let t0 = column as f32 / columns as f32;
                                        let t1 = (column + 1) as f32 / columns as f32;
                                        let bar_left = domain.origin.x + domain.size.width * t0;
                                        let bar_right = domain.origin.x + domain.size.width * t1;
                                        let height = amplitude * extent;
                                        let selected = t0 >= a && t0 <= b;
                                        window.paint_quad(
                                            fill(
                                                Bounds::new(
                                                    point(bar_left, center - height),
                                                    size(
                                                        (bar_right - bar_left).max(px(1.)),
                                                        height * 2.,
                                                    ),
                                                ),
                                                ink(if selected { 0xc8c8c8ff } else { 0x777777ff }),
                                            )
                                            .corner_radii(px(1.)),
                                        );
                                    }
                                }
                                for center in [left, right] {
                                    paint_trim_handle(window, center, y + px(20.));
                                }
                                paint_playhead(
                                    window,
                                    domain.origin.x + domain.size.width * head,
                                    y,
                                );
                            },
                        )
                        .size_full(),
                    )
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                            if this.export_job.is_some() || this.crop.open || duration <= 0.0 {
                                return;
                            }
                            if let Some(bounds) = this.timeline_bounds {
                                let x = event.position.x;
                                this.timeline_drag = Some(
                                    if (x - (bounds.origin.x + bounds.size.width * a)).abs()
                                        < px(12.)
                                    {
                                        DragTarget::Start
                                    } else if (x - (bounds.origin.x + bounds.size.width * b)).abs()
                                        < px(12.)
                                    {
                                        DragTarget::End
                                    } else {
                                        DragTarget::Playhead
                                    },
                                );
                                this.native.pause();
                                this.drag_timeline(x, window, cx);
                            }
                        }),
                    )
                    .on_mouse_up(
                        MouseButton::Left,
                        cx.listener(|this, _, _, cx| this.finish_timeline_drag(cx)),
                    )
                    .on_mouse_up_out(
                        MouseButton::Left,
                        cx.listener(|this, _, _, cx| this.finish_timeline_drag(cx)),
                    ),
            )
            .child(
                h_flex()
                    .w_full()
                    .justify_between()
                    .text_sm()
                    .text_color(ink(MUTED))
                    .child(format!(
                        "{} / {}",
                        format_timestamp(playhead),
                        format_timestamp(duration)
                    ))
                    .child(format!(
                        "Selected {}",
                        format_timestamp((end - start).max(0.0))
                    )),
            )
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::visible_waveform_extent;

    #[test]
    fn waveform_visualization_keeps_quiet_audio_visible() {
        assert_eq!(visible_waveform_extent(0.0), 0.0);
        assert!(visible_waveform_extent(0.01) >= 0.04);
        assert!(visible_waveform_extent(0.1) > 0.1);
        assert!(visible_waveform_extent(1.0) <= 1.0);
    }
}

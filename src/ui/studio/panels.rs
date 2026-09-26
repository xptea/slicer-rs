use super::*;

pub(super) const PANEL_GAP: f32 = 5.;

// Gaps belong to the shared dividers, never to either adjacent panel.
pub(super) fn panel_sizes(width: f32, height: f32, requested: [f32; 3]) -> [f32; 3] {
    let available = (width - PANEL_GAP * 2.).max(3.);
    let min_side = 160_f32.min(available / 4.);
    let min_center = 240_f32.min(available / 3.);
    let side_budget = available - min_center;
    let left = requested[0].clamp(min_side, side_budget - min_side);
    let right = requested[1].clamp(min_side, side_budget - left);
    let vertical = (height - PANEL_GAP).max(2.);
    let min_bottom = 150_f32.min(vertical / 2.);
    let min_top = 160_f32.min(vertical / 2.);
    [
        left,
        right,
        requested[2].clamp(min_bottom, vertical - min_top),
    ]
}

pub(super) fn heading(title: &'static str) -> AnyElement {
    div()
        .h(px(26.))
        .flex_shrink_0()
        .px_2()
        .flex()
        .items_center()
        .text_xs()
        .font_semibold()
        .text_color(ink(MUTED))
        .child(title)
        .into_any_element()
}

#[derive(Clone)]
pub(super) struct MediaDrag {
    pub path: PathBuf,
}
impl Render for MediaDrag {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .p_2()
            .rounded(px(5.))
            .bg(ink(SURFACE_RAISED))
            .border_1()
            .border_color(ink(ACCENT_STRONG))
            .text_color(ink(TEXT))
            .text_sm()
            .child(
                self.path
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_string(),
            )
    }
}

impl SlicerApp {
    pub(super) fn studio_splitter(
        &self,
        index: usize,
        sizes: [f32; 3],
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let splitter = div()
            .id(("panel-divider", index))
            .flex_shrink_0()
            .bg(ink(BG))
            .hover(|style| style.bg(ink(0x607c80ff)))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                    this.studio.as_mut().unwrap().drag = Some(Drag::Panel {
                        index,
                        start: event.position,
                        sizes,
                    });
                    cx.stop_propagation();
                    cx.notify();
                }),
            );
        if index == 2 {
            splitter
                .h(px(PANEL_GAP))
                .w_full()
                .cursor_ns_resize()
                .into_any_element()
        } else {
            splitter
                .w(px(PANEL_GAP))
                .h_full()
                .cursor_ew_resize()
                .into_any_element()
        }
    }

    pub(super) fn studio_files(&self, width: f32, cx: &mut Context<Self>) -> AnyElement {
        let s = self.studio.as_ref().unwrap();
        let mut files = v_flex()
            .id("media-file-list")
            .flex_1()
            .min_h(px(0.))
            .overflow_y_scroll()
            .p_1()
            .gap_1();
        if s.project.media.is_empty() {
            files = files.child(div().p_3().text_sm().text_color(ink(MUTED)).child(
                "Drop video, audio, or images here. Then drag a file onto a timeline track.",
            ));
        }
        for (i, clip) in s.project.media.iter().enumerate() {
            let mut poster = div()
                .w(px(64.))
                .h(px(44.))
                .flex_shrink_0()
                .bg(ink(BG))
                .rounded(px(3.))
                .flex()
                .items_center()
                .justify_center()
                .overflow_hidden();
            if let Some(image) = s
                .thumbnails
                .image(&super::super::timeline_thumbnails::poster(&clip.path))
            {
                poster = poster.child(img(image).size_full().object_fit(ObjectFit::Contain));
            } else {
                poster = poster.child(if clip.visual {
                    IconName::Film
                } else {
                    IconName::Music
                });
            }
            files = files.child(
                h_flex()
                    .id(("media-file", i))
                    .gap_2()
                    .p_1()
                    .rounded(px(4.))
                    .bg(ink(SURFACE_RAISED))
                    .hover(|style| style.bg(ink(SURFACE_HOVER)))
                    .cursor_move()
                    .child(poster)
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w(px(0.))
                            .overflow_hidden()
                            .child(
                                div().text_xs().overflow_hidden().child(
                                    clip.path
                                        .file_name()
                                        .unwrap_or_default()
                                        .to_string_lossy()
                                        .to_string(),
                                ),
                            )
                            .child(div().text_xs().text_color(ink(MUTED)).child(format!(
                                "{} · {}",
                                if clip.still {
                                    "Image"
                                } else if clip.visual {
                                    "Video"
                                } else {
                                    "Audio"
                                },
                                format_timestamp(clip.duration as f64 / SECOND as f64)
                            ))),
                    )
                    .on_drag(
                        MediaDrag {
                            path: clip.path.clone(),
                        },
                        |item, _, _, cx| cx.new(|_| item.clone()),
                    ),
            );
        }
        v_flex()
            .w(px(width))
            .h_full()
            .flex_shrink_0()
            .min_h(px(0.))
            .bg(ink(SURFACE))
            .border_1()
            .border_color(ink(BORDER))
            .rounded(px(WINDOW_RADIUS))
            .overflow_hidden()
            .child(
                h_flex()
                    .h(px(30.))
                    .flex_shrink_0()
                    .px_2()
                    .child(
                        div()
                            .text_xs()
                            .font_semibold()
                            .flex_1()
                            .child(format!("Files ({})", s.project.media.len())),
                    )
                    .child(
                        Button::new("files-import")
                            .ghost()
                            .compact()
                            .icon(IconName::FilePlus)
                            .tooltip("Import media")
                            .accessibility_label("Import media")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.studio.as_mut().unwrap().import_dialog();
                                cx.notify();
                            })),
                    ),
            )
            .child(files)
            .into_any_element()
    }

    pub(super) fn studio_properties(&self, width: f32, cx: &mut Context<Self>) -> AnyElement {
        let s = self.studio.as_ref().unwrap();
        let mut content = v_flex()
            .id("clip-property-list")
            .flex_1()
            .min_h(px(0.))
            .overflow_y_scroll()
            .p_2()
            .gap_2();
        content = content.child(self.studio_extra_properties(cx));
        if let Some(clip) = s
            .selected
            .and_then(|id| s.project.clip(id))
            .filter(|_| !s.canvas_settings && !s.shortcuts_open)
        {
            let editable = s.project.editable(clip.id);
            content = content.child(
                div()
                    .text_sm()
                    .font_semibold()
                    .overflow_hidden()
                    .child(clip.label()),
            );
            for (label, value) in [
                (
                    "Type",
                    if let Some(graphic) = &clip.graphic {
                        match graphic {
                            slicer::engine::project::Graphic::Text(_) => "Text",
                            slicer::engine::project::Graphic::Color { .. } => "Color",
                        }
                    } else if clip.still {
                        "Image"
                    } else if clip.visual {
                        "Video"
                    } else {
                        "Audio"
                    }
                    .to_owned(),
                ),
                (
                    "Timeline start",
                    format_timestamp(clip.start as f64 / SECOND as f64),
                ),
                (
                    "Duration",
                    format_timestamp(clip.duration as f64 / SECOND as f64),
                ),
                (
                    "Source in",
                    format_timestamp(clip.source_in as f64 / SECOND as f64),
                ),
                (
                    "Source out",
                    format_timestamp((clip.source_in + clip.duration) as f64 / SECOND as f64),
                ),
            ] {
                content = content.child(
                    h_flex()
                        .justify_between()
                        .text_xs()
                        .child(div().text_color(ink(MUTED)).child(label))
                        .child(value),
                );
            }
            if !editable {
                content = content.child(
                    div()
                        .text_xs()
                        .text_color(ink(MUTED))
                        .child("Track is locked"),
                );
            }
            for (label, value, field) in [
                ("Position X", format!("{:.0}%", clip.transform.x * 100.), 0),
                ("Position Y", format!("{:.0}%", clip.transform.y * 100.), 1),
                ("Width", format!("{:.0}%", clip.transform.width * 100.), 2),
                ("Height", format!("{:.0}%", clip.transform.height * 100.), 3),
                ("Rotation", format!("{:.0}°", clip.transform.rotation), 4),
                (
                    "Opacity",
                    format!("{:.0}%", clip.transform.opacity * 100.),
                    5,
                ),
                ("Volume", format!("{:.0}%", clip.gain * 100.), 6),
            ] {
                if (field < 6 && !clip.visual) || (field == 6 && !clip.audio) {
                    continue;
                }
                let mut row = h_flex()
                    .debug_selector(move || format!("clip-property-{field}"))
                    .gap_1()
                    .text_xs()
                    .child(div().flex_1().child(label));
                for direction in [-1., 1.] {
                    if direction > 0. {
                        row = row.child(div().w(px(40.)).text_center().child(value.clone()));
                    }
                    row = row.child(
                        Button::new((
                            if direction < 0. {
                                "property-minus"
                            } else {
                                "property-plus"
                            },
                            field as usize,
                        ))
                        .ghost()
                        .compact()
                        .icon(if direction < 0. {
                            IconName::Minus
                        } else {
                            IconName::Plus
                        })
                        .tooltip(format!(
                            "{} {label}",
                            if direction < 0. {
                                "Decrease"
                            } else {
                                "Increase"
                            }
                        ))
                        .accessibility_label(format!(
                            "{} {label}",
                            if direction < 0. {
                                "Decrease"
                            } else {
                                "Increase"
                            }
                        ))
                        .disabled(!editable)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            let s = this.studio.as_mut().unwrap();
                            let Some(id) = s.selected.filter(|id| s.project.editable(*id)) else {
                                return;
                            };
                            s.checkpoint();
                            let clip = s.project.clip_mut(id).unwrap();
                            match field {
                                0 => clip.transform.x += direction * 0.01,
                                1 => clip.transform.y += direction * 0.01,
                                2 => {
                                    clip.transform.width =
                                        (clip.transform.width + direction * 0.01).max(0.01)
                                }
                                3 => {
                                    clip.transform.height =
                                        (clip.transform.height + direction * 0.01).max(0.01)
                                }
                                4 => {
                                    clip.transform.rotation =
                                        (clip.transform.rotation + direction * 15.).rem_euclid(360.)
                                }
                                5 => {
                                    clip.transform.opacity =
                                        (clip.transform.opacity + direction * 0.05).clamp(0., 1.)
                                }
                                _ => clip.gain = (clip.gain + direction * 0.05).clamp(0., 4.),
                            }
                            s.sync();
                            cx.notify();
                        })),
                    );
                }
                content = content.child(row);
            }
        } else if !s.shortcuts_open && s.selected.is_none() {
            content = content.child(
                div()
                    .text_sm()
                    .text_color(ink(MUTED))
                    .child("Select a timeline clip to view and edit its properties."),
            );
        }
        v_flex()
            .w(px(width))
            .h_full()
            .flex_shrink_0()
            .min_h(px(0.))
            .bg(ink(SURFACE))
            .border_1()
            .border_color(ink(BORDER))
            .rounded(px(WINDOW_RADIUS))
            .overflow_hidden()
            .child(heading("Properties"))
            .child(content)
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::{PANEL_GAP, panel_sizes};

    #[test]
    fn shared_dividers_leave_one_gap_and_keep_panels_inside_the_window() {
        for (width, height) in [(1280., 720.), (800., 500.), (480., 320.)] {
            for wanted in [
                [220., 240., 290.],
                [2000., 2000., 2000.],
                [-10., -10., -10.],
            ] {
                let [left, right, bottom] = panel_sizes(width, height, wanted);
                let center = width - left - right - 2. * PANEL_GAP;
                let top = height - bottom - PANEL_GAP;
                assert!(left > 0. && right > 0. && center > 0. && top > 0. && bottom > 0.);
                let preview_left = left + PANEL_GAP;
                let properties_left = preview_left + center + PANEL_GAP;
                assert_eq!(preview_left - left, 5.);
                assert_eq!(properties_left - (preview_left + center), 5.);
                assert!((properties_left + right - width).abs() < 0.001);
                assert!((top + PANEL_GAP + bottom - height).abs() < 0.001);
            }
        }
    }
}

#[cfg(all(test, feature = "ui-tests"))]
mod interaction_tests;

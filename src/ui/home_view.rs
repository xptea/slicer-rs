//! Home drop area and the three recent-video cards.

use super::*;

impl SlicerApp {
    pub(super) fn home_view(&self, cx: &mut Context<Self>) -> AnyElement {
        let dragging = self.external_drop.is_some();
        let cards = if self.recent.is_empty() {
            div()
                .text_color(ink(MUTED))
                .text_sm()
                .text_center()
                .child(
                    if self.library_scanning {
                        "Scanning your library…"
                    } else if let Some(error) = self.library_error.as_ref() {
                        error.as_str()
                    } else if self.settings.library_directory.is_none() {
                        "Choose a library folder in Settings"
                    } else {
                        "No videos in this folder yet"
                    }
                    .to_owned(),
                )
                .into_any_element()
        } else {
            let mut row = h_flex()
                .w_full()
                .justify_center()
                .items_start()
                .gap(px(32.));
            for (index, video) in self.recent.iter().enumerate() {
                row = row.child(self.video_card(index, video, cx));
            }
            row.into_any_element()
        };
        let open = div()
            .id("home-open-video")
            .rounded(px(12.))
            .px(px(16.))
            .py(px(10.))
            .border_1()
            .border_color(ink(BORDER))
            .cursor_pointer()
            .hover(|style| style.bg(ink(SURFACE_HOVER)))
            .child("Open Video File")
            .on_click(cx.listener(|this, _, _, _| this.launch_dialog(DialogKind::Open)));

        div()
            .size_full()
            .px(px(12.))
            .pb(px(12.))
            .child(
                v_flex()
                    .size_full()
                    .relative()
                    .id("home-drop-zone")
                    .rounded(px(18.))
                    .items_center()
                    .justify_center()
                    .child(
                        canvas(
                            |_, _, _| {},
                            move |bounds, _, window, _| {
                                let mut border =
                                    PathBuilder::stroke(px(if dragging { 3. } else { 1. }))
                                        .dash_array(&[px(8.), px(5.)]);
                                let left = bounds.origin.x + px(if dragging { 1.5 } else { 0.5 });
                                let top = bounds.origin.y + px(if dragging { 1.5 } else { 0.5 });
                                let right = bounds.origin.x + bounds.size.width
                                    - px(if dragging { 1.5 } else { 0.5 });
                                let bottom = bounds.origin.y + bounds.size.height
                                    - px(if dragging { 1.5 } else { 0.5 });
                                let radius = px(18.);
                                border.move_to(point(left + radius, top));
                                border.line_to(point(right - radius, top));
                                border.curve_to(point(right, top + radius), point(right, top));
                                border.line_to(point(right, bottom - radius));
                                border
                                    .curve_to(point(right - radius, bottom), point(right, bottom));
                                border.line_to(point(left + radius, bottom));
                                border.curve_to(point(left, bottom - radius), point(left, bottom));
                                border.line_to(point(left, top + radius));
                                border.curve_to(point(left + radius, top), point(left, top));
                                border.close();
                                if let Ok(path) = border.build() {
                                    window
                                        .paint_path(path, ink(if dragging { TEXT } else { MUTED }));
                                }
                            },
                        )
                        .absolute()
                        .size_full(),
                    )
                    .child(
                        v_flex()
                            .w_full()
                            .max_w(px(640.))
                            .px(px(24.))
                            .items_center()
                            .child(div().text_size(px(20.)).font_bold().child(if dragging {
                                "Drop video to open"
                            } else {
                                "Drop your video on screen"
                            }))
                            .child(div().mt(px(6.)).mb(px(12.)).child("or"))
                            .child(open)
                            .child(div().mt(px(48.)).mb(px(18.)).child("Recent files:"))
                            .child(cards),
                    ),
            )
            .into_any_element()
    }

    pub(super) fn video_card(
        &self,
        index: usize,
        video: &home::RecentVideo,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let thumbnail = self
            .thumbnails
            .get(index)
            .and_then(|slot| slot.image.clone());
        let path = video.path.clone();
        let image_element = if let Some(image) = thumbnail {
            // Remove intrinsic image dimensions from layout so the fixed 16:9
            // container cannot crop off the image's lower rounded corners.
            img(image)
                .absolute()
                .size_full()
                .rounded(px(12.))
                .object_fit(ObjectFit::Cover)
                .into_any_element()
        } else {
            v_flex()
                .size_full()
                .items_center()
                .justify_center()
                .gap_2()
                .bg(ink(SURFACE_RAISED))
                .child(div().text_color(ink(ACCENT)).text_size(px(30.)).child("▶"))
                .child(
                    div().text_color(ink(MUTED)).text_sm().child(
                        if self
                            .thumbnails
                            .get(index)
                            .and_then(|slot| slot.error.as_ref())
                            .is_some()
                        {
                            "Preview unavailable"
                        } else {
                            "Loading frame"
                        },
                    ),
                )
                .into_any_element()
        };
        div()
            .id(format!("recent-card-{index}"))
            .rounded(px(12.))
            .w(px(160.))
            .flex_shrink_0()
            .min_w(px(0.))
            .cursor_pointer()
            .overflow_hidden()
            .hover(|style| style.text_color(ink(TEXT)))
            .on_click(cx.listener(move |this, _, window, cx| {
                this.open_file(path.clone(), window, cx);
            }))
            .child(
                div()
                    .relative()
                    .h(px(90.))
                    .flex_shrink_0()
                    .overflow_hidden()
                    .rounded(px(12.))
                    .w_full()
                    .child(image_element),
            )
            .child(
                div()
                    .mt(px(10.))
                    .text_color(ink(MUTED))
                    .font_semibold()
                    .text_sm()
                    .truncate()
                    .child(video.name.clone()),
            )
            .into_any_element()
    }
}

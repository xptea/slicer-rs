//! Native video drawable layout. Video frames never enter the GPUI image cache.
use super::*;

impl SlicerApp {
    pub(super) fn preview_panel(&self, cx: &mut Context<Self>) -> AnyElement {
        if self.uses_composition_preview() {
            return self.composition_preview_panel();
        }
        let entity = cx.entity().downgrade();
        let mut panel = div()
            .size_full()
            .relative()
            .rounded(px(14.))
            .overflow_hidden()
            .bg(ink(0x0d0d0dff))
            .child(
                canvas(
                    move |bounds, window, cx| {
                        let _ = entity.update(cx, |this, cx| {
                            this.update_native_surface(bounds, window, cx)
                        });
                    },
                    |_, _, _, _| {},
                )
                .size_full(),
            )
            .when(!self.native.ready && !self.crop.open, |view| {
                view.child(
                    div()
                        .absolute()
                        .inset_0()
                        .flex()
                        .items_center()
                        .justify_center()
                        .text_sm()
                        .text_color(ink(MUTED))
                        .child(if self.inspecting {
                            "Opening video…"
                        } else if self.preview_error.is_some() {
                            "Video unavailable"
                        } else {
                            "Preparing video…"
                        }),
                )
            })
            .when_some(
                self.preview_error.clone().or(self.media_error.clone()),
                |view, error| {
                    view.child(
                        div()
                            .absolute()
                            .bottom_0()
                            .p_3()
                            .text_sm()
                            .text_color(ink(BAD))
                            .child(error),
                    )
                },
            );
        if self.crop.has_frame() {
            // The native child surface is hidden by the root render loop while
            // this GPUI frame is active, so the handles remain interactive.
            panel = panel.child(self.crop_inline(cx));
        }
        panel.into_any_element()
    }

    pub(super) fn composition_preview_panel(&self) -> AnyElement {
        let image = self.composition_preview_image.clone();
        let mut panel = div()
            .size_full()
            .relative()
            .rounded(px(14.))
            .overflow_hidden()
            .bg(ink(0x0d0d0dff));
        if let Some(image) = image {
            panel = panel.child(
                img(image)
                    .absolute()
                    .inset_0()
                    .size_full()
                    .object_fit(ObjectFit::Contain),
            );
        }
        if self.composition_preview_image.is_none() {
            panel = panel.child(
                div()
                    .absolute()
                    .inset_0()
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_sm()
                    .text_color(ink(if self.composition_preview_error.is_some() {
                        BAD
                    } else {
                        MUTED
                    }))
                    .child(if self.composition_preview_error.is_some() {
                        "Layered preview unavailable"
                    } else if self.composition_preview_loading {
                        "Rendering layered preview…"
                    } else {
                        "Preparing layered preview…"
                    }),
            );
        }
        panel
            .when(self.composition_preview_loading, |view| {
                view.child(
                    div()
                        .absolute()
                        .top_0()
                        .left_0()
                        .right_0()
                        .px_3()
                        .py_2()
                        .text_sm()
                        .text_color(ink(WARN))
                        .bg(rgba(0x090909cc))
                        .child("Rendering layered preview…"),
                )
            })
            .when_some(self.composition_preview_error.clone(), |view, error| {
                view.child(
                    div()
                        .absolute()
                        .bottom_0()
                        .left_0()
                        .right_0()
                        .px_3()
                        .py_2()
                        .text_sm()
                        .text_color(ink(BAD))
                        .bg(rgba(0x090909cc))
                        .child(error),
                )
            })
            .into_any_element()
    }
}

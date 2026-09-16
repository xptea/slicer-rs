//! Native video drawable layout. Video frames never enter the GPUI image cache.
use super::*;

impl SlicerApp {
    pub(super) fn preview_panel(&self, cx: &mut Context<Self>) -> AnyElement {
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
}

//! Editor screen composition and its empty-state file picker.

use super::*;

impl SlicerApp {
    pub(super) fn editor_view(&self, cx: &mut Context<Self>) -> AnyElement {
        v_flex()
            .size_full()
            .px(px(CONTENT_GUTTER))
            .pb_4()
            .gap_3()
            .child(
                div()
                    .flex_1()
                    .min_h(px(120.))
                    .child(if self.editor_path.is_some() {
                        self.preview_panel(cx)
                    } else {
                        self.drop_zone(cx)
                    }),
            )
            .child(self.transport(cx))
            .child(self.timeline(cx))
            .into_any_element()
    }

    pub(super) fn drop_zone(&self, cx: &mut Context<Self>) -> AnyElement {
        let browse = Button::new("drop-browse")
            .primary()
            .label("Choose a video")
            .on_click(cx.listener(|this, _, _, _| this.launch_dialog(DialogKind::Open)));
        v_flex()
            .size_full()
            .min_h(px(360.))
            .items_center()
            .justify_center()
            .gap_3()
            .rounded(px(18.))
            .border_1()
            .border_color(ink(ACCENT_STRONG))
            .bg(ink(SURFACE))
            .on_drop::<ExternalPaths>(cx.listener(|this, paths: &ExternalPaths, window, cx| {
                if let Some(path) = paths.paths().first().cloned() {
                    this.open_file(path, window, cx);
                }
            }))
            .child(div().text_color(ink(ACCENT)).text_size(px(42.)).child("＋"))
            .child(
                div()
                    .text_color(ink(TEXT))
                    .font_semibold()
                    .text_lg()
                    .child("Drop a video here"),
            )
            .child(
                div()
                    .text_color(ink(MUTED))
                    .child("or browse for an MP4, MKV, MOV, WebM, or AVI"),
            )
            .child(browse)
            .into_any_element()
    }
}

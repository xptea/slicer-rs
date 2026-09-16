//! Export completion feedback stays above the native video surface.
use super::*;
pub(super) struct ExportToast {
    pub message: String,
    pub until: Instant,
}
impl SlicerApp {
    pub(super) fn poll_export_toast(&mut self) {
        if let Some(result) = self.clipboard_rx.as_ref().and_then(|rx| rx.try_recv().ok()) {
            self.clipboard_rx = None;
            if let Some(toast) = &mut self.toast {
                toast.message = match result {
                    Ok(()) => "Exported · Copied to clipboard".into(),
                    Err(_) => "Exported · Could not copy to clipboard".into(),
                };
            }
        }
        if self
            .toast
            .as_ref()
            .is_some_and(|t| Instant::now() >= t.until)
        {
            self.toast = None;
        }
    }
    pub(super) fn export_toast(&self, cx: &mut Context<Self>) -> AnyElement {
        let message = self
            .toast
            .as_ref()
            .map(|t| t.message.clone())
            .unwrap_or_default();
        // The header occupies 56px above the native drawable, so the toast
        // never gets covered by the renderer's child window.
        div()
            .absolute()
            .top(px(4.))
            .left_0()
            .right_0()
            .flex()
            .justify_center()
            .child(
                h_flex()
                    .gap_3()
                    .px_3()
                    .py_2()
                    .rounded(px(12.))
                    .bg(ink(SURFACE))
                    .border_1()
                    .border_color(ink(BORDER))
                    .occlude()
                    .child(div().text_sm().child(message))
                    .child(
                        Button::new("toast-open-folder")
                            .ghost()
                            .compact()
                            .label("Open folder")
                            .on_click(cx.listener(|this, _, _, _| this.open_folder())),
                    ),
            )
            .into_any_element()
    }
}

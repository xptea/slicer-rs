//! Window-level external file drops, including scaled X11 desktops.
use super::*;
impl SlicerApp {
    pub(super) fn file_drop_listener(&self, cx: &mut Context<Self>) -> AnyElement {
        let owner = cx.entity().downgrade();
        canvas(
            |_, _, _| {},
            move |_, _, window, _| {
                let owner = owner.clone();
                // The OS already targets this window. Consume external drops at
                // capture phase so previews, controls, and overlays all open files
                // consistently, including drops near the window edges.
                window.on_mouse_event(move |event: &MouseUpEvent, phase, window, cx| {
                    if phase != DispatchPhase::Capture
                        || event.button != MouseButton::Left
                        || !cx.has_active_drag()
                    {
                        return;
                    }
                    let _ = owner.update(cx, |this, cx| {
                        if let Some(paths) = this.external_drop.take() {
                            cx.stop_active_drag(window);
                            cx.stop_propagation();
                            if this.screen == Screen::Studio {
                                this.studio_import(paths.paths().to_vec());
                            } else if let Some(path) = paths.paths().first().cloned() {
                                this.open_file(path, window, cx);
                            }
                            cx.notify();
                        }
                    });
                });
            },
        )
        .absolute()
        .size_full()
        .into_any_element()
    }
}

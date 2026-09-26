//! Rounded transparent window surface, with Linux client-side decorations.

use super::*;

pub(super) fn frame(
    content: impl IntoElement,
    mut tools: Option<AnyElement>,
    window: &mut Window,
    cx: &mut Context<SlicerApp>,
) -> AnyElement {
    let owner = cx.entity().downgrade();
    let studio = tools.is_some();
    let client = matches!(window.window_decorations(), Decorations::Client { .. });
    let maximized = window.is_maximized() || window.is_fullscreen();
    let radius = if maximized { px(0.) } else { px(WINDOW_RADIUS) };
    let inset = if client && !maximized { px(6.) } else { px(0.) };
    // TitleBar lays out its custom children in a flex region that ends before
    // the platform control buttons. Position the title over the full bar and
    // extend it by the known left padding and control width so its center is
    // the window center, regardless of which controls the WM exposes.
    let titlebar_left_padding = if cfg!(target_os = "macos") {
        80.0
    } else {
        12.0
    };
    let controls = window.window_controls();
    let control_count =
        1 + if controls.minimize { 1 } else { 0 } + if controls.maximize { 1 } else { 0 };
    let titlebar_controls_width = if cfg!(target_os = "macos") || cfg!(target_family = "wasm") {
        0.0
    } else if client {
        f32::from(TITLE_BAR_HEIGHT) * control_count as f32
    } else {
        0.0
    };
    if client {
        window.set_client_inset(px(6.));
    }
    // Only this surface paints the background. A square ancestor background
    // would fill the transparent corners even if this element is rounded.
    let surface = v_flex()
        .size_full()
        .rounded(radius)
        .bg(ink(BG))
        .overflow_hidden()
        .when(client, |surface| {
            surface.child(
                TitleBar::new()
                    .on_close_window(move |_, window, cx| {
                        let _ = owner.update(cx, |this, _| {
                            this.studio = None;
                            this.native.shutdown()
                        });
                        window.remove_window();
                    })
                    .bg(ink(if studio { BG } else { SURFACE }))
                    .when(studio, |bar| bar.border_b_0())
                    .rounded_t(radius)
                    .child(
                        div()
                            .absolute()
                            .left(px(-titlebar_left_padding))
                            .right(px(-titlebar_controls_width))
                            .top_0()
                            .bottom_0()
                            .flex()
                            .items_center()
                            .justify_center()
                            .text_sm()
                            .child("Slicer"),
                    )
                    .when_some(tools.take(), |bar, tools| bar.child(tools)),
            )
        })
        .when_some(tools, |surface, tools| {
            surface.child(div().h(px(32.)).px_2().child(tools))
        })
        .child(
            div()
                .flex_1()
                .min_h(px(0.))
                .overflow_hidden()
                .child(content),
        );
    div()
        .id("rounded-window-frame")
        .relative()
        .size_full()
        .p(inset)
        .child(surface)
        .when(client && !maximized, |frame| frame.child(resize_regions()))
        .into_any_element()
}

/// Explicit topmost hit regions keep controls and modal overlays from stealing
/// resize gestures. Corner squares are wider than the edge strips.
fn resize_regions() -> AnyElement {
    const EDGE: f32 = 10.;
    const CORNER: f32 = 24.;
    let edges = [
        (
            "resize-left",
            ResizeEdge::Left,
            CursorStyle::ResizeLeftRight,
        ),
        (
            "resize-right",
            ResizeEdge::Right,
            CursorStyle::ResizeLeftRight,
        ),
        ("resize-top", ResizeEdge::Top, CursorStyle::ResizeUpDown),
        (
            "resize-bottom",
            ResizeEdge::Bottom,
            CursorStyle::ResizeUpDown,
        ),
        (
            "resize-top-left",
            ResizeEdge::TopLeft,
            CursorStyle::ResizeUpLeftDownRight,
        ),
        (
            "resize-top-right",
            ResizeEdge::TopRight,
            CursorStyle::ResizeUpRightDownLeft,
        ),
        (
            "resize-bottom-left",
            ResizeEdge::BottomLeft,
            CursorStyle::ResizeUpRightDownLeft,
        ),
        (
            "resize-bottom-right",
            ResizeEdge::BottomRight,
            CursorStyle::ResizeUpLeftDownRight,
        ),
    ];
    let mut regions = div().absolute().inset_0();
    for (id, edge, cursor) in edges {
        let region = div().absolute().id(id).cursor(cursor).occlude();
        let region = match edge {
            ResizeEdge::Left => region
                .left_0()
                .top(px(CORNER))
                .bottom(px(CORNER))
                .w(px(EDGE)),
            ResizeEdge::Right => region
                .right_0()
                .top(px(CORNER))
                .bottom(px(CORNER))
                .w(px(EDGE)),
            ResizeEdge::Top => region
                .top_0()
                .left(px(CORNER))
                .right(px(CORNER))
                .h(px(EDGE)),
            ResizeEdge::Bottom => region
                .bottom_0()
                .left(px(CORNER))
                .right(px(CORNER))
                .h(px(EDGE)),
            ResizeEdge::TopLeft => region.top_0().left_0().size(px(CORNER)),
            ResizeEdge::TopRight => region.top_0().right_0().size(px(CORNER)),
            ResizeEdge::BottomLeft => region.bottom_0().left_0().size(px(CORNER)),
            ResizeEdge::BottomRight => region.bottom_0().right_0().size(px(CORNER)),
        };
        regions = regions.child(
            region.on_mouse_down(MouseButton::Left, move |_, window, cx| {
                window.start_window_resize(edge);
                cx.stop_propagation();
            }),
        );
    }
    regions.into_any_element()
}

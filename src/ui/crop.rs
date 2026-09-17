//! Inline crop selection for the editor preview.
//!
//! The native player is hidden while the crop editor is open so the paused
//! frame can be drawn in GPUI with an interactive selection on top. Applying
//! the selection sends the source-pixel rectangle back to the native preview
//! and the export worker.
use super::*;

const MIN_SELECTION_SIZE: f32 = 0.02;
const FRAME_INSET: f32 = 14.;
const HANDLE_SIZE: f32 = 14.;
const HANDLE_RADIUS: f32 = HANDLE_SIZE / 2.;
const HANDLE_HIT_RADIUS: f32 = 12.;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CropHandle {
    TopLeft,
    Top,
    TopRight,
    Right,
    BottomRight,
    Bottom,
    BottomLeft,
    Left,
}

impl CropHandle {
    const ALL: [Self; 8] = [
        Self::TopLeft,
        Self::Top,
        Self::TopRight,
        Self::Right,
        Self::BottomRight,
        Self::Bottom,
        Self::BottomLeft,
        Self::Left,
    ];

    fn position(self, selection: [f32; 4]) -> (f32, f32) {
        let [left, top, right, bottom] = selection;
        let center_x = (left + right) / 2.;
        let center_y = (top + bottom) / 2.;
        match self {
            Self::TopLeft => (left, top),
            Self::Top => (center_x, top),
            Self::TopRight => (right, top),
            Self::Right => (right, center_y),
            Self::BottomRight => (right, bottom),
            Self::Bottom => (center_x, bottom),
            Self::BottomLeft => (left, bottom),
            Self::Left => (left, center_y),
        }
    }

    fn id(self) -> &'static str {
        match self {
            Self::TopLeft => "top-left",
            Self::Top => "top",
            Self::TopRight => "top-right",
            Self::Right => "right",
            Self::BottomRight => "bottom-right",
            Self::Bottom => "bottom",
            Self::BottomLeft => "bottom-left",
            Self::Left => "left",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum CropDrag {
    New {
        start: (f32, f32),
        aspect: f32,
    },
    Move {
        start: (f32, f32),
        selection: [f32; 4],
    },
    Resize {
        handle: CropHandle,
        start: (f32, f32),
        selection: [f32; 4],
    },
}

#[derive(Default)]
pub(super) struct CropEditor {
    pub open: bool,
    pub applied: Option<job::CropRect>,
    selection: [f32; 4],
    drag: Option<CropDrag>,
    bounds: Option<Bounds<Pixels>>,
    frame_space: Option<gpui_kit::gpui::Size<Pixels>>,
    image: Option<Arc<RenderImage>>,
    image_seconds: Option<f64>,
    requested_seconds: f64,
    frame_presented: bool,
    decode_rx: Option<mpsc::Receiver<Result<Arc<RenderImage>, String>>>,
    worker: Option<preview::PreviewWorker>,
    error: Option<String>,
}

impl CropEditor {
    pub(super) fn is_ready(&self) -> bool {
        self.open && self.image.is_some() && self.frame_presented
    }
    pub(super) fn has_frame(&self) -> bool {
        self.open && self.image.is_some()
    }
    pub(super) fn error(&self) -> Option<&str> {
        self.open.then_some(self.error.as_deref()).flatten()
    }
}

impl SlicerApp {
    fn video_dimensions(&self) -> Option<(u32, u32)> {
        self.media.as_ref()?.streams.iter().find_map(|s| {
            if s.kind == "video" {
                Some((s.width?, s.height?))
            } else {
                None
            }
        })
    }

    pub(super) fn open_crop(&mut self, cx: &mut Context<Self>) {
        let Some((width, height)) = self.video_dimensions() else {
            return;
        };
        self.native.pause();
        self.crop.open = true;
        self.crop.drag = None;
        self.crop.bounds = None;
        self.crop.error = None;
        let seconds = self.media.as_ref().map_or(self.preview_seconds, |media| {
            self.preview_seconds.min((media.duration - 0.05).max(0.))
        });
        let cached = self.crop.image.is_some() && self.crop.image_seconds == Some(seconds);
        if !cached {
            self.crop.image = None;
        }
        self.crop.frame_presented = false;
        self.crop.requested_seconds = seconds;
        self.crop.decode_rx = None;
        self.crop.worker = None;
        self.crop.selection = self
            .crop
            .applied
            .map(|r| {
                [
                    r.x as f32 / width as f32,
                    r.y as f32 / height as f32,
                    (r.x + r.width) as f32 / width as f32,
                    (r.y + r.height) as f32 / height as f32,
                ]
            })
            .unwrap_or([0., 0., 1., 1.]);
        if !cached
            && let (Some(binaries), Some(path)) = (self.binaries.clone(), self.editor_path.clone())
        {
            let worker = preview::PreviewWorker::new(binaries);
            worker.request(path, seconds);
            self.crop.worker = Some(worker);
        }
        cx.notify();
    }

    pub(super) fn poll_crop(&mut self) {
        if let Some(event) = self
            .crop
            .worker
            .as_ref()
            .and_then(|w| w.events.try_recv().ok())
        {
            self.crop.worker = None;
            match event.result {
                Ok(bytes) => {
                    let (tx, rx) = mpsc::channel();
                    self.crop.decode_rx = Some(rx);
                    thread::spawn(move || {
                        let result = image::load_from_memory(&bytes)
                            .map(|image| {
                                let mut pixels = image.into_rgba8();
                                for pixel in pixels.pixels_mut() {
                                    pixel.0.swap(0, 2); // GPUI RenderImage stores BGRA.
                                }
                                Arc::new(RenderImage::new([image::Frame::new(pixels)]))
                            })
                            .map_err(|error| error.to_string());
                        let _ = tx.send(result);
                    });
                }
                Err(error) => self.crop.error = Some(error),
            }
        }
        if let Some(result) = self
            .crop
            .decode_rx
            .as_ref()
            .and_then(|rx| rx.try_recv().ok())
        {
            self.crop.decode_rx = None;
            match result {
                Ok(image) => {
                    self.crop.image = Some(image);
                    self.crop.image_seconds = Some(self.crop.requested_seconds);
                }
                Err(error) => self.crop.error = Some(error),
            }
        }
    }

    fn crop_point(&self, point: Point<Pixels>) -> Option<(f32, f32)> {
        if !self.crop.is_ready() {
            return None;
        }
        let bounds = self.crop.bounds?;
        if bounds.size.width <= px(0.) || bounds.size.height <= px(0.) {
            return None;
        }
        Some((
            (f32::from(point.x - bounds.origin.x) / f32::from(bounds.size.width)).clamp(0., 1.),
            (f32::from(point.y - bounds.origin.y) / f32::from(bounds.size.height)).clamp(0., 1.),
        ))
    }

    fn crop_handle_at(&self, point: Point<Pixels>) -> Option<CropHandle> {
        let normalized = self.crop_point(point)?;
        let bounds = self.crop.bounds?;
        let hit_x = HANDLE_HIT_RADIUS / f32::from(bounds.size.width).max(1.);
        let hit_y = HANDLE_HIT_RADIUS / f32::from(bounds.size.height).max(1.);
        let selection = self.crop.selection;
        CropHandle::ALL.into_iter().find(|handle| {
            let (x, y) = handle.position(selection);
            (normalized.0 - x).abs() <= hit_x && (normalized.1 - y).abs() <= hit_y
        })
    }

    fn selection_contains(&self, point: (f32, f32)) -> bool {
        let [left, top, right, bottom] = self.crop.selection;
        point.0 >= left && point.0 <= right && point.1 >= top && point.1 <= bottom
    }

    fn begin_crop_drag(&mut self, point: Point<Pixels>, handle: Option<CropHandle>) {
        let handle = handle.or_else(|| self.crop_handle_at(point));
        let Some(point) = self.crop_point(point) else {
            return;
        };
        self.crop.drag = Some(match handle {
            Some(handle) => CropDrag::Resize {
                handle,
                start: point,
                selection: self.crop.selection,
            },
            None if self.selection_contains(point) => CropDrag::Move {
                start: point,
                selection: self.crop.selection,
            },
            None => CropDrag::New {
                start: point,
                aspect: selection_aspect(self.crop.selection),
            },
        });
    }

    pub(super) fn drag_crop(&mut self, point: Point<Pixels>, shift: bool) {
        let Some(point) = self.crop_point(point) else {
            return;
        };
        let Some(drag) = self.crop.drag else {
            return;
        };
        self.crop.selection = match drag {
            CropDrag::New { start, aspect } => {
                if shift {
                    selection_from_points_with_aspect(start, point, aspect)
                } else {
                    selection_from_points(start, point)
                }
            }
            CropDrag::Move { start, selection } => {
                move_selection(selection, point.0 - start.0, point.1 - start.1)
            }
            CropDrag::Resize {
                handle,
                start,
                selection,
            } => resize_from_drag(
                selection,
                handle,
                start,
                point,
                shift.then(|| selection_aspect(selection)),
            ),
        };
    }

    pub(super) fn finish_crop_drag(&mut self) {
        self.crop.drag = None;
    }

    /// Move the draft crop by source pixels for fine keyboard adjustments.
    pub(super) fn nudge_crop_pixels(&mut self, x: i32, y: i32) {
        let Some((width, height)) = self.video_dimensions() else {
            return;
        };
        self.crop.selection = move_selection(
            self.crop.selection,
            x as f32 / width as f32,
            y as f32 / height as f32,
        );
    }

    fn selected_crop(&self) -> Option<job::CropRect> {
        let (width, height) = self.video_dimensions()?;
        crop_pixels(self.crop.selection, width, height)
    }

    fn crop_aspect(&mut self, ratio: f32) {
        let Some((w, h)) = self.video_dimensions() else {
            return;
        };
        if !ratio.is_finite() || ratio <= 0. {
            return;
        }
        let source_ratio = w as f32 / h as f32;
        let (cw, ch) = if source_ratio > ratio {
            (ratio / source_ratio, 1.)
        } else {
            (1., source_ratio / ratio)
        };
        self.crop.selection = [
            (1. - cw) / 2.,
            (1. - ch) / 2.,
            (1. + cw) / 2.,
            (1. + ch) / 2.,
        ];
    }

    fn crop_handle(&self, handle: CropHandle, cx: &mut Context<Self>) -> AnyElement {
        let (x, y) = handle.position(self.crop.selection);
        div()
            .absolute()
            .left(relative(x))
            .top(relative(y))
            .size(px(HANDLE_SIZE))
            .ml(px(-HANDLE_RADIUS))
            .mt(px(-HANDLE_RADIUS))
            .rounded_full()
            .bg(ink(TEXT))
            .border_2()
            .border_color(ink(SURFACE))
            .id(format!("crop-handle-{}", handle.id()))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                    this.begin_crop_drag(event.position, Some(handle));
                    cx.stop_propagation();
                    cx.notify();
                }),
            )
            .into_any_element()
    }

    /// The crop editor sits inside the preview and owns its pointer events.
    pub(super) fn crop_inline(&self, cx: &mut Context<Self>) -> AnyElement {
        let entity = cx.entity().downgrade();
        let [left, top, right, bottom] = self.crop.selection;
        let source_ratio = self
            .video_dimensions()
            .map(|(width, height)| width as f32 / height as f32)
            .filter(|ratio| ratio.is_finite() && *ratio > 0.)
            .unwrap_or(16. / 9.);

        let space = self.crop.frame_space.unwrap_or(size(px(320.), px(180.)));
        let available_w = (f32::from(space.width) - FRAME_INSET * 2.).max(1.);
        let available_h = (f32::from(space.height) - FRAME_INSET * 2.).max(1.);
        let picture_w = available_w.min(available_h * source_ratio);
        let picture_h = picture_w / source_ratio;
        let layout_entity = cx.entity().downgrade();
        let painted_entity = cx.entity().downgrade();
        let mut picture = div()
            .relative()
            .w(px(picture_w))
            .h(px(picture_h))
            .flex_shrink_0()
            .child(
                div()
                    .absolute()
                    .inset_0()
                    .rounded(px(10.))
                    .overflow_hidden()
                    .bg(ink(BG))
                    .when_some(self.crop.image.clone(), |view, image| {
                        view.child(img(image).size_full().object_fit(ObjectFit::Fill))
                    }),
            )
            .child(
                canvas(
                    move |bounds, _, cx| {
                        let _ = entity.update(cx, |this, _| this.crop.bounds = Some(bounds));
                    },
                    |_, _, _, _| {},
                )
                .absolute()
                .inset_0(),
            )
            .child(
                div()
                    .absolute()
                    .top_0()
                    .left_0()
                    .right_0()
                    .h(relative(top))
                    .bg(rgba(0x00000072)),
            )
            .child(
                div()
                    .absolute()
                    .bottom_0()
                    .left_0()
                    .right_0()
                    .h(relative(1. - bottom))
                    .bg(rgba(0x00000072)),
            )
            .child(
                div()
                    .absolute()
                    .left_0()
                    .top(relative(top))
                    .w(relative(left))
                    .h(relative(bottom - top))
                    .bg(rgba(0x00000072)),
            )
            .child(
                div()
                    .absolute()
                    .right_0()
                    .top(relative(top))
                    .w(relative(1. - right))
                    .h(relative(bottom - top))
                    .bg(rgba(0x00000072)),
            )
            .child(
                div()
                    .absolute()
                    .left(relative(left))
                    .top(relative(top))
                    .w(relative(right - left))
                    .h(relative(bottom - top))
                    .border_2()
                    .border_color(ink(TEXT))
                    .bg(rgba(0xffffff18))
                    .id("crop-selection")
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, event: &MouseDownEvent, _, cx| {
                            this.begin_crop_drag(event.position, None);
                            cx.stop_propagation();
                            cx.notify();
                        }),
                    ),
            );
        for handle in CropHandle::ALL {
            picture = picture.child(self.crop_handle(handle, cx));
        }
        let picture = picture
            .id("crop-picture")
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event: &MouseDownEvent, _, cx| {
                    this.begin_crop_drag(event.position, None);
                    cx.stop_propagation();
                    cx.notify();
                }),
            )
            .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _, cx| {
                if event.pressed_button == Some(MouseButton::Left) {
                    this.drag_crop(event.position, event.modifiers.shift);
                    cx.notify();
                }
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    this.finish_crop_drag();
                    cx.stop_propagation();
                    cx.notify();
                }),
            );

        div()
            .absolute()
            .inset_0()
            .v_flex()
            .bg(ink(BG))
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_h(px(0.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .p(px(FRAME_INSET))
                    .child(
                        canvas(
                            move |bounds, _, cx| {
                                let _ = layout_entity.update(cx, |this, cx| {
                                    if this.crop.frame_space != Some(bounds.size) {
                                        this.crop.frame_space = Some(bounds.size);
                                        cx.notify();
                                    }
                                });
                            },
                            |_, _, _, _| {},
                        )
                        .absolute()
                        .inset_0(),
                    )
                    .child(picture),
            )
            .child(
                canvas(
                    |_, _, _| {},
                    move |_, _, _, cx| {
                        let _ = painted_entity.update(cx, |this, cx| {
                            if this.crop.image.is_some() && !this.crop.frame_presented {
                                this.crop.frame_presented = true;
                                cx.notify();
                            }
                        });
                    },
                )
                .absolute()
                .inset_0(),
            )
            .into_any_element()
    }
    pub(super) fn crop_tools(&self, cx: &mut Context<Self>) -> AnyElement {
        let selection = self.selected_crop();
        let mut presets = h_flex().gap_2();
        presets = presets
            .child(
                Button::new("crop-full")
                    .secondary()
                    .label("Original")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.crop.selection = [0., 0., 1., 1.];
                        cx.notify();
                    })),
            )
            .child(
                Button::new("crop-square")
                    .secondary()
                    .label("1:1")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.crop_aspect(1.);
                        cx.notify();
                    })),
            )
            .child(
                Button::new("crop-wide")
                    .secondary()
                    .label("16:9")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.crop_aspect(16. / 9.);
                        cx.notify();
                    })),
            )
            .child(
                Button::new("crop-portrait")
                    .secondary()
                    .label("9:16")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.crop_aspect(9. / 16.);
                        cx.notify();
                    })),
            );

        h_flex()
            .flex_shrink_0()
            .gap_2()
            .items_center()
            .child(presets)
            .child(
                Button::new("cancel-crop")
                    .ghost()
                    .icon(gpui_kit::assets::IconName::X)
                    .accessibility_label("Cancel crop")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.crop.open = false;
                        this.finish_crop_drag();
                        cx.notify();
                    })),
            )
            .child(
                Button::new("apply-crop")
                    .primary()
                    .icon(gpui_kit::assets::IconName::Check)
                    .accessibility_label("Apply crop")
                    .disabled(selection.is_none() || self.crop.image.is_none())
                    .on_click(cx.listener(|this, _, _, cx| {
                        let full = this
                            .video_dimensions()
                            .zip(this.selected_crop())
                            .is_some_and(|((width, height), crop)| {
                                crop.x == 0
                                    && crop.y == 0
                                    && crop.width == width
                                    && crop.height == height
                            });
                        this.crop.applied = if full { None } else { this.selected_crop() };
                        this.sync_project_crop();
                        this.native.set_crop(this.crop.applied);
                        this.crop.open = false;
                        this.finish_crop_drag();
                        this.crop.bounds = None;
                        cx.notify();
                    })),
            )
            .into_any_element()
    }
}

fn normalize_selection(selection: [f32; 4]) -> [f32; 4] {
    let [x0, y0, x1, y1] = selection;
    let mut left = x0.min(x1).clamp(0., 1.);
    let mut top = y0.min(y1).clamp(0., 1.);
    let mut right = x0.max(x1).clamp(0., 1.);
    let mut bottom = y0.max(y1).clamp(0., 1.);
    if right - left < MIN_SELECTION_SIZE {
        let center = (left + right) / 2.;
        left = (center - MIN_SELECTION_SIZE / 2.).clamp(0., 1. - MIN_SELECTION_SIZE);
        right = left + MIN_SELECTION_SIZE;
    }
    if bottom - top < MIN_SELECTION_SIZE {
        let center = (top + bottom) / 2.;
        top = (center - MIN_SELECTION_SIZE / 2.).clamp(0., 1. - MIN_SELECTION_SIZE);
        bottom = top + MIN_SELECTION_SIZE;
    }
    [left, top, right, bottom]
}

fn selection_from_points(start: (f32, f32), end: (f32, f32)) -> [f32; 4] {
    normalize_selection([start.0, start.1, end.0, end.1])
}

fn selection_aspect(selection: [f32; 4]) -> f32 {
    let [left, top, right, bottom] = normalize_selection(selection);
    ((right - left) / (bottom - top)).max(MIN_SELECTION_SIZE)
}

fn fit_aspect_size(
    raw_width: f32,
    raw_height: f32,
    aspect: f32,
    max_width: f32,
    max_height: f32,
) -> (f32, f32) {
    let aspect = if aspect.is_finite() && aspect > 0. {
        aspect
    } else {
        1.
    };
    let raw_width = raw_width.max(MIN_SELECTION_SIZE);
    let raw_height = raw_height.max(MIN_SELECTION_SIZE);
    let (mut width, mut height) = if raw_width / raw_height > aspect {
        (raw_height * aspect, raw_height)
    } else {
        (raw_width, raw_width / aspect)
    };
    let scale = (1_f32)
        .min(max_width.max(0.) / width.max(f32::EPSILON))
        .min(max_height.max(0.) / height.max(f32::EPSILON));
    width *= scale;
    height *= scale;
    (width, height)
}

fn selection_from_points_with_aspect(start: (f32, f32), end: (f32, f32), aspect: f32) -> [f32; 4] {
    let start = (start.0.clamp(0., 1.), start.1.clamp(0., 1.));
    let end = (end.0.clamp(0., 1.), end.1.clamp(0., 1.));
    let sign_x = if end.0 >= start.0 { 1. } else { -1. };
    let sign_y = if end.1 >= start.1 { 1. } else { -1. };
    let max_width = if sign_x > 0. { 1. - start.0 } else { start.0 };
    let max_height = if sign_y > 0. { 1. - start.1 } else { start.1 };
    let (width, height) = fit_aspect_size(
        (end.0 - start.0).abs(),
        (end.1 - start.1).abs(),
        aspect,
        max_width,
        max_height,
    );
    normalize_selection([
        start.0,
        start.1,
        start.0 + sign_x * width,
        start.1 + sign_y * height,
    ])
}

fn move_selection(selection: [f32; 4], dx: f32, dy: f32) -> [f32; 4] {
    let [left, top, right, bottom] = normalize_selection(selection);
    let width = right - left;
    let height = bottom - top;
    let left = (left + dx).clamp(0., 1. - width);
    let top = (top + dy).clamp(0., 1. - height);
    [left, top, left + width, top + height]
}

fn resize_from_drag(
    selection: [f32; 4],
    handle: CropHandle,
    start: (f32, f32),
    point: (f32, f32),
    locked_aspect: Option<f32>,
) -> [f32; 4] {
    let (edge_x, edge_y) = handle.position(selection);
    let point = (edge_x + point.0 - start.0, edge_y + point.1 - start.1);
    locked_aspect.map_or_else(
        || resize_selection(selection, handle, point),
        |aspect| resize_selection_with_aspect(selection, handle, point, aspect),
    )
}

fn resize_selection_with_aspect(
    selection: [f32; 4],
    handle: CropHandle,
    point: (f32, f32),
    aspect: f32,
) -> [f32; 4] {
    let [left, top, right, bottom] = normalize_selection(selection);
    let aspect = if aspect.is_finite() && aspect > 0. {
        aspect
    } else {
        selection_aspect(selection)
    };
    let center_x = (left + right) / 2.;
    let center_y = (top + bottom) / 2.;
    match handle {
        CropHandle::TopLeft
        | CropHandle::TopRight
        | CropHandle::BottomRight
        | CropHandle::BottomLeft => {
            let (fixed_x, fixed_y, sign_x, sign_y) = match handle {
                CropHandle::TopLeft => (right, bottom, -1., -1.),
                CropHandle::TopRight => (left, bottom, 1., -1.),
                CropHandle::BottomRight => (left, top, 1., 1.),
                CropHandle::BottomLeft => (right, top, -1., 1.),
                _ => unreachable!(),
            };
            let (width, height) = fit_aspect_size(
                (point.0 - fixed_x).abs(),
                (point.1 - fixed_y).abs(),
                aspect,
                if sign_x > 0. { 1. - fixed_x } else { fixed_x },
                if sign_y > 0. { 1. - fixed_y } else { fixed_y },
            );
            normalize_selection([
                fixed_x,
                fixed_y,
                fixed_x + sign_x * width,
                fixed_y + sign_y * height,
            ])
        }
        CropHandle::Top | CropHandle::Bottom => {
            let sign_y = if matches!(handle, CropHandle::Top) {
                -1.
            } else {
                1.
            };
            let fixed_y = if sign_y < 0. { bottom } else { top };
            let height = (point.1 - fixed_y).abs().max(MIN_SELECTION_SIZE);
            let width = height * aspect;
            let max_height = if sign_y > 0. { 1. - fixed_y } else { fixed_y };
            let scale = (1_f32)
                .min((center_x * 2.).min((1. - center_x) * 2.) / width.max(f32::EPSILON))
                .min(max_height / height.max(f32::EPSILON));
            let width = width * scale;
            let height = height * scale;
            normalize_selection([
                center_x - width / 2.,
                fixed_y + sign_y * height,
                center_x + width / 2.,
                fixed_y,
            ])
        }
        CropHandle::Left | CropHandle::Right => {
            let sign_x = if matches!(handle, CropHandle::Left) {
                -1.
            } else {
                1.
            };
            let fixed_x = if sign_x < 0. { right } else { left };
            let width = (point.0 - fixed_x).abs().max(MIN_SELECTION_SIZE);
            let height = width / aspect.max(f32::EPSILON);
            let max_width = if sign_x > 0. { 1. - fixed_x } else { fixed_x };
            let scale = (1_f32)
                .min(max_width / width.max(f32::EPSILON))
                .min((center_y * 2.).min((1. - center_y) * 2.) / height.max(f32::EPSILON));
            let width = width * scale;
            let height = height * scale;
            normalize_selection([
                fixed_x + sign_x * width,
                center_y - height / 2.,
                fixed_x,
                center_y + height / 2.,
            ])
        }
    }
}

fn resize_selection(selection: [f32; 4], handle: CropHandle, point: (f32, f32)) -> [f32; 4] {
    let [mut left, mut top, mut right, mut bottom] = normalize_selection(selection);
    let (x, y) = (point.0.clamp(0., 1.), point.1.clamp(0., 1.));
    match handle {
        CropHandle::TopLeft => {
            left = x.min(right - MIN_SELECTION_SIZE);
            top = y.min(bottom - MIN_SELECTION_SIZE);
        }
        CropHandle::Top => top = y.min(bottom - MIN_SELECTION_SIZE),
        CropHandle::TopRight => {
            right = x.max(left + MIN_SELECTION_SIZE);
            top = y.min(bottom - MIN_SELECTION_SIZE);
        }
        CropHandle::Right => right = x.max(left + MIN_SELECTION_SIZE),
        CropHandle::BottomRight => {
            right = x.max(left + MIN_SELECTION_SIZE);
            bottom = y.max(top + MIN_SELECTION_SIZE);
        }
        CropHandle::Bottom => bottom = y.max(top + MIN_SELECTION_SIZE),
        CropHandle::BottomLeft => {
            left = x.min(right - MIN_SELECTION_SIZE);
            bottom = y.max(top + MIN_SELECTION_SIZE);
        }
        CropHandle::Left => left = x.min(right - MIN_SELECTION_SIZE),
    }
    normalize_selection([left, top, right, bottom])
}

fn crop_pixels(selection: [f32; 4], width: u32, height: u32) -> Option<job::CropRect> {
    if width < 2 || height < 2 || selection.iter().any(|v| !v.is_finite()) {
        return None;
    }
    let [x0, y0, x1, y1] = selection.map(|v| v.clamp(0., 1.));
    let (x0, x1) = (x0.min(x1), x0.max(x1));
    let (y0, y1) = (y0.min(y1), y0.max(y1));
    let x = (x0 * width as f32).floor() as u32 / 2 * 2;
    let y = (y0 * height as f32).floor() as u32 / 2 * 2;
    let right = (x1 * width as f32).floor() as u32 / 2 * 2;
    let bottom = (y1 * height as f32).floor() as u32 / 2 * 2;
    let width = right.checked_sub(x)?;
    let height = bottom.checked_sub(y)?;
    (width >= 2 && height >= 2).then_some(job::CropRect {
        x,
        y,
        width,
        height,
    })
}

#[cfg(test)]
mod tests {
    use super::{
        CropHandle, crop_pixels, move_selection, resize_from_drag, resize_selection,
        selection_aspect, selection_from_points, selection_from_points_with_aspect,
    };

    #[test]
    fn native_frame_stays_visible_until_crop_frame_was_painted() {
        let mut crop = super::CropEditor {
            open: true,
            ..Default::default()
        };
        assert!(!crop.is_ready());
        crop.image = Some(std::sync::Arc::new(super::RenderImage::new([
            image::Frame::new(image::RgbaImage::new(2, 2)),
        ])));
        assert!(crop.has_frame());
        assert!(!crop.is_ready());
        crop.frame_presented = true;
        assert!(crop.is_ready());
        crop.open = false;
        assert!(!crop.is_ready());
    }
    #[test]
    fn crop_selection_is_even_and_bounded() {
        let r = crop_pixels([0.1, 0.2, 0.9, 0.8], 1921, 1081).unwrap();
        assert_eq!((r.x, r.y, r.width, r.height), (192, 216, 1536, 648));
        assert!(crop_pixels([0.5, 0.5, 0.5, 0.5], 1920, 1080).is_none());
        assert!(crop_pixels([f32::NAN, 0., 1., 1.], 1920, 1080).is_none());
    }

    #[test]
    fn new_selection_is_normalized_with_a_usable_minimum() {
        let selection = selection_from_points((0.9, 0.8), (0.1, 0.2));
        assert_eq!(selection, [0.1, 0.2, 0.9, 0.8]);
        let tiny = selection_from_points((0.5, 0.5), (0.5, 0.5));
        assert!(tiny[2] - tiny[0] >= 0.02 - f32::EPSILON);
        assert!(tiny[3] - tiny[1] >= 0.02 - f32::EPSILON);
        assert!(tiny.iter().all(|value| (0. ..=1.).contains(value)));
    }

    #[test]
    fn moving_selection_stays_inside_the_frame() {
        let selection = move_selection([0.2, 0.3, 0.6, 0.7], 0.8, -0.8);
        for (actual, expected) in selection.into_iter().zip([0.6, 0., 1., 0.4]) {
            assert!((actual - expected).abs() < 1e-5);
        }
    }

    #[test]
    fn resizing_handles_respect_minimum_and_edges() {
        let selection = resize_selection([0.2, 0.2, 0.8, 0.8], CropHandle::TopLeft, (1., 1.));
        assert!(selection[2] - selection[0] >= 0.02 - f32::EPSILON);
        assert!(selection[3] - selection[1] >= 0.02 - f32::EPSILON);
        assert!(selection.iter().all(|value| (0. ..=1.).contains(value)));

        let selection = resize_selection([0.2, 0.2, 0.8, 0.8], CropHandle::BottomRight, (0., 0.));
        for (actual, expected) in selection.into_iter().zip([0.2, 0.2, 0.22, 0.22]) {
            assert!((actual - expected).abs() < 1e-5);
        }
    }

    #[test]
    fn resizing_preserves_the_pointer_offset_at_mouse_down() {
        let selection = resize_from_drag(
            [0.2, 0.2, 0.8, 0.8],
            CropHandle::Right,
            (0.78, 0.5),
            (0.80, 0.5),
            None,
        );
        assert!((selection[2] - 0.82).abs() < f32::EPSILON);
    }

    #[test]
    fn shift_drawing_keeps_the_saved_selection_aspect() {
        let selection = selection_from_points_with_aspect((0.1, 0.1), (0.8, 0.6), 0.75);
        let aspect = (selection[2] - selection[0]) / (selection[3] - selection[1]);
        assert!((aspect - 0.75).abs() < 1e-5);
        assert!(selection.iter().all(|value| (0. ..=1.).contains(value)));
    }

    #[test]
    fn shift_resize_preserves_aspect_for_every_handle() {
        let original = [0.25, 0.25, 0.75, 0.75];
        let aspect = selection_aspect(original);
        for handle in CropHandle::ALL {
            let start = handle.position(original);
            let delta = match handle {
                CropHandle::TopLeft => (-0.1, -0.05),
                CropHandle::Top => (0., -0.1),
                CropHandle::TopRight => (0.1, -0.05),
                CropHandle::Right => (0.1, 0.),
                CropHandle::BottomRight => (0.1, 0.1),
                CropHandle::Bottom => (0., 0.1),
                CropHandle::BottomLeft => (-0.1, 0.1),
                CropHandle::Left => (-0.1, 0.),
            };
            let selection = resize_from_drag(
                original,
                handle,
                start,
                (start.0 + delta.0, start.1 + delta.1),
                Some(aspect),
            );
            let actual = (selection[2] - selection[0]) / (selection[3] - selection[1]);
            assert!((actual - aspect).abs() < 1e-5, "{handle:?}: {selection:?}");
            assert!(selection.iter().all(|value| (0. ..=1.).contains(value)));
        }
    }

    #[test]
    fn shift_resize_keeps_an_offset_side_drag_anchored() {
        let original = [0.2, 0.2, 0.8, 0.8];
        let selection = resize_from_drag(
            original,
            CropHandle::Right,
            (0.78, 0.5),
            (0.80, 0.5),
            Some(selection_aspect(original)),
        );
        assert!((selection[2] - 0.82).abs() < 1e-5);
        assert!((selection[2] - selection[0] - (selection[3] - selection[1])).abs() < 1e-5);
    }

    #[test]
    fn reversed_pixel_bounds_are_sorted_before_rounding() {
        let r = crop_pixels([0.9, 0.8, 0.1, 0.2], 1920, 1080).unwrap();
        assert_eq!((r.x, r.y, r.width, r.height), (192, 216, 1536, 648));
    }
}

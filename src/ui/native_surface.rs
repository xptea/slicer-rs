//! Native X11 and AppKit drawables for GPU video presentation.
//!
//! GPUI's Linux renderer owns the top-level window and paints its scene with
//! wgpu.  A video renderer such as libmpv cannot paint into that scene without
//! a shared-texture integration, so the production Linux path uses an X11
//! child window instead.  libmpv is given the child XID and presents directly
//! to that drawable.  No video frame is copied through the CPU or GPUI's image
//! cache.
//!
//! The Linux implementation supports X11 only.  Wayland does not allow an
//! arbitrary foreign surface to be embedded as a child of an `xdg_toplevel`.
//! On a Wayland desktop, run Slicer through XWayland by removing
//! `WAYLAND_DISPLAY` before GPUI initializes and keeping `DISPLAY` set.  The
//! caller gets a descriptive error when GPUI exposes a Wayland handle instead
//! of silently falling back to a screenshot or CPU frame path.

use anyhow::{Result, anyhow};
#[cfg(not(target_os = "macos"))]
use gpui_kit::Refineable;
#[cfg(not(target_os = "macos"))]
use gpui_kit::gpui::{
    App, Element, ElementId, GlobalElementId, InspectorElementId, IntoElement, LayoutId, Style,
    StyleRefinement, Styled,
};
use gpui_kit::gpui::{Bounds, Pixels, Window};

#[cfg(target_os = "linux")]
mod platform {
    use super::*;
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use std::{
        cell::RefCell,
        rc::{Rc, Weak},
    };
    use x11rb::protocol::shape::ConnectionExt as _;
    use x11rb::protocol::xproto::ConnectionExt as _;
    use x11rb::{COPY_DEPTH_FROM_PARENT, COPY_FROM_PARENT};
    use x11rb::{
        connection::Connection,
        protocol::{shape, xproto},
        rust_connection::RustConnection,
    };

    const DEFAULT_CORNER_RADIUS: f32 = 14.0;

    /// The integer geometry sent to X11 after converting GPUI logical pixels
    /// to device pixels.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    struct DeviceGeometry {
        x: i16,
        y: i16,
        width: u16,
        height: u16,
    }

    impl DeviceGeometry {
        fn has_area(self) -> bool {
            self.width > 0 && self.height > 0
        }
    }

    fn desired_map_state(visible: bool, has_area: bool, mapped: bool) -> bool {
        if !visible {
            false
        } else if has_area {
            true
        } else {
            // Keep a mapped surface during transient empty layout passes while
            // the parent window is being resized.
            mapped
        }
    }

    /// State owned by `NativeSurface`.  The X11 connection is deliberately a
    /// separate RustConnection instead of a borrowed wrapper around GPUI's
    /// private XCB connection.  It uses the same `DISPLAY` selected by GPUI,
    /// while keeping ownership and request sequencing local to this surface.
    struct SurfaceState {
        connection: RustConnection,
        window: xproto::Window,
        geometry: Option<DeviceGeometry>,
        mapped: bool,
        corner_radius: f32,
        scale_factor: f32,
        shaped: Option<(u16, u16, u16)>,
    }

    impl SurfaceState {
        fn update(
            &mut self,
            bounds: Bounds<Pixels>,
            scale_factor: f32,
            visible: bool,
        ) -> Result<()> {
            let geometry = device_geometry(bounds, scale_factor)?;
            let mut changed = false;

            if let Some(geometry) = geometry {
                if self.geometry != Some(geometry) {
                    self.connection
                        .configure_window(
                            self.window,
                            &xproto::ConfigureWindowAux::new()
                                .x(i32::from(geometry.x))
                                .y(i32::from(geometry.y))
                                .width(u32::from(geometry.width))
                                .height(u32::from(geometry.height)),
                        )?
                        // Keep geometry and the optional shape update in the
                        // same X11 output batch. Calling `check()` here would
                        // flush and round-trip before the shape request, and
                        // the compositor can present that intermediate frame
                        // while libmpv is reallocating its swapchain.
                        .ignore_error();
                    self.geometry = Some(geometry);
                    changed = true;
                }

                // The shape radius is expressed in logical pixels while XShape
                // consumes device pixels. Rechecking the cached shape key also
                // handles a display scale-factor change whose rounded device
                // radius changes without changing the final integer geometry.
                self.scale_factor = scale_factor;
                let shape_before = self.shaped;
                self.apply_rounded_shape(geometry.width, geometry.height)?;
                changed |= self.shaped != shape_before;
            }

            // X11 rejects zero-sized windows. During a native top-level resize
            // GPUI can briefly report an empty child layout while the flex tree
            // settles. Keep the current mapped state for that transient frame;
            // unmapping and remapping the parent makes libmpv's descendant
            // surface flash black. An explicit `visible = false` still wins.
            let should_map = desired_map_state(
                visible,
                geometry.is_some_and(DeviceGeometry::has_area),
                self.mapped,
            );
            if should_map != self.mapped {
                if should_map {
                    self.connection
                        .map_window(self.window)?
                        .check()
                        .map_err(|error| anyhow!("map native video surface: {error}"))?;
                } else {
                    self.connection
                        .unmap_window(self.window)?
                        .check()
                        .map_err(|error| anyhow!("unmap native video surface: {error}"))?;
                }
                self.mapped = should_map;
                changed = true;
            }

            if changed {
                self.connection
                    .flush()
                    .map_err(|error| anyhow!("flush native video surface: {error}"))?;
            }
            Ok(())
        }

        fn apply_rounded_shape(&mut self, width: u16, height: u16) -> Result<()> {
            let radius = self
                .corner_radius
                .max(0.0)
                .mul_add(self.scale_factor, 0.0)
                .round()
                .min(f32::from(width.min(height)) / 2.0) as u16;

            let shape_key = (width, height, radius);
            if self.shaped == Some(shape_key) {
                return Ok(());
            }

            // XShape has no anti-aliased rounded-rectangle primitive. A
            // one-pixel horizontal span per row gives the native child the
            // same corner silhouette as GPUI's rounded preview container.
            // For an unusually large virtual desktop, clear any old mask to a
            // full rectangle rather than overflowing signed row coordinates.
            let rectangles = if radius == 0 || width > i16::MAX as u16 || height > i16::MAX as u16 {
                vec![xproto::Rectangle {
                    x: 0,
                    y: 0,
                    width,
                    height,
                }]
            } else {
                rounded_rectangles(width, height, radius)
            };
            self.set_shape(shape_key, rectangles)
        }

        fn set_shape(
            &mut self,
            shape_key: (u16, u16, u16),
            rectangles: Vec<xproto::Rectangle>,
        ) -> Result<()> {
            if self.shaped == Some(shape_key) {
                return Ok(());
            }
            self.connection
                .shape_rectangles(
                    shape::SO::SET,
                    shape::SK::BOUNDING,
                    xproto::ClipOrdering::YX_BANDED,
                    self.window,
                    0,
                    0,
                    &rectangles,
                )?
                // Geometry and shape requests are flushed together below.
                // Waiting for a checked void request on every resize forces
                // an X11 round-trip between frames and lets the compositor
                // show the parent's temporary cleared state. Dropping the
                // error here keeps resize on the same asynchronous path as
                // the configure request; rendering remains a rectangle if an
                // optional XShape server error is reported.
                .ignore_error();
            self.shaped = Some(shape_key);
            Ok(())
        }
    }

    impl Drop for SurfaceState {
        fn drop(&mut self) {
            // Destruction is best-effort.  The player is owned by the caller
            // and must be dropped before this state, so libmpv never retains a
            // stale `wid` while the XID is being destroyed.
            let _ = self
                .connection
                .destroy_window(self.window)
                .map_err(anyhow::Error::from)
                .and_then(|cookie| cookie.check().map_err(anyhow::Error::from));
            let _ = self.connection.flush();
        }
    }

    /// Owner for a native X11 child drawable used by libmpv.
    pub struct NativeSurface {
        state: Rc<RefCell<SurfaceState>>,
    }

    impl NativeSurface {
        /// Creates an unmapped X11 child of GPUI's top-level window.
        ///
        /// The child is initially one device pixel in size and remains
        /// unmapped until `update` receives a visible, non-empty layout.
        pub fn new(window: &Window) -> Result<Self> {
            let parent = parent_window_id(window)?;
            let (connection, _) = x11rb::connect(None)
                .map_err(|error| anyhow!("connect to GPUI's X11 display: {error}"))?;
            let child = connection
                .generate_id()
                .map_err(|error| anyhow!("allocate native video X11 window ID: {error}"))?;

            connection
                .create_window(
                    COPY_DEPTH_FROM_PARENT,
                    child,
                    parent,
                    0,
                    0,
                    1,
                    1,
                    0,
                    xproto::WindowClass::INPUT_OUTPUT,
                    COPY_FROM_PARENT,
                    &xproto::CreateWindowAux::new()
                        // Do not clear the drawable to an opaque black pixel
                        // when its size changes. The parent is the GPUI
                        // surface, so inheriting its background lets the last
                        // composed frame remain visible while libmpv's child
                        // reallocates its GPU swapchain. These are hints (X11
                        // compositors may ignore backing stores), but they
                        // avoid forcing a black clear on the common path.
                        .background_pixmap(xproto::BackPixmap::PARENT_RELATIVE)
                        .bit_gravity(xproto::Gravity::NORTH_WEST)
                        .backing_store(xproto::BackingStore::ALWAYS)
                        // Deliberately do not select keyboard or pointer
                        // events.  The empty XShape input region below is also
                        // required: an X11 child remains the XDND hit target
                        // even when it selects no events, so GPUI would never
                        // see a file drop over the video.
                        .event_mask(xproto::EventMask::NO_EVENT),
                )?
                .check()
                .map_err(|error| anyhow!("create native video X11 child: {error}"))?;

            // This window is a rendering-only bridge for libmpv.  Make its
            // input region empty so pointer events and XDND client messages
            // resolve to GPUI's top-level X11 window instead of being
            // swallowed by the native drawable.  `NO_EVENT` above only
            // controls which events this client selects; it does not remove
            // the child from X11 hit testing.
            connection
                .shape_rectangles(
                    shape::SO::SET,
                    shape::SK::INPUT,
                    xproto::ClipOrdering::UNSORTED,
                    child,
                    0,
                    0,
                    &[],
                )?
                .check()
                .map_err(|error| anyhow!("make native video child input-transparent: {error}"))?;
            connection
                .flush()
                .map_err(|error| anyhow!("flush native video X11 child: {error}"))?;

            Ok(Self {
                state: Rc::new(RefCell::new(SurfaceState {
                    connection,
                    window: child,
                    geometry: None,
                    mapped: false,
                    corner_radius: DEFAULT_CORNER_RADIUS,
                    scale_factor: 1.0,
                    shaped: None,
                })),
            })
        }

        /// Returns the X11 child window ID to pass to libmpv's `wid` option.
        ///
        /// libmpv's X11 `wid` mode creates and maps its own child below this
        /// window. Call `update(..., true)` once before constructing the
        /// player and keep the surface mapped until the first frame arrives;
        /// an unmapped parent leaves libmpv's child unmapped and produces a
        /// black preview even when the GPU backend initialized successfully.
        pub fn window_id(&self) -> u64 {
            u64::from(self.state.borrow().window)
        }

        /// Repositions/resizes the child and maps or unmaps it.
        ///
        /// `bounds` is in GPUI logical pixels and uses the same client-area
        /// origin as the element tree.  The conversion rounds each edge
        /// independently so adjacent GPUI edges remain aligned at fractional
        /// scale factors.  GPUI's client decoration inset is already reflected
        /// by the element bounds (the app's frame supplies the visible inset),
        /// so it is not added a second time here.
        pub fn update(
            &mut self,
            bounds: Bounds<Pixels>,
            scale_factor: f32,
            visible: bool,
        ) -> Result<()> {
            self.state
                .try_borrow_mut()
                .map_err(|_| anyhow!("native video surface is already being updated"))?
                .update(bounds, scale_factor, visible)
        }

        /// Sets the shape radius in GPUI logical pixels.  The default matches
        /// the editor preview's 14px rounded container.
        #[allow(dead_code)]
        pub fn set_corner_radius(&mut self, radius: f32) -> Result<()> {
            if !radius.is_finite() || radius < 0.0 {
                return Err(anyhow!(
                    "native video corner radius must be finite and non-negative"
                ));
            }
            let mut state = self
                .state
                .try_borrow_mut()
                .map_err(|_| anyhow!("native video surface is already being updated"))?;
            state.corner_radius = radius;
            state.shaped = None;
            if let Some(geometry) = state.geometry {
                state.apply_rounded_shape(geometry.width, geometry.height)?;
                state
                    .connection
                    .flush()
                    .map_err(|error| anyhow!("flush native video shape: {error}"))?;
            }
            Ok(())
        }

        /// Unmaps the child while retaining its geometry and XID.
        ///
        /// The libmpv child remains mapped in X11's hierarchy and becomes
        /// `IsUnviewable` while this parent is hidden; mapping this window
        /// again makes it viewable without recreating the player.
        pub fn hide(&mut self) {
            let Ok(mut state) = self.state.try_borrow_mut() else {
                return;
            };
            if !state.mapped {
                return;
            }
            if state
                .connection
                .unmap_window(state.window)
                .map_err(anyhow::Error::from)
                .and_then(|cookie| cookie.check().map_err(anyhow::Error::from))
                .is_ok()
            {
                state.mapped = false;
                let _ = state.connection.flush();
            }
        }

        /// Maps the child using its most recent valid geometry.
        #[allow(dead_code)]
        pub fn show(&mut self) -> Result<()> {
            let mut state = self
                .state
                .try_borrow_mut()
                .map_err(|_| anyhow!("native video surface is already being updated"))?;
            if state.mapped || !state.geometry.is_some_and(DeviceGeometry::has_area) {
                return Ok(());
            }
            state
                .connection
                .map_window(state.window)?
                .check()
                .map_err(|error| anyhow!("map native video surface: {error}"))?;
            state.mapped = true;
            state
                .connection
                .flush()
                .map_err(|error| anyhow!("flush native video surface: {error}"))
        }

        /// Creates a weak GPUI element that keeps the native child synced from
        /// the element prepaint pass without extending the owner's lifetime.
        #[allow(dead_code)]
        pub fn element(&self) -> NativeSurfaceElement {
            NativeSurfaceElement {
                state: Rc::downgrade(&self.state),
                visible: true,
                style: StyleRefinement::default(),
            }
        }
    }

    /// A zero-paint GPUI element whose prepaint pass updates a native surface.
    ///
    /// The element only holds a `Weak` reference.  Dropping the owning
    /// `NativeSurface` therefore destroys the X11 child even if a frame still
    /// contains an element value.
    #[allow(dead_code)]
    pub struct NativeSurfaceElement {
        state: Weak<RefCell<SurfaceState>>,
        visible: bool,
        style: StyleRefinement,
    }

    #[allow(dead_code)]
    impl NativeSurfaceElement {
        /// Controls whether the X11 child is mapped during prepaint.
        pub fn visible(mut self, visible: bool) -> Self {
            self.visible = visible;
            self
        }
    }

    impl IntoElement for NativeSurfaceElement {
        type Element = Self;

        fn into_element(self) -> Self::Element {
            self
        }
    }

    impl Element for NativeSurfaceElement {
        type RequestLayoutState = Style;
        type PrepaintState = ();

        fn id(&self) -> Option<ElementId> {
            None
        }

        fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
            None
        }

        fn request_layout(
            &mut self,
            _id: Option<&GlobalElementId>,
            _inspector_id: Option<&InspectorElementId>,
            window: &mut Window,
            cx: &mut App,
        ) -> (LayoutId, Self::RequestLayoutState) {
            let mut style = Style::default();
            style.refine(&self.style);
            let layout_id = window.request_layout(style.clone(), [], cx);
            (layout_id, style)
        }

        fn prepaint(
            &mut self,
            _id: Option<&GlobalElementId>,
            _inspector_id: Option<&InspectorElementId>,
            bounds: Bounds<Pixels>,
            _request_layout: &mut Self::RequestLayoutState,
            window: &mut Window,
            _cx: &mut App,
        ) -> Self::PrepaintState {
            if let Some(state) = self.state.upgrade()
                && let Ok(mut state) = state.try_borrow_mut()
            {
                let _ = state.update(bounds, window.scale_factor(), self.visible);
            }
        }

        fn paint(
            &mut self,
            _id: Option<&GlobalElementId>,
            _inspector_id: Option<&InspectorElementId>,
            _bounds: Bounds<Pixels>,
            _request_layout: &mut Self::RequestLayoutState,
            _prepaint: &mut Self::PrepaintState,
            _window: &mut Window,
            _cx: &mut App,
        ) {
            // libmpv owns the pixels in the child window.  GPUI intentionally
            // emits no scene quad for this element.
        }
    }

    impl Styled for NativeSurfaceElement {
        fn style(&mut self) -> &mut StyleRefinement {
            &mut self.style
        }
    }

    fn parent_window_id(window: &Window) -> Result<xproto::Window> {
        // `gpui::Window` also has an inherent `window_handle` method for its
        // logical GPUI handle. Call the raw-window-handle trait explicitly so
        // this code receives `RawWindowHandle` from the platform backend.
        let handle = HasWindowHandle::window_handle(window)
            .map_err(|error| anyhow!("get GPUI native window handle: {error}"))?;
        match handle.as_raw() {
            RawWindowHandle::Xcb(handle) => Ok(handle.window.get()),
            RawWindowHandle::Xlib(handle) => u32::try_from(handle.window)
                .map_err(|_| anyhow!("GPUI Xlib window ID does not fit an X11 window")),
            RawWindowHandle::Wayland(_) => Err(anyhow!(
                "native GPU video requires an X11 GPUI window; launch through XWayland with WAYLAND_DISPLAY unset and DISPLAY set"
            )),
            _ => Err(anyhow!(
                "native GPU video requires GPUI's X11 window handle (Xcb or Xlib)"
            )),
        }
    }

    fn device_geometry(
        bounds: Bounds<Pixels>,
        scale_factor: f32,
    ) -> Result<Option<DeviceGeometry>> {
        if !scale_factor.is_finite() || scale_factor <= 0.0 {
            return Err(anyhow!("invalid GPUI scale factor {scale_factor}"));
        }

        let origin_x = f32::from(bounds.origin.x);
        let origin_y = f32::from(bounds.origin.y);
        let right = origin_x + f32::from(bounds.size.width);
        let bottom = origin_y + f32::from(bounds.size.height);
        if !origin_x.is_finite()
            || !origin_y.is_finite()
            || !right.is_finite()
            || !bottom.is_finite()
            || bounds.size.width <= Pixels::from(0.0)
            || bounds.size.height <= Pixels::from(0.0)
        {
            return Ok(None);
        }

        let left = (origin_x * scale_factor).round();
        let top = (origin_y * scale_factor).round();
        let right = (right * scale_factor).round();
        let bottom = (bottom * scale_factor).round();

        // X11 child coordinates are signed 16-bit and dimensions are
        // unsigned 16-bit.  Clamp pathological layout values instead of
        // wrapping into a different part of the parent.
        let x = left.clamp(f32::from(i16::MIN), f32::from(i16::MAX)) as i16;
        let y = top.clamp(f32::from(i16::MIN), f32::from(i16::MAX)) as i16;
        let width = (right - left).clamp(1.0, f32::from(u16::MAX)) as u16;
        let height = (bottom - top).clamp(1.0, f32::from(u16::MAX)) as u16;
        Ok(Some(DeviceGeometry {
            x,
            y,
            width,
            height,
        }))
    }

    fn rounded_rectangles(width: u16, height: u16, radius: u16) -> Vec<xproto::Rectangle> {
        let width_f = f32::from(width);
        let height_f = f32::from(height);
        let radius_f = f32::from(radius);
        let center = radius_f - 0.5;
        let mut rectangles = Vec::with_capacity(usize::from(height));
        for row in 0..height {
            let edge_row = f32::from(row.min(height - 1 - row));
            let inset = if edge_row >= radius_f {
                0
            } else {
                let dy = center - edge_row;
                let dx = (radius_f * radius_f - dy * dy).max(0.0).sqrt();
                (center - dx).ceil().max(0.0) as u16
            };
            let span = width.saturating_sub(inset.saturating_mul(2));
            if span > 0 {
                rectangles.push(xproto::Rectangle {
                    x: inset.min(i16::MAX as u16) as i16,
                    y: row as i16,
                    width: span,
                    height: 1,
                });
            }
        }
        // Keep the parameters visibly used in debug builds and make the
        // intended invariant explicit for future changes to the algorithm.
        debug_assert!(width_f >= radius_f * 2.0 && height_f >= radius_f * 2.0);
        rectangles
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use gpui_kit::gpui::{point, px, size};

        #[test]
        fn geometry_rounds_each_edge_at_fractional_scale() {
            let bounds = Bounds::new(point(px(1.25), px(2.5)), size(px(10.25), px(4.5)));
            let geometry = device_geometry(bounds, 1.5).unwrap().unwrap();
            assert_eq!(geometry.x, 2);
            assert_eq!(geometry.y, 4);
            assert_eq!(geometry.width, 15);
            assert_eq!(geometry.height, 7);
        }

        #[test]
        fn empty_or_negative_layout_is_hidden() {
            let bounds = Bounds::new(point(px(0.), px(0.)), size(px(0.), px(10.)));
            assert!(device_geometry(bounds, 1.0).unwrap().is_none());
        }

        #[test]
        fn transient_empty_layout_keeps_mapped_surface() {
            assert!(desired_map_state(true, false, true));
            assert!(!desired_map_state(true, false, false));
            assert!(!desired_map_state(false, true, true));
        }

        #[test]
        fn rounded_shape_spans_cover_every_row() {
            let rectangles = rounded_rectangles(100, 40, 14);
            assert_eq!(rectangles.len(), 40);
            assert!(rectangles.iter().all(|rect| rect.height == 1));
            assert!(rectangles.first().unwrap().x > 0);
            assert_eq!(rectangles[20].x, 0);
            assert_eq!(
                rectangles.first().unwrap().width,
                rectangles.last().unwrap().width
            );
        }
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
mod platform {
    use super::*;

    /// Placeholder surface for unsupported platforms.  The desktop app can
    /// still compile and report a clear error instead of selecting a CPU
    /// screenshot fallback.
    pub struct NativeSurface;

    impl NativeSurface {
        pub fn new(_window: &Window) -> Result<Self> {
            Err(anyhow!(
                "native GPU video surface is currently implemented for Linux/X11 only"
            ))
        }

        pub fn window_id(&self) -> u64 {
            0
        }

        pub fn update(
            &mut self,
            _bounds: Bounds<Pixels>,
            _scale_factor: f32,
            _visible: bool,
        ) -> Result<()> {
            Err(anyhow!(
                "native GPU video surface is currently implemented for Linux/X11 only"
            ))
        }

        pub fn set_corner_radius(&mut self, _radius: f32) -> Result<()> {
            Ok(())
        }

        pub fn hide(&mut self) {}

        pub fn show(&mut self) -> Result<()> {
            Ok(())
        }

        pub fn element(&self) -> NativeSurfaceElement {
            NativeSurfaceElement {
                visible: false,
                style: StyleRefinement::default(),
            }
        }
    }

    #[allow(dead_code)]
    pub struct NativeSurfaceElement {
        visible: bool,
        style: StyleRefinement,
    }

    #[allow(dead_code)]
    impl NativeSurfaceElement {
        pub fn visible(mut self, visible: bool) -> Self {
            self.visible = visible;
            self
        }
    }

    impl IntoElement for NativeSurfaceElement {
        type Element = Self;

        fn into_element(self) -> Self::Element {
            self
        }
    }

    impl Element for NativeSurfaceElement {
        type RequestLayoutState = Style;
        type PrepaintState = ();

        fn id(&self) -> Option<ElementId> {
            None
        }

        fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
            None
        }

        fn request_layout(
            &mut self,
            _id: Option<&GlobalElementId>,
            _inspector_id: Option<&InspectorElementId>,
            window: &mut Window,
            cx: &mut App,
        ) -> (LayoutId, Self::RequestLayoutState) {
            let mut style = Style::default();
            style.refine(&self.style);
            let layout_id = window.request_layout(style.clone(), [], cx);
            (layout_id, style)
        }

        fn prepaint(
            &mut self,
            _id: Option<&GlobalElementId>,
            _inspector_id: Option<&InspectorElementId>,
            _bounds: Bounds<Pixels>,
            _request_layout: &mut Self::RequestLayoutState,
            _window: &mut Window,
            _cx: &mut App,
        ) {
            let _ = self.visible;
        }

        fn paint(
            &mut self,
            _id: Option<&GlobalElementId>,
            _inspector_id: Option<&InspectorElementId>,
            _bounds: Bounds<Pixels>,
            _request_layout: &mut Self::RequestLayoutState,
            _prepaint: &mut Self::PrepaintState,
            _window: &mut Window,
            _cx: &mut App,
        ) {
        }
    }

    impl Styled for NativeSurfaceElement {
        fn style(&mut self) -> &mut StyleRefinement {
            &mut self.style
        }
    }
}

#[allow(unused_imports)]
pub use platform::NativeSurface;
#[cfg(not(target_os = "macos"))]
pub use platform::NativeSurfaceElement;

#[cfg(target_os = "macos")]
#[path = "native_surface_macos.rs"]
mod platform;

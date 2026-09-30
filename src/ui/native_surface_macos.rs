//! Input-transparent CGL view. AppKit coordinates are points, independent of Retina scaling.
#![allow(deprecated)] // mpv's stable GPU render API uses OpenGL on macOS.
use super::*;
use objc2::rc::Retained;
use objc2::{AnyThread, MainThreadMarker, MainThreadOnly, define_class, msg_send};
use objc2_app_kit::{NSOpenGLPixelFormat, NSOpenGLView, NSView};
use objc2_foundation::{NSPoint, NSRect, NSSize};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use slicer::native_player::{NativePlayer, OpenGlRenderer};
use std::{
    ffi::{c_char, c_void},
    ptr::NonNull,
};

define_class!(
    #[unsafe(super = NSOpenGLView)]
    #[thread_kind = MainThreadOnly]
    struct VideoView;
    impl VideoView {
        // Returning nil skips the entire native subtree, including mpv's drawable.
        #[unsafe(method(hitTest:))]
        fn hit_test(&self, _point: NSPoint) -> *mut NSView { std::ptr::null_mut() }
    }
);

#[link(name = "System")]
unsafe extern "C" {
    fn dlsym(handle: *mut c_void, name: *const c_char) -> *mut c_void;
}
unsafe extern "C" fn gl_proc(_: *mut c_void, name: *const c_char) -> *mut c_void {
    // RTLD_DEFAULT; OpenGL is linked by AppKit's NSOpenGLView implementation.
    unsafe { dlsym(-2isize as *mut c_void, name) }
}

pub struct NativeSurface {
    renderer: Option<OpenGlRenderer>,
    view: Retained<VideoView>,
    parent: Retained<NSView>,
    frame: Option<NSRect>,
}
impl NativeSurface {
    pub fn new(window: &Window) -> Result<Self> {
        let mtm = MainThreadMarker::new()
            .ok_or_else(|| anyhow!("video view requires the main thread"))?;
        let RawWindowHandle::AppKit(handle) = HasWindowHandle::window_handle(window)
            .map_err(|e| anyhow!("AppKit window handle: {e}"))?
            .as_raw()
        else {
            return Err(anyhow!("video preview requires an AppKit window"));
        };
        // GPUI owns this live view; retain it for the surface lifetime.
        let parent = unsafe { Retained::retain(handle.ns_view.as_ptr() as *mut NSView) }
            .ok_or_else(|| anyhow!("GPUI AppKit view is null"))?;
        let mut attributes = [99u32, 0x3200, 5, 8, 24, 11, 8, 73, 0];
        let format = unsafe {
            NSOpenGLPixelFormat::initWithAttributes(
                NSOpenGLPixelFormat::alloc(),
                NonNull::new(attributes.as_mut_ptr()).unwrap(),
            )
        }
        .ok_or_else(|| anyhow!("macOS could not create an OpenGL 3.2 pixel format"))?;
        let frame = NSRect::new(NSPoint::new(0., 0.), NSSize::new(1., 1.));
        let view: Retained<VideoView> = unsafe {
            msg_send![VideoView::alloc(mtm), initWithFrame: frame, pixelFormat: &*format]
        };
        view.setHidden(true);
        view.setWantsBestResolutionOpenGLSurface(true);
        view.setWantsLayer(true);
        if let Some(layer) = view.layer() {
            layer.setCornerRadius(14.);
            layer.setMasksToBounds(true);
        }
        parent.addSubview(&view);
        Ok(Self {
            renderer: None,
            view,
            parent,
            frame: None,
        })
    }
    // Nonzero selects the render API; it is never passed as wid on macOS.
    pub fn window_id(&self) -> u64 {
        Retained::as_ptr(&self.view) as u64
    }
    pub fn attach_player(&mut self, player: &NativePlayer) -> Result<()> {
        if self.renderer.is_none() {
            let context = self
                .view
                .openGLContext()
                .ok_or_else(|| anyhow!("video OpenGL context unavailable"))?;
            context.makeCurrentContext();
            self.renderer = Some(
                unsafe { player.create_opengl_renderer(gl_proc) }.map_err(anyhow::Error::msg)?,
            );
        }
        Ok(())
    }
    pub fn detach_player(&mut self) {
        if let Some(context) = self.view.openGLContext() {
            context.makeCurrentContext();
        }
        drop(self.renderer.take());
    }
    pub fn update(
        &mut self,
        bounds: Bounds<Pixels>,
        _scale_factor: f32,
        visible: bool,
    ) -> Result<()> {
        let width = f64::from(f32::from(bounds.size.width));
        let height = f64::from(f32::from(bounds.size.height));
        if !visible || width <= 0. || height <= 0. {
            self.hide();
            return Ok(());
        }
        let x = f64::from(f32::from(bounds.origin.x));
        let top = f64::from(f32::from(bounds.origin.y));
        let y = if self.parent.isFlipped() {
            top
        } else {
            self.parent.bounds().size.height - top - height
        };
        let frame = NSRect::new(NSPoint::new(x, y), NSSize::new(width, height));
        if self.frame != Some(frame) {
            self.view.setFrame(frame);
            self.view.update();
            self.frame = Some(frame);
        }
        self.view.setHidden(false);
        if let Some(renderer) = &mut self.renderer {
            let context = self
                .view
                .openGLContext()
                .ok_or_else(|| anyhow!("video OpenGL context unavailable"))?;
            context.makeCurrentContext();
            let backing = self.view.convertRectToBacking(self.view.bounds());
            unsafe {
                renderer.render(
                    backing.size.width.round() as i32,
                    backing.size.height.round() as i32,
                )
            }
            .map_err(anyhow::Error::msg)?;
            context.flushBuffer();
            renderer.report_swap();
        }
        Ok(())
    }
    pub fn hide(&mut self) {
        self.view.setHidden(true);
    }
}
impl Drop for NativeSurface {
    fn drop(&mut self) {
        self.detach_player();
        self.view.removeFromSuperview();
    }
}

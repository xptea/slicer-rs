//! CGL render API bridge. The retained core outlives both the event worker and renderer.
use super::*;
use std::marker::PhantomData;
use std::rc::Rc;

pub(super) struct MacCore {
    pub(super) handle: usize,
    pub(super) api: Arc<MpvApi>,
}
impl Drop for MacCore {
    fn drop(&mut self) {
        unsafe { (self.api.terminate_destroy)(self.handle as *mut MpvHandle) };
    }
}

#[repr(C)]
struct RenderParam {
    kind: c_int,
    data: *mut c_void,
}
#[repr(C)]
struct GlInit {
    get_proc_address: unsafe extern "C" fn(*mut c_void, *const c_char) -> *mut c_void,
    context: *mut c_void,
}
#[repr(C)]
struct GlFbo {
    fbo: c_int,
    width: c_int,
    height: c_int,
    format: c_int,
}
type RenderCreate =
    unsafe extern "C" fn(*mut *mut c_void, *mut MpvHandle, *mut RenderParam) -> c_int;
type Render = unsafe extern "C" fn(*mut c_void, *mut RenderParam) -> c_int;
type RenderFree = unsafe extern "C" fn(*mut c_void);
type RenderSwap = unsafe extern "C" fn(*mut c_void);

/// Must stay on the thread with the CGL context used at creation.
pub struct OpenGlRenderer {
    context: *mut c_void,
    render: Render,
    free: RenderFree,
    swap: RenderSwap,
    core: Arc<MacCore>,
    _thread: PhantomData<Rc<()>>,
}
impl NativePlayer {
    /// Create the renderer before loading media.
    ///
    /// # Safety
    /// The caller must keep the same OpenGL context current for creation, rendering,
    /// and destruction. The callback must return valid OpenGL function pointers.
    pub unsafe fn create_opengl_renderer(
        &self,
        get_proc_address: unsafe extern "C" fn(*mut c_void, *const c_char) -> *mut c_void,
    ) -> Result<OpenGlRenderer, String> {
        let core = self
            .shared
            .core
            .lock()
            .map_err(|_| "player core lock poisoned")?
            .clone()
            .ok_or("player core unavailable")?;
        let library = &core.api._library;
        let create: RenderCreate = unsafe { library.get(b"mpv_render_context_create\0") }
            .map(|s| *s)
            .map_err(|e| e.to_string())?;
        let render: Render = unsafe { library.get(b"mpv_render_context_render\0") }
            .map(|s| *s)
            .map_err(|e| e.to_string())?;
        let free: RenderFree = unsafe { library.get(b"mpv_render_context_free\0") }
            .map(|s| *s)
            .map_err(|e| e.to_string())?;
        let swap: RenderSwap = unsafe { library.get(b"mpv_render_context_report_swap\0") }
            .map(|s| *s)
            .map_err(|e| e.to_string())?;
        let mut gl = GlInit {
            get_proc_address,
            context: ptr::null_mut(),
        };
        let mut params = [
            RenderParam {
                kind: 1,
                data: c"opengl".as_ptr() as *mut c_void,
            },
            RenderParam {
                kind: 2,
                data: &mut gl as *mut _ as *mut c_void,
            },
            RenderParam {
                kind: 0,
                data: ptr::null_mut(),
            },
        ];
        let mut context = ptr::null_mut();
        let code = unsafe {
            create(
                &mut context,
                core.handle as *mut MpvHandle,
                params.as_mut_ptr(),
            )
        };
        if code < 0 {
            return Err(format!("libmpv OpenGL initialization: {}", unsafe {
                core.api.error(code)
            }));
        }
        Ok(OpenGlRenderer {
            context,
            render,
            free,
            swap,
            core,
            _thread: PhantomData,
        })
    }
}
impl OpenGlRenderer {
    /// # Safety
    /// The CGL context used at creation must be current on this thread.
    pub unsafe fn render(&mut self, width: i32, height: i32) -> Result<(), String> {
        if width <= 0 || height <= 0 {
            return Ok(());
        }
        let mut fbo = GlFbo {
            fbo: 0,
            width,
            height,
            format: 0,
        };
        let mut flip: c_int = 1;
        let mut block: c_int = 0;
        let mut params = [
            RenderParam {
                kind: 3,
                data: &mut fbo as *mut _ as *mut c_void,
            },
            RenderParam {
                kind: 4,
                data: &mut flip as *mut _ as *mut c_void,
            },
            // Never wait for a presentation deadline on the GPUI event thread.
            RenderParam {
                kind: 12,
                data: &mut block as *mut _ as *mut c_void,
            },
            RenderParam {
                kind: 0,
                data: ptr::null_mut(),
            },
        ];
        let code = unsafe { (self.render)(self.context, params.as_mut_ptr()) };
        if code < 0 {
            return Err(unsafe { self.core.api.error(code) });
        }
        Ok(())
    }
    pub fn report_swap(&self) {
        unsafe { (self.swap)(self.context) };
    }
}
impl Drop for OpenGlRenderer {
    fn drop(&mut self) {
        unsafe { (self.free)(self.context) };
    }
}

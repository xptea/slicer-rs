//! Owned, worker-thread-only FFmpeg sessions. This implementation is explicitly software decode.
use super::project::Time;
use anyhow::{Result, bail};
use std::{
    ffi::{CStr, CString, c_char, c_void},
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};
#[repr(C)]
#[derive(Default, Clone, Copy, Debug)]
pub struct Info {
    pub width: i32,
    pub height: i32,
    pub video: i32,
    pub audio: i32,
    pub duration_us: i64,
}
#[repr(C)]
#[derive(Default)]
struct RawVideo {
    width: i32,
    height: i32,
    rgba: i32,
    full_range: i32,
    matrix: i32,
    transfer: i32,
    primaries: i32,
    pts_us: i64,
    duration_us: i64,
    data: *const u8,
    length: i32,
}
unsafe extern "C" {
    fn slicer_decoder_open(
        path: *const c_char,
        audio: i32,
        error: *mut c_char,
        error_len: i32,
        info: *mut Info,
    ) -> *mut c_void;
    fn slicer_decoder_interrupt(
        d: *mut c_void,
        callback: unsafe extern "C" fn(*mut c_void) -> i32,
        data: *mut c_void,
    );
    fn slicer_decoder_close(d: *mut c_void);
    fn slicer_decoder_error(d: *mut c_void) -> *const c_char;
    fn slicer_decoder_video(d: *mut c_void, time: i64, out: *mut RawVideo) -> i32;
    fn slicer_decoder_audio(d: *mut c_void, time: i64, out: *mut f32, count: i32) -> i32;
}
#[derive(Clone, Debug)]
pub struct Frame {
    pub width: u32,
    pub height: u32,
    pub rgba: bool,
    pub full_range: bool,
    pub matrix: i32,
    pub transfer: i32,
    pub primaries: i32,
    pub pts: Time,
    pub duration: Time,
    pub pixels: Arc<[u8]>,
}
struct Cancellation {
    generation: Arc<AtomicU64>,
    expected: u64,
}
unsafe extern "C" fn interrupted(data: *mut c_void) -> i32 {
    let token = unsafe { &*(data as *const Cancellation) };
    (token.generation.load(Ordering::Acquire) != token.expected) as i32
}
pub struct Decoder {
    cancellation: Option<Box<Cancellation>>,
    raw: *mut c_void,
    pub info: Info,
    last: Option<Arc<Frame>>,
    cache: std::collections::VecDeque<Arc<Frame>>,
    cache_bytes: usize,
}
// Ownership moves to one worker; methods require exclusive access and never share AV contexts.
unsafe impl Send for Decoder {}
impl Decoder {
    pub fn open(path: &Path, audio: bool) -> Result<Self> {
        if !path.is_file() {
            bail!("Media file is missing: {}", path.display());
        }
        let path = CString::new(path.as_os_str().as_encoded_bytes())?;
        let mut info = Info::default();
        let mut error = [0i8; 256];
        let raw = unsafe {
            slicer_decoder_open(
                path.as_ptr(),
                audio as i32,
                error.as_mut_ptr(),
                256,
                &mut info,
            )
        };
        if raw.is_null() {
            bail!(
                "{}",
                unsafe { CStr::from_ptr(error.as_ptr()) }.to_string_lossy()
            );
        }
        Ok(Self {
            cancellation: None,
            raw,
            info,
            last: None,
            cache: std::collections::VecDeque::new(),
            cache_bytes: 0,
        })
    }
    pub fn cancel_on_generation(&mut self, generation: Arc<AtomicU64>, expected: u64) {
        let mut token = Box::new(Cancellation {
            generation,
            expected,
        });
        unsafe {
            slicer_decoder_interrupt(
                self.raw,
                interrupted,
                (&mut *token as *mut Cancellation).cast(),
            )
        };
        self.cancellation = Some(token);
    }
    fn error(&self) -> anyhow::Error {
        anyhow::anyhow!(
            "{}",
            unsafe { CStr::from_ptr(slicer_decoder_error(self.raw)) }.to_string_lossy()
        )
    }
    pub fn video(&mut self, time: Time) -> Result<Arc<Frame>> {
        if let Some(index) = self
            .cache
            .iter()
            .position(|f| time >= f.pts && time < f.pts + f.duration)
        {
            let frame = self.cache.remove(index).unwrap();
            self.cache.push_back(frame.clone());
            return Ok(frame);
        }
        if let Some(f) = &self.last {
            if time >= f.pts && time < f.pts + f.duration {
                return Ok(f.clone());
            }
        }
        let mut f = RawVideo::default();
        let r = unsafe { slicer_decoder_video(self.raw, time.max(0), &mut f) };
        if r <= 0 {
            bail!(
                "{}",
                if r < 0 {
                    self.error().to_string()
                } else {
                    "No video frame".into()
                }
            );
        }
        if f.data.is_null() || f.length <= 0 {
            bail!("Invalid decoded frame");
        }
        let frame = Arc::new(Frame {
            width: f.width as u32,
            height: f.height as u32,
            rgba: f.rgba != 0,
            full_range: f.full_range != 0,
            matrix: f.matrix,
            transfer: f.transfer,
            primaries: f.primaries,
            pts: f.pts_us,
            duration: f.duration_us.max(1),
            pixels: unsafe { std::slice::from_raw_parts(f.data, f.length as usize) }.into(),
        });
        self.cache_bytes += frame.pixels.len();
        self.cache.push_back(frame.clone());
        while self.cache.len() > 1 && (self.cache.len() > 8 || self.cache_bytes > 32 * 1024 * 1024)
        {
            self.cache_bytes -= self.cache.pop_front().unwrap().pixels.len();
        }
        self.last = Some(frame.clone());
        Ok(frame)
    }
    pub fn audio(&mut self, time: Time, out: &mut [f32]) -> Result<()> {
        if unsafe { slicer_decoder_audio(self.raw, time, out.as_mut_ptr(), (out.len() / 2) as i32) }
            < 0
        {
            return Err(self.error());
        }
        Ok(())
    }
}
impl Drop for Decoder {
    fn drop(&mut self) {
        unsafe { slicer_decoder_close(self.raw) }
    }
}

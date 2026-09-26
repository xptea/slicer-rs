//! Offline render uses the same libmpv sources and OpenGL compositor as preview.
#[cfg(feature = "desktop")]
mod desktop {
    use super::super::{decoder::Decoder, gl_canvas::Renderer, project::*};
    use anyhow::{Result, bail};
    use std::{
        collections::HashMap,
        ffi::{CStr, CString, c_char, c_void},
        path::PathBuf,
        sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
            mpsc,
        },
    };
    pub enum ExportEvent {
        Progress(f64),
        Finished(Result<PathBuf, String>),
    }
    pub struct ExportHandle {
        pub events: mpsc::Receiver<ExportEvent>,
        cancel: Arc<AtomicBool>,
        thread: Option<std::thread::JoinHandle<()>>,
    }
    impl ExportHandle {
        pub fn spawn(project: Project, path: PathBuf) -> Result<Self> {
            project.validate()?;
            if project.duration() == 0 {
                bail!("The timeline is empty");
            }
            if project.width % 2 != 0 || project.height % 2 != 0 {
                bail!("MP4 export requires even dimensions");
            }
            if path.exists() {
                bail!("Output already exists; choose another name");
            }
            let (tx, rx) = mpsc::channel();
            let cancel = Arc::new(AtomicBool::new(false));
            let c = cancel.clone();
            let thread = std::thread::spawn(move || {
                let result = render(project, &path, &c, &tx)
                    .map(|_| path)
                    .map_err(|e| e.to_string());
                let _ = tx.send(ExportEvent::Finished(result));
            });
            Ok(Self {
                events: rx,
                cancel,
                thread: Some(thread),
            })
        }
        pub fn cancel(&self) {
            self.cancel.store(true, Ordering::Release);
        }
    }
    impl Drop for ExportHandle {
        fn drop(&mut self) {
            self.cancel();
            if let Some(t) = self.thread.take() {
                let _ = t.join();
            }
        }
    }
    struct Temp(PathBuf);
    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }
    unsafe extern "C" {
        fn slicer_encoder_open(
            path: *const c_char,
            w: i32,
            h: i32,
            num: i32,
            den: i32,
            error: *mut c_char,
            len: i32,
        ) -> *mut c_void;
        fn slicer_encoder_close(e: *mut c_void);
        fn slicer_encoder_error(e: *mut c_void) -> *const c_char;
        fn slicer_encoder_video(e: *mut c_void, rgba: *const u8) -> i32;
        fn slicer_encoder_audio(e: *mut c_void, audio: *const f32, count: i32) -> i32;
        fn slicer_encoder_finish(e: *mut c_void) -> i32;
    }
    struct Encoder(*mut c_void);
    impl Drop for Encoder {
        fn drop(&mut self) {
            unsafe { slicer_encoder_close(self.0) }
        }
    }
    impl Encoder {
        fn check(&self, result: i32) -> Result<()> {
            if result < 0 {
                bail!(
                    "{}",
                    unsafe { CStr::from_ptr(slicer_encoder_error(self.0)) }.to_string_lossy()
                );
            }
            Ok(())
        }
    }
    fn render(
        project: Project,
        path: &std::path::Path,
        cancel: &AtomicBool,
        tx: &mpsc::Sender<ExportEvent>,
    ) -> Result<()> {
        let parent = path.parent().unwrap_or(std::path::Path::new("."));
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos();
        let temp = Temp(parent.join(format!(".slicer-{}-{nonce}.mp4", std::process::id())));
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp.0)?;
        let name = CString::new(temp.0.as_os_str().as_encoded_bytes())?;
        let mut error = [0i8; 256];
        let raw = unsafe {
            slicer_encoder_open(
                name.as_ptr(),
                project.width as i32,
                project.height as i32,
                project.fps_num as i32,
                project.fps_den as i32,
                error.as_mut_ptr(),
                256,
            )
        };
        if raw.is_null() {
            bail!(
                "{}",
                unsafe { CStr::from_ptr(error.as_ptr()) }.to_string_lossy()
            );
        }
        let encoder = Encoder(raw);
        let mut canvas = Renderer::new(0, project.width, project.height)?;
        let mut audio = HashMap::new();
        let frames = ((project.duration() as i128 * project.fps_num as i128
            + SECOND as i128 * project.fps_den as i128
            - 1)
            / (SECOND as i128 * project.fps_den as i128)) as i64;
        let total_samples = project.duration() as i128 * 48000 / SECOND as i128;
        for index in 0..frames {
            if cancel.load(Ordering::Acquire) {
                bail!("Export cancelled");
            }
            let time = (index as i128 * SECOND as i128 * project.fps_den as i128
                / project.fps_num as i128) as i64;
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
            loop {
                if cancel.load(Ordering::Acquire) {
                    bail!("Export cancelled");
                }
                let status = canvas.render(
                    &project,
                    time,
                    index as u64 + 1,
                    false,
                    [project.width, project.height],
                    None,
                )?;
                if status.ready {
                    break;
                }
                if std::time::Instant::now() > deadline {
                    bail!("Timed out waiting for export frame {index}");
                }
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            let pixels = canvas.read_pixels([project.width, project.height]);
            encoder.check(unsafe { slicer_encoder_video(encoder.0, pixels.as_ptr()) })?;
            let a0 = index as i128 * 48000 * project.fps_den as i128 / project.fps_num as i128;
            let a1 = ((index + 1) as i128 * 48000 * project.fps_den as i128
                / project.fps_num as i128)
                .min(total_samples);
            let count = (a1 - a0).max(0) as usize;
            let mut mix = vec![0f32; count * 2];
            for t in &project.tracks {
                if t.muted {
                    continue;
                }
                for c in &t.clips {
                    if !c.audio {
                        continue;
                    }
                    let start = (c.start as i128 * 48000 / SECOND as i128).max(a0);
                    let end = (c.end() as i128 * 48000 / SECOND as i128).min(a1);
                    if end <= start {
                        continue;
                    }
                    if let std::collections::hash_map::Entry::Vacant(e) = audio.entry(c.id) {
                        e.insert(Decoder::open(&c.path, true)?);
                    }
                    let mut samples = vec![0f32; ((end - start) * 2) as usize];
                    audio.get_mut(&c.id).unwrap().audio(
                        c.source_in + (start * SECOND as i128 / 48000) as i64 - c.start,
                        &mut samples,
                    )?;
                    for (sample, (dst, src)) in mix
                        [((start - a0) * 2) as usize..((end - a0) * 2) as usize]
                        .iter_mut()
                        .zip(samples)
                        .enumerate()
                    {
                        *dst += src
                            * c.gain
                            * c.fade(
                                (start * SECOND as i128 / 48000) as i64
                                    + (sample / 2) as i64 * SECOND / 48000,
                            );
                    }
                }
            }
            audio.retain(|id, _| project.clip(*id).is_some_and(|c| c.end() > time));
            for x in &mut mix {
                *x = x.clamp(-1., 1.);
            }
            encoder
                .check(unsafe { slicer_encoder_audio(encoder.0, mix.as_ptr(), count as i32) })?;
            if index % 10 == 0 {
                let _ = tx.send(ExportEvent::Progress(index as f64 / frames as f64));
            }
        }
        encoder.check(unsafe { slicer_encoder_finish(encoder.0) })?;
        drop(encoder);
        if cancel.load(Ordering::Acquire) {
            bail!("Export cancelled");
        }
        std::fs::File::open(&temp.0)?.sync_all()?;
        std::fs::hard_link(&temp.0, path)?;
        Ok(())
    }
}
#[cfg(feature = "desktop")]
pub use desktop::*;

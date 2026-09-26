//! Worker-owned preview decoding. HDR uses the same GPU color pipeline as
//! playback; small SDR sources keep the inexpensive software path.
use super::{
    decoder::{Decoder, Frame, Info},
    gl_canvas::Renderer,
    project::{Clip, Project, Time, Transform},
};
use anyhow::{Result, bail};
use std::{
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

pub struct PreviewSource {
    path: PathBuf,
    decoder: Option<Decoder>,
    gpu: Option<(Renderer, Project, [u32; 2])>,
    cancel: Option<(Arc<AtomicU64>, u64)>,
    pub info: Info,
}
impl PreviewSource {
    pub fn open(path: &Path) -> Result<Self> {
        let decoder = Decoder::open_preview(path)?;
        let info = decoder.info;
        Ok(Self {
            path: path.into(),
            decoder: Some(decoder),
            gpu: None,
            cancel: None,
            info,
        })
    }
    pub fn cancel_on_generation(&mut self, token: Arc<AtomicU64>, version: u64) {
        if let Some(decoder) = &mut self.decoder {
            decoder.cancel_on_generation(token.clone(), version);
        }
        self.cancel = Some((token, version));
    }
    fn check_cancelled(&self) -> Result<()> {
        if self
            .cancel
            .as_ref()
            .is_some_and(|(token, version)| token.load(Ordering::Acquire) != *version)
        {
            bail!("Preview superseded");
        }
        Ok(())
    }
    pub fn video(&mut self, time: Time) -> Result<Arc<Frame>> {
        self.frame(time, false)
    }
    pub fn sequential_video(&mut self, time: Time) -> Result<Arc<Frame>> {
        self.frame(time, true)
    }
    fn frame(&mut self, time: Time, sequential: bool) -> Result<Arc<Frame>> {
        self.check_cancelled()?;
        if let Some(decoder) = &mut self.decoder {
            match decoder.video(time) {
                Ok(frame) => return Ok(frame),
                Err(error)
                    if error.to_string().contains("HDR media")
                        || error.to_string().contains("oriented preview") => {}
                Err(error) => return Err(error),
            }
            self.decoder = None;
        }
        if self.gpu.is_none() {
            let w = self.info.width.max(1) as f32;
            let h = self.info.height.max(1) as f32;
            let scale = (640. / w.max(h)).min(1.);
            let size = [
                (w * scale).round().max(1.) as u32,
                (h * scale).round().max(1.) as u32,
            ];
            let mut project = Project::default();
            project.width = size[0];
            project.height = size[1];
            project.tracks[0].clips.push(Clip {
                id: 1,
                path: self.path.clone(),
                start: 0,
                source_in: 0,
                duration: self.info.duration_us.max(1),
                source_duration: self.info.duration_us.max(1),
                visual: true,
                audio: false,
                still: false,
                transform: Transform::default(),
                gain: 1.,
                graphic: None,
                fade_in: 0,
                fade_out: 0,
            });
            self.gpu = Some((Renderer::new(0, size[0], size[1])?, project, size));
        }
        let time = time.clamp(0, (self.info.duration_us - 1).max(0));
        if sequential {
            self.gpu.as_mut().unwrap().0.step_preview(time)?;
        }
        let began = Instant::now();
        loop {
            self.check_cancelled()?;
            let (renderer, project, size) = self.gpu.as_mut().unwrap();
            let status = renderer.render(project, time, time as u64, false, *size, None)?;
            if let Some(error) = status.error {
                bail!("{error}");
            }
            if status.ready && (!sequential || renderer.preview_at_target(time)) {
                return Ok(Arc::new(Frame {
                    width: size[0],
                    height: size[1],
                    rgba: true,
                    full_range: true,
                    matrix: 1,
                    transfer: 13,
                    primaries: 1,
                    pts: time,
                    duration: 33_333,
                    pixels: renderer.read_pixels(*size).into(),
                }));
            }
            anyhow::ensure!(
                began.elapsed() < Duration::from_secs(10),
                "GPU preview timed out"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
    }
}

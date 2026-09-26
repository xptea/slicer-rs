//! libmpv controls run on their existing workers; this owner only calls the render API.
//! The compositor and all OpenGL resources stay on one thread, including destruction.
use super::graphics::Graphics;
use super::project::{Clip, Graphic, Project, Time, Transform};
use super::{
    decoder::Frame,
    scrub::{ScrubCache, Target},
};
use crate::native_player::NativePlayer;
use anyhow::{Result, bail};
use std::{
    collections::HashMap,
    ffi::{CStr, CString, c_char, c_void},
    path::PathBuf,
    sync::{Arc, Condvar, Mutex},
    time::{Duration, Instant},
};
unsafe extern "C" {
    fn slicer_gl_open(
        window: u64,
        w: i32,
        h: i32,
        library: *const c_char,
        error: *mut c_char,
        len: i32,
    ) -> *mut c_void;
    fn slicer_gl_close(c: *mut c_void);
    fn slicer_gl_source(
        c: *mut c_void,
        handle: *mut c_void,
        error: *mut c_char,
        len: i32,
    ) -> *mut c_void;
    fn slicer_gl_source_free(c: *mut c_void, s: *mut c_void);
    fn slicer_gl_video(c: *mut c_void, s: *mut c_void, w: i32, h: i32) -> i32;
    fn slicer_gl_image(s: *mut c_void, w: i32, h: i32, rgba: *const u8);
    fn slicer_gl_begin(c: *mut c_void, w: i32, h: i32) -> i32;
    fn slicer_gl_draw(c: *mut c_void, s: *mut c_void, t: *const f32, selected: i32);
    fn slicer_gl_finish(c: *mut c_void);
    fn slicer_gl_present(c: *mut c_void) -> i32;
    fn slicer_gl_read(c: *mut c_void, pixels: *mut u8);
}
struct Source {
    graphic: Option<(Graphic, u32, u32, u32)>,
    raw: *mut c_void,
    player: Option<NativePlayer>,
    path: PathBuf,
    generation: u64,
    target: Time,
    rendered: bool,
    used: u64,
    last_correction: Instant,
    playing: bool,
    require_target: bool,
    frame_size: [i32; 2],
    image_bytes: usize,
}
struct ScrubTexture {
    raw: *mut c_void,
    frame: Arc<Frame>,
    path: PathBuf,
    used: u64,
}
pub struct Renderer {
    graphics: Option<Graphics>,
    raw: *mut c_void,
    sources: HashMap<u64, Source>,
    epoch: u64,
    seeks: u64,
    opens: u64,
    scrubbing: bool,
    scrub_cache: Option<ScrubCache>,
    scrub_textures: HashMap<u64, ScrubTexture>,
    dirty: bool,
    last_layout: Option<(Vec<(u64, Transform)>, [u32; 2], Option<u64>)>,
    // Raw EGL contexts cannot move between threads.
    _thread: std::marker::PhantomData<std::rc::Rc<()>>,
}
#[derive(Clone, Default, Debug)]
pub struct Diagnostics {
    pub generation: u64,
    pub ready: bool,
    pub frames: u64,
    pub seeks: u64,
    pub opens: u64,
    pub redrawn: bool,
    pub error: Option<String>,
    pub decoders: Vec<String>,
    pub positions: Vec<(u64, f64, bool, bool)>,
    pub scrub_status: String,
}
impl Renderer {
    pub fn new(window: u64, width: u32, height: u32) -> Result<Self> {
        let library = CString::new(
            crate::native_player::resolve_mpv_library()
                .map_err(anyhow::Error::msg)?
                .as_os_str()
                .as_encoded_bytes(),
        )?;
        let mut error = [0i8; 512];
        let raw = unsafe {
            slicer_gl_open(
                window,
                width as i32,
                height as i32,
                library.as_ptr(),
                error.as_mut_ptr(),
                512,
            )
        };
        if raw.is_null() {
            bail!(
                "{}",
                unsafe { CStr::from_ptr(error.as_ptr()) }.to_string_lossy()
            );
        }
        Ok(Self {
            graphics: None,
            raw,
            sources: HashMap::new(),
            epoch: 0,
            seeks: 0,
            opens: 0,
            last_layout: None,
            scrubbing: false,
            scrub_cache: None,
            scrub_textures: HashMap::new(),
            dirty: true,
            _thread: Default::default(),
        })
    }
    fn remove(&mut self, id: u64) {
        if let Some(source) = self.sources.remove(&id) {
            unsafe { slicer_gl_source_free(self.raw, source.raw) };
            drop(source);
        }
    }
    fn source(&mut self, clip: &Clip, project: &Project, initial_time: Time) -> Result<()> {
        let (id, path, still) = (clip.id, &clip.path, clip.still);
        let graphic = clip.graphic.as_ref().map(|g| {
            (
                g.clone(),
                (project.width as f32 * clip.transform.width)
                    .ceil()
                    .clamp(1., 4096.) as u32,
                (project.height as f32 * clip.transform.height)
                    .ceil()
                    .clamp(1., 4096.) as u32,
                project.height,
            )
        });
        if self
            .sources
            .get(&id)
            .is_some_and(|s| s.path == *path && s.graphic == graphic)
        {
            return Ok(());
        }
        self.remove(id);
        let player = if still {
            None
        } else {
            Some(NativePlayer::new_for_render().map_err(anyhow::Error::msg)?)
        };
        // Decode images before allocating the GL object, so failures cannot leak resources.
        let image = if let Some((graphic, w, h, project_height)) = &graphic {
            Some(self.graphics.get_or_insert_with(Graphics::default).render(
                graphic,
                *w,
                *h,
                *project_height,
            ))
        } else if still {
            Some(image::open(path)?.into_rgba8())
        } else {
            None
        };
        let image_bytes = image.as_ref().map_or(0, |i| i.as_raw().len());
        let mut error = [0i8; 512];
        let raw = unsafe {
            slicer_gl_source(
                self.raw,
                player
                    .as_ref()
                    .map_or(std::ptr::null_mut(), |p| p.render_handle()),
                error.as_mut_ptr(),
                512,
            )
        };
        if raw.is_null() {
            bail!(
                "{}",
                unsafe { CStr::from_ptr(error.as_ptr()) }.to_string_lossy()
            );
        }
        if let Some(image) = image {
            unsafe {
                slicer_gl_image(
                    raw,
                    image.width() as i32,
                    image.height() as i32,
                    image.as_ptr(),
                )
            };
        }
        if let Some(p) = &player {
            if let Err(e) = p.load_at(path, initial_time as f64 / 1e6) {
                unsafe { slicer_gl_source_free(self.raw, raw) };
                bail!(e);
            }
        }
        self.opens += 1;
        self.sources.insert(
            id,
            Source {
                graphic,
                raw,
                player,
                path: path.into(),
                generation: u64::MAX,
                target: initial_time,
                rendered: still,
                used: self.epoch,
                last_correction: Instant::now(),
                playing: false,
                require_target: true,
                frame_size: [1, 1],
                image_bytes,
            },
        );
        self.dirty = true;
        Ok(())
    }
    /// Prepare at most one missing neighboring clip per idle pass. Existing
    /// cached positions are preserved; warming never seeks an active player.
    pub fn prewarm(&mut self, project: &Project, time: Time, size: [u32; 2]) -> Result<()> {
        let mut nearby: Vec<_> = project
            .tracks
            .iter()
            .filter(|t| !t.hidden)
            .flat_map(|t| &t.clips)
            .filter(|c| c.visual && c.source_time(time).is_none())
            .map(|c| {
                (
                    if time < c.start {
                        c.start - time
                    } else {
                        time - c.end()
                    },
                    c,
                )
            })
            .collect();
        nearby.sort_by_key(|(distance, _)| *distance);
        let candidates: Vec<_> = nearby.into_iter().take(2).map(|(_, c)| c).collect();
        for clip in &candidates {
            if let Some(s) = self.sources.get_mut(&clip.id) {
                s.used = self.epoch.saturating_sub(1);
            }
        }
        if let Some(clip) = candidates
            .into_iter()
            .find(|c| !self.sources.get(&c.id).is_some_and(|s| s.path == c.path))
        {
            let source_time = if clip.still {
                0
            } else if time >= clip.end() {
                clip.source_in + (clip.duration - 1).max(0)
            } else {
                clip.source_in
            };
            self.source(clip, project, source_time)?;
            let s = self.sources.get_mut(&clip.id).unwrap();
            s.used = self.epoch.saturating_sub(1);
            s.frame_size = [
                (size[0] as f32 * clip.transform.width)
                    .ceil()
                    .clamp(1., 8192.) as i32,
                (size[1] as f32 * clip.transform.height)
                    .ceil()
                    .clamp(1., 8192.) as i32,
            ];
        }
        Ok(())
    }
    pub fn set_scrubbing(&mut self, scrubbing: bool) {
        self.dirty |= self.scrubbing != scrubbing;
        self.scrubbing = scrubbing;
    }
    /// Export renderers never enable the approximation/cache path.
    pub fn enable_scrub_cache(&mut self) {
        self.scrub_cache.get_or_insert_with(ScrubCache::new);
    }

    /// Used only by the offline HDR proxy worker, never by timeline playback.
    pub(super) fn step_preview(&mut self, time: Time) -> Result<()> {
        if let Some(source) = self.sources.get_mut(&1) {
            if let Some(player) = &source.player {
                let snapshot = player.snapshot();
                let fps = snapshot.estimated_vf_fps.unwrap_or(30.);
                let delta = time as f64 / 1e6 - snapshot.position;
                let frames = (delta * fps).round();
                if snapshot.loaded && !snapshot.seeking && frames >= 1. && frames <= 4. {
                    player
                        .step_preview(frames as u32)
                        .map_err(anyhow::Error::msg)?;
                    source.generation = time as u64;
                    source.target = time;
                    source.rendered = false;
                    source.require_target = true;
                }
            }
        }
        Ok(())
    }

    pub(super) fn preview_at_target(&self, time: Time) -> bool {
        self.sources
            .get(&1)
            .and_then(|s| s.player.as_ref())
            .is_some_and(|p| {
                let snapshot = p.snapshot();
                snapshot.paused
                    && (snapshot.position - time as f64 / 1e6).abs()
                        <= 0.51 / snapshot.estimated_vf_fps.unwrap_or(30.)
            })
    }
    /// Render a requested project time. Playing sessions advance normally; scrubs use coalesced seeks.
    pub fn render(
        &mut self,
        project: &Project,
        time: Time,
        generation: u64,
        playing: bool,
        size: [u32; 2],
        selected: Option<u64>,
    ) -> Result<Diagnostics> {
        self.epoch += 1;
        let mut layers = vec![];
        let mut draw_sources = HashMap::new();
        let mut changed_pixels = false;
        let mut status = Diagnostics {
            generation,
            ready: true,
            ..Default::default()
        };
        if let Some(cache) = &self.scrub_cache {
            let mut sources: Vec<_> = project
                .media
                .iter()
                .chain(project.tracks.iter().flat_map(|t| &t.clips))
                .filter(|c| c.visual && !c.still)
                .map(|c| c.path.clone())
                .collect();
            sources.sort();
            sources.dedup();
            cache.prepare(sources);
            cache.request(if playing {
                vec![]
            } else {
                project
                    .active(time)
                    .filter(|(t, c, _)| !t.hidden && c.visual && !c.still)
                    .map(|(_, c, time)| Target {
                        path: c.path.clone(),
                        time,
                    })
                    .collect()
            });
            status.scrub_status = cache.status();
        }
        for (track, clip, source_time) in project.active(time) {
            if track.hidden || !clip.visual {
                continue;
            }
            if self.scrubbing {
                if let Some(cache) = &self.scrub_cache {
                    if !clip.still && !cache.failed(&clip.path) {
                        if let Some(frame) = cache.frame(&clip.path, source_time) {
                            let mut error = [0i8; 512];
                            if !self.scrub_textures.contains_key(&clip.id) {
                                let raw = unsafe {
                                    slicer_gl_source(
                                        self.raw,
                                        std::ptr::null_mut(),
                                        error.as_mut_ptr(),
                                        512,
                                    )
                                };
                                if raw.is_null() {
                                    bail!("Cannot allocate scrub texture");
                                }
                                unsafe {
                                    slicer_gl_image(
                                        raw,
                                        frame.width as i32,
                                        frame.height as i32,
                                        frame.pixels.as_ptr(),
                                    )
                                };
                                self.scrub_textures.insert(
                                    clip.id,
                                    ScrubTexture {
                                        raw,
                                        frame: frame.clone(),
                                        path: clip.path.clone(),
                                        used: self.epoch,
                                    },
                                );
                                changed_pixels = true;
                            }
                            let texture = self.scrub_textures.get_mut(&clip.id).unwrap();
                            if !Arc::ptr_eq(&texture.frame, &frame) || texture.path != clip.path {
                                unsafe {
                                    slicer_gl_image(
                                        texture.raw,
                                        frame.width as i32,
                                        frame.height as i32,
                                        frame.pixels.as_ptr(),
                                    )
                                };
                                texture.frame = frame.clone();
                                texture.path = clip.path.clone();
                                changed_pixels = true;
                            }
                            texture.used = self.epoch;
                            draw_sources.insert(clip.id, texture.raw);
                            let mut transform = clip.transform.clone();
                            transform.opacity *= clip.fade(time);
                            layers.push((clip.id, transform));
                            status.ready &= source_time >= frame.pts
                                && source_time < frame.pts + frame.duration;
                            status
                                .positions
                                .push((clip.id, frame.pts as f64 / 1e6, false, true));
                        } else {
                            status.ready = false;
                        }
                        continue;
                    }
                }
            }
            self.source(clip, project, source_time)?;
            let s = self.sources.get_mut(&clip.id).unwrap();
            s.used = self.epoch;
            if let Some(p) = &s.player {
                let snapshot = p.snapshot();
                if let Some(error) = snapshot.error {
                    bail!("{}: {error}", clip.path.display());
                }
                let changed = s.generation != generation;
                // Correct small clock drift with bounded rate changes, never periodic seeks.
                if playing
                    && snapshot.loaded
                    && !snapshot.seeking
                    && s.last_correction.elapsed() > Duration::from_millis(250)
                {
                    let delta = source_time as f64 / 1e6 - snapshot.position;
                    let speed = if delta.abs() < 0.025 {
                        1.
                    } else {
                        (1. + delta * 0.25).clamp(0.95, 1.05)
                    };
                    p.set_clock_speed(speed).map_err(anyhow::Error::msg)?;
                    s.last_correction = Instant::now();
                }
                if snapshot.loaded && s.playing != playing {
                    p.set_paused(!playing).map_err(anyhow::Error::msg)?;
                    s.playing = playing;
                    s.require_target = false;
                    p.set_clock_speed(1.).map_err(anyhow::Error::msg)?;
                }
                if changed && snapshot.loaded {
                    // NativePlayer uses exact decoding for both drag and release;
                    // the flag changes scheduling only. Releasing at the same target
                    // must not restart the decoder a second time.
                    if s.target != source_time {
                        p.seek(source_time as f64 / 1e6, !self.scrubbing)
                            .map_err(anyhow::Error::msg)?;
                        self.seeks += 1;
                        s.rendered = false;
                        s.require_target = true;
                        s.last_correction = Instant::now();
                    }
                    s.generation = generation;
                    s.target = source_time;
                }
                let t = &clip.transform;
                // Ask mpv to scale at the actual display footprint. Images retain native pixels.
                let w = (size[0] as f32 * t.width).ceil().clamp(1., 8192.) as i32;
                let h = (size[1] as f32 * t.height).ceil().clamp(1., 8192.) as i32;
                s.frame_size = [w, h];
                let result = unsafe { slicer_gl_video(self.raw, s.raw, w, h) };
                if result < 0 {
                    bail!("libmpv render failed ({result})");
                }
                let snapshot = p.snapshot();
                if result > 0 {
                    changed_pixels = true;
                    s.rendered = true;
                }
                let frame_period = 1.0 / snapshot.estimated_vf_fps.unwrap_or(30.).max(1.);
                let at_target = playing
                    || !s.require_target
                    || (snapshot.position - source_time as f64 / 1e6).abs() <= frame_period * 1.1;
                status.ready &= s.rendered && snapshot.loaded && !snapshot.seeking && at_target;
                status
                    .positions
                    .push((clip.id, snapshot.position, snapshot.seeking, s.rendered));
                status.decoders.push(format!(
                    "{}: {}",
                    clip.id,
                    snapshot.hwdec.unwrap_or("software".into())
                ));
            }
            draw_sources.insert(clip.id, s.raw);
            let mut transform = clip.transform.clone();
            transform.opacity *= clip.fade(time);
            layers.push((clip.id, transform));
        }
        // Textures are cheap to re-upload from the bounded CPU cache. Retain
        // only this scene so cuts cannot grow GPU memory or expose stale clips.
        let unused: Vec<_> = self
            .scrub_textures
            .iter()
            .filter(|(_, s)| s.used != self.epoch)
            .map(|(id, _)| *id)
            .collect();
        for id in unused {
            let texture = self.scrub_textures.remove(&id).unwrap();
            unsafe { slicer_gl_source_free(self.raw, texture.raw) };
        }
        for source in self.sources.values_mut() {
            if source.used != self.epoch {
                if source.player.is_some() {
                    let updated = unsafe {
                        slicer_gl_video(
                            self.raw,
                            source.raw,
                            source.frame_size[0],
                            source.frame_size[1],
                        )
                    };
                    if updated > 0 {
                        source.rendered = true;
                    }
                }
                if let Some(p) = &source.player {
                    if source.playing {
                        p.set_paused(true).map_err(anyhow::Error::msg)?;
                        source.playing = false;
                        source.generation = u64::MAX;
                        source.target = -1;
                    }
                }
            }
        }
        // Keep a bounded working set of nearby/recent clips across gaps and cuts.
        let mut inactive: Vec<_> = self
            .sources
            .iter()
            .filter(|(_, s)| s.used != self.epoch)
            .map(|(id, s)| (*id, s.used))
            .collect();
        inactive.sort_by_key(|(_, used)| std::cmp::Reverse(*used));
        let mut videos = 0;
        let mut image_bytes = 0;
        for (id, _) in inactive {
            let source = &self.sources[&id];
            let evict = if source.player.is_some() {
                videos += 1;
                videos > 4
            } else {
                // Image textures do not own decoder/GPU player sessions. A
                // four-source limit repeatedly decoded entire image collages
                // when scrubbing back over their shared boundary.
                image_bytes += source.image_bytes;
                image_bytes > 256 * 1024 * 1024
            };
            if evict {
                self.remove(id);
            }
        }
        let layout = (layers.clone(), size, selected);
        status.seeks = self.seeks;
        status.opens = self.opens;
        self.dirty |= changed_pixels || self.last_layout.as_ref() != Some(&layout);
        // Paused seeks publish only a coherent set of source frames. Keep the last
        // completed canvas visible while any source is still seeking.
        if (!status.ready && !(self.scrubbing && self.scrub_cache.is_some())) || !self.dirty {
            return Ok(status);
        }
        self.dirty = false;
        self.last_layout = Some(layout);
        status.redrawn = true;
        if unsafe { slicer_gl_begin(self.raw, size[0] as i32, size[1] as i32) } < 0 {
            bail!("OpenGL composition target unavailable");
        }
        for (id, t) in layers {
            let params = transform(&t);
            unsafe {
                slicer_gl_draw(
                    self.raw,
                    draw_sources[&id],
                    params.as_ptr(),
                    (selected == Some(id)) as i32,
                )
            };
        }
        unsafe { slicer_gl_finish(self.raw) };
        status.seeks = self.seeks;
        status.opens = self.opens;
        Ok(status)
    }
    pub fn present(&self) -> Result<()> {
        if unsafe { slicer_gl_present(self.raw) } < 0 {
            bail!("EGL canvas presentation failed");
        }
        Ok(())
    }
    pub fn read_pixels(&self, size: [u32; 2]) -> Vec<u8> {
        let stride = size[0] as usize * 4;
        let mut raw = vec![0; stride * size[1] as usize];
        unsafe { slicer_gl_read(self.raw, raw.as_mut_ptr()) };
        let mut pixels = vec![0; raw.len()];
        for (src, dst) in raw
            .chunks_exact(stride)
            .zip(pixels.chunks_exact_mut(stride).rev())
        {
            dst.copy_from_slice(src);
        }
        pixels
    }
}
fn transform(t: &Transform) -> [f32; 6] {
    [
        t.x,
        t.y,
        t.width,
        t.height,
        t.rotation.to_radians(),
        t.opacity,
    ]
}
impl Drop for Renderer {
    fn drop(&mut self) {
        self.scrub_cache.take();
        for (_, texture) in self.scrub_textures.drain() {
            unsafe { slicer_gl_source_free(self.raw, texture.raw) };
        }
        let ids: Vec<_> = self.sources.keys().copied().collect();
        for id in ids {
            self.remove(id);
        }
        unsafe { slicer_gl_close(self.raw) };
    }
}
#[derive(Clone)]
struct Request {
    project: Arc<Project>,
    time: Time,
    generation: u64,
    playing: bool,
    size: [u32; 2],
    selected: Option<u64>,
    revision: u64,
    scrubbing: bool,
}
impl Request {
    // Coalescing is safe only within the same visible scene. Crossing a cut,
    // overlap, image boundary, or gap must supersede the old sample immediately.
    fn same_scene(&self, other: &Self) -> bool {
        self.size == other.size
            && self.selected == other.selected
            && self.playing == other.playing
            && (Arc::ptr_eq(&self.project, &other.project) || *self.project == *other.project)
            && self
                .project
                .active(self.time)
                .filter(|(t, c, _)| !t.hidden && c.visual)
                .map(|(_, c, _)| c.id)
                .eq(other
                    .project
                    .active(other.time)
                    .filter(|(t, c, _)| !t.hidden && c.visual)
                    .map(|(_, c, _)| c.id))
    }

    fn can_publish_for(&self, latest: &Self) -> bool {
        self.revision == latest.revision
            || (self.scrubbing && latest.scrubbing && self.same_scene(latest))
    }
}
struct State {
    request: Option<Request>,
    stopped: bool,
    status: Diagnostics,
}
pub struct Preview {
    shared: Arc<(Mutex<State>, Condvar)>,
    worker: Option<std::thread::JoinHandle<()>>,
}
impl Preview {
    pub fn new(window: u64) -> Self {
        let shared = Arc::new((
            Mutex::new(State {
                request: None,
                stopped: false,
                status: Diagnostics::default(),
            }),
            Condvar::new(),
        ));
        let state = shared.clone();
        let worker = std::thread::spawn(move || {
            let result = (|| -> Result<()> {
                let mut renderer = Renderer::new(window, 1, 1)?;
                renderer.enable_scrub_cache();
                let mut frames = 0;
                let mut previous = 0;
                loop {
                    let request = {
                        let (lock, wake) = &*state;
                        let mut s = lock.lock().unwrap();
                        while !s.stopped && s.request.is_none() {
                            s = wake.wait(s).unwrap();
                        }
                        if s.stopped {
                            break;
                        }
                        s.request.clone().unwrap()
                    };
                    renderer.set_scrubbing(request.scrubbing);
                    let mut status = renderer.render(
                        &request.project,
                        request.time,
                        request.generation,
                        request.playing,
                        request.size,
                        request.selected,
                    )?;
                    let completed = status.ready;
                    // Drag requests always consume the latest target. Cached layers
                    // may update independently; release restores precise composition.
                    // Do not display an obsolete seek result if the UI replaced it during rendering.
                    let current = state
                        .0
                        .lock()
                        .unwrap()
                        .request
                        .as_ref()
                        .is_some_and(|r| request.can_publish_for(r));
                    if current {
                        if status.redrawn {
                            renderer.present()?;
                            frames += 1;
                        }
                        status.frames = frames;
                        state.0.lock().unwrap().status = status.clone();
                    } else if status.redrawn {
                        // Composition changed the back buffer without presenting it.
                        // A subsequent request must redraw even if its layout matches.
                        renderer.dirty = true;
                    }
                    if completed && !request.playing && !request.scrubbing {
                        let unchanged = state
                            .0
                            .lock()
                            .unwrap()
                            .request
                            .as_ref()
                            .is_some_and(|r| r.revision == request.revision);
                        if unchanged {
                            // A failed speculative load must not take down the active preview.
                            let _ = renderer.prewarm(&request.project, request.time, request.size);
                        }
                    }
                    let (lock, wake) = &*state;
                    let s = lock.lock().unwrap();
                    if s.stopped {
                        break;
                    }
                    let newer = completed
                        && s.request
                            .as_ref()
                            .is_some_and(|r| r.revision != request.revision);
                    if !newer {
                        let _ = wake
                            .wait_timeout(
                                s,
                                if request.playing || !status.ready || previous != request.revision
                                {
                                    Duration::from_millis(8)
                                } else {
                                    Duration::from_millis(100)
                                },
                            )
                            .unwrap();
                    }
                    previous = request.revision;
                }
                Ok(())
            })();
            if let Err(e) = result {
                state.0.lock().unwrap().status.error = Some(e.to_string());
            }
        });
        Self {
            shared,
            worker: Some(worker),
        }
    }
    pub fn request(
        &self,
        project: Arc<Project>,
        time: Time,
        generation: u64,
        playing: bool,
        size: [u32; 2],
        selected: Option<u64>,
        scrubbing: bool,
    ) {
        let mut s = self.shared.0.lock().unwrap();
        if s.request.as_ref().is_some_and(|r| {
            r.time == time
                && r.generation == generation
                && r.playing == playing
                && r.size == size
                && r.selected == selected
                && r.scrubbing == scrubbing
                && *r.project == *project
        }) {
            return;
        }
        let revision = s.request.as_ref().map_or(1, |r| r.revision + 1);
        s.request = Some(Request {
            project,
            time,
            generation,
            playing,
            size,
            selected,
            revision,
            scrubbing,
        });
        self.shared.1.notify_all();
    }
    pub fn status(&self) -> Diagnostics {
        self.shared.0.lock().unwrap().status.clone()
    }
}
impl Drop for Preview {
    fn drop(&mut self) {
        self.shared.0.lock().unwrap().stopped = true;
        self.shared.1.notify_all();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

#[cfg(test)]
mod scheduler_tests {
    use super::*;
    use crate::engine::project::{Clip, Track};

    fn request(time: Time) -> Request {
        let mut project = Project::default();
        project.tracks = [
            (1, 0, 4_000_000, true),
            (2, 1_750_000, 3_000_000, false),
            (3, 3_590_000, 4_000_000, false),
        ]
        .into_iter()
        .map(|(id, start, duration, still)| {
            let mut track = Track::new("Layer");
            track.clips.push(Clip {
                id,
                path: "fixture".into(),
                start,
                duration,
                source_in: 0,
                source_duration: duration,
                visual: true,
                audio: false,
                still,
                transform: Transform::default(),
                gain: 1.,
                graphic: None,
                fade_in: 0,
                fade_out: 0,
            });
            track
        })
        .collect();
        Request {
            project: Arc::new(project),
            time,
            generation: time as u64,
            playing: false,
            size: [320, 180],
            selected: None,
            revision: time as u64,
            scrubbing: true,
        }
    }

    #[test]
    fn every_staggered_boundary_preempts_unfinished_scrub_in_both_directions() {
        for boundary in [1_750_000, 3_590_000, 4_000_000, 4_750_000, 7_590_000] {
            let before = request(boundary - 1);
            let after = request(boundary);
            for (old, latest) in [(&before, &after), (&after, &before)] {
                assert!(!old.can_publish_for(latest));
            }
        }
    }

    #[test]
    fn completed_samples_may_publish_only_for_the_same_drag_scene() {
        let old = request(2_000_000);
        let mut latest = request(2_100_000);
        assert!(old.can_publish_for(&latest));
        latest.scrubbing = false;
        assert!(!old.can_publish_for(&latest));
        latest.scrubbing = true;
        Arc::make_mut(&mut latest.project).tracks[0].hidden = true;
        assert!(!old.can_publish_for(&latest));
    }
}

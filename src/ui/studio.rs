//! Multitrack workspace. The legacy trimmer remains a separate screen during migration.
use super::*;
mod editing;
mod panels;
use gpui_kit::assets::IconName;
use panels::{MediaDrag, panel_sizes};
use slicer::engine::{
    decoder::Decoder,
    gl_canvas::Preview,
    playback::Engine,
    project::{Clip, History, Project, SECOND, Time, Track, Transform},
};

fn clip_height(clip: &Clip) -> f32 {
    if clip.visual {
        66.
    } else {
        17. + if clip.audio { 18. } else { 0. }
    }
}
fn row_height(track: &Track) -> f32 {
    track
        .clips
        .iter()
        .map(|c| clip_height(c) + 4.)
        .fold(52., f32::max)
}
fn row_centers(project: &Project) -> Vec<f32> {
    let mut top = 0.;
    project
        .tracks
        .iter()
        .map(|track| {
            let h = row_height(track);
            let center = top + h / 2.;
            top += h + 4.;
            center
        })
        .collect()
}

fn fitted_zoom(duration: Time, width: f32) -> f32 {
    (width.max(1.) / (duration.max(SECOND) as f32 / SECOND as f32)).min(600.)
}
fn ruler_step(zoom: f32) -> f64 {
    let target = 80. / zoom as f64;
    let magnitude = 10_f64.powf(target.log10().floor());
    [1., 2., 5., 10.]
        .into_iter()
        .map(|factor| factor * magnitude)
        .find(|step| *step >= target)
        .unwrap()
}
fn snapped_start(project: &Project, clip: &Clip, start: Time, playhead: Time, zoom: f32) -> Time {
    let tolerance = (8. / zoom * SECOND as f32) as Time;
    let targets = [0, playhead].into_iter().chain(
        project
            .tracks
            .iter()
            .flat_map(|track| &track.clips)
            .filter(|other| other.id != clip.id)
            .flat_map(|other| [other.start, other.end()]),
    );
    targets
        .flat_map(|target| [target, target - clip.duration])
        .filter(|candidate| *candidate >= 0 && (*candidate - start).abs() < tolerance)
        .min_by_key(|candidate| (*candidate - start).abs())
        .unwrap_or(start)
}

pub(super) struct Studio {
    project: Project,
    history: History,
    engine: Engine,
    preview: Option<Preview>,
    surface: Option<native_surface::NativeSurface>,
    generation: u64,
    selected: Option<u64>,
    selection: std::collections::BTreeSet<u64>,
    clipboard: Vec<(usize, Clip)>,
    inputs: Option<editing::StudioInputs>,
    canvas_settings: bool,
    shortcuts_open: bool,
    refresh_inputs: bool,
    form_error: Option<String>,
    playing: bool,
    time: Time,
    anchor: (Instant, Time),
    bounds: Option<Bounds<Pixels>>,
    timeline: Option<Bounds<Pixels>>,
    track_scroll: ScrollHandle,
    workspace: Option<Bounds<Pixels>>,
    panel_sizes: [f32; 3],
    thumbnails: super::timeline_thumbnails::Thumbnails,
    waveforms: super::timeline_waveforms::Waveforms,
    snapping: bool,
    drag: Option<Drag>,
    zoom: f32,
    offset: Time,
    status: String,
    preview_status: String,
    pending: Option<mpsc::Receiver<Event>>,
    export: Option<slicer::engine::export::ExportHandle>,
    saved: Option<PathBuf>,
    dirty: bool,
    autosave_at: Instant,
    autosave_rx: Option<mpsc::Receiver<Result<(), String>>>,
}
enum Event {
    Imported(Vec<Result<Clip, String>>),
    Loaded(Result<(Project, PathBuf), String>),
    Saved(Result<PathBuf, String>),
    ExportPath(Option<PathBuf>),
}
#[derive(Clone)]
enum Drag {
    Pan {
        start: Point<Pixels>,
        offset: Time,
    },
    Panel {
        index: usize,
        start: Point<Pixels>,
        sizes: [f32; 3],
    },
    Canvas {
        start: Point<Pixels>,
        original: Transform,
        resize: bool,
    },
    Clip {
        start: Point<Pixels>,
        original: Clip,
        track: usize,
        centers: Vec<f32>,
        edge: i32,
        others: Vec<(usize, Clip)>,
    },
    Seek,
}
impl Studio {
    fn thumbnail_width(&self, clip: &Clip) -> f32 {
        // Bin transforms describe the source fitted to the import canvas.
        // They do not change when a timeline instance is cut or resized.
        let aspect = self
            .project
            .media
            .iter()
            .find(|asset| asset.path == clip.path)
            .map(|asset| asset.transform.width / asset.transform.height * (1920. / 1080.))
            .unwrap_or(16. / 9.);
        ((if clip.audio { 31. } else { 49. }) * aspect).clamp(20., 120.)
    }
    fn new() -> Self {
        Self {
            project: Project::default(),
            history: History::default(),
            engine: Engine::new(),
            preview: None,
            surface: None,
            generation: 0,
            selected: None,
            selection: Default::default(),
            clipboard: Vec::new(),
            inputs: None,
            canvas_settings: false,
            shortcuts_open: false,
            refresh_inputs: true,
            form_error: None,
            playing: false,
            time: 0,
            anchor: (Instant::now(), 0),
            bounds: None,
            timeline: None,
            track_scroll: ScrollHandle::new(),
            workspace: None,
            panel_sizes: [220., 240., 290.],
            thumbnails: super::timeline_thumbnails::Thumbnails::new(),
            waveforms: super::timeline_waveforms::Waveforms::new(),
            snapping: true,
            drag: None,
            zoom: 60.,
            offset: 0,
            status: "Import videos, images, and audio to begin".into(),
            preview_status: String::new(),
            pending: None,
            export: None,
            saved: None,
            dirty: false,
            autosave_at: Instant::now(),
            autosave_rx: None,
        }
    }
    fn sync(&mut self) {
        self.sync_transport(true);
    }
    fn sync_transport(&mut self, seek: bool) {
        self.time = self.time.clamp(0, self.project.duration());
        self.anchor = (Instant::now(), self.time);
        if seek {
            self.generation = self.generation.wrapping_add(1);
        }
        let audio = self
            .project
            .tracks
            .iter()
            .any(|t| !t.muted && t.clips.iter().any(|c| c.audio));
        self.engine.transport(
            Arc::new(self.project.clone()),
            self.time,
            self.playing && audio,
        );
    }
    fn checkpoint(&mut self) {
        self.dirty = true;
        self.history.checkpoint(&self.project);
    }
    fn pause(&mut self) {
        self.playing = false;
        self.sync_transport(false);
    }
    fn toggle(&mut self) {
        if self.project.duration() == 0 {
            return;
        }
        let restart = self.time >= self.project.duration();
        if restart {
            self.time = 0;
        }
        self.playing = !self.playing;
        self.sync_transport(restart);
    }
    fn seek(&mut self, t: Time) {
        self.time = t.clamp(0, (self.project.duration() - 1).max(0));
        self.playing = false;
        self.sync();
    }
    fn poll(&mut self) {
        let width = self
            .timeline
            .map_or(600., |b| f32::from(b.size.width) - 100.);
        let mut keys: Vec<_> = self
            .project
            .tracks
            .iter()
            .flat_map(|t| &t.clips)
            .flat_map(|c| {
                super::timeline_thumbnails::tiles(
                    c,
                    self.offset,
                    self.zoom,
                    width,
                    self.thumbnail_width(c),
                )
            })
            .map(|tile| tile.key)
            .collect();
        keys.extend(
            self.project
                .media
                .iter()
                .filter(|c| c.visual)
                .take(128)
                .map(|c| super::timeline_thumbnails::poster(&c.path)),
        );
        self.thumbnails.update(keys);
        self.waveforms.update(
            self.project
                .tracks
                .iter()
                .flat_map(|t| &t.clips)
                .filter(|c| c.audio)
                .map(|c| (c.path.clone(), c.source_duration as f64 / SECOND as f64))
                .collect(),
        );

        if let Some(result) = self.autosave_rx.as_ref().and_then(|rx| rx.try_recv().ok()) {
            self.autosave_rx = None;
            if let Err(e) = result {
                self.status = format!("Autosave failed: {e}");
                self.dirty = true;
            }
        }
        if self.dirty
            && self.drag.is_none()
            && self.autosave_rx.is_none()
            && self.autosave_at.elapsed() > Duration::from_secs(2)
        {
            let path = self
                .saved
                .as_ref()
                .map(|p| p.with_extension("autosave.slicer"))
                .or_else(|| {
                    let root = std::env::var_os("XDG_STATE_HOME")
                        .map(PathBuf::from)
                        .or_else(|| {
                            std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/state"))
                        })?;
                    Some(
                        root.join("slicer")
                            .join(format!("recovery-{}.slicer", std::process::id())),
                    )
                });
            if let Some(path) = path {
                let project = self.project.clone();
                let (tx, rx) = mpsc::channel();
                self.autosave_rx = Some(rx);
                self.dirty = false;
                self.autosave_at = Instant::now();
                thread::spawn(move || {
                    let result = (|| -> anyhow::Result<()> {
                        std::fs::create_dir_all(path.parent().unwrap())?;
                        project.save(&path)
                    })();
                    let _ = tx.send(result.map_err(|e| e.to_string()));
                });
            }
        }

        if let Some(e) = self.pending.as_ref().and_then(|r| r.try_recv().ok()) {
            self.pending = None;
            match e {
                Event::Imported(clips) => {
                    self.checkpoint();
                    let mut errors = vec![];
                    for c in clips {
                        match c {
                            Ok(c) => self.project.add_media(c),
                            Err(e) => errors.push(e),
                        }
                    }
                    self.status = if errors.is_empty() {
                        "Media added to Files".into()
                    } else {
                        errors.join("; ")
                    };
                    self.sync();
                }
                Event::Loaded(result) => match result {
                    Ok((p, path)) => {
                        self.checkpoint();
                        self.project = p;
                        self.project.populate_media();
                        self.saved = Some(path);
                        self.selected = None;
                        self.selection.clear();
                        self.refresh_inputs = true;
                        self.time = 0;
                        self.pause();
                        self.status = "Project opened".into();
                    }
                    Err(e) => self.status = e,
                },
                Event::Saved(result) => match result {
                    Ok(p) => {
                        self.status = format!("Saved {}", p.display());
                        self.saved = Some(p);
                    }
                    Err(e) => self.status = e,
                },
                Event::ExportPath(path) => {
                    if let Some(path) = path {
                        match slicer::engine::export::ExportHandle::spawn(
                            self.project.clone(),
                            path,
                        ) {
                            Ok(h) => {
                                self.export = Some(h);
                                self.status = "Exporting…".into();
                            }
                            Err(e) => self.status = e.to_string(),
                        }
                    }
                }
            }
        }
        if let Some(h) = self.export.as_ref() {
            if let Ok(event) = h.events.try_recv() {
                match event {
                    slicer::engine::export::ExportEvent::Progress(p) => {
                        self.status = format!("Exporting {:.0}%", p * 100.)
                    }
                    slicer::engine::export::ExportEvent::Finished(result) => {
                        self.status = match result {
                            Ok(path) => format!("Exported {}", path.display()),
                            Err(e) => e,
                        };
                        self.export = None;
                    }
                }
            }
        }
        if self.playing {
            self.time = self
                .engine
                .audio_clock()
                .unwrap_or_else(|| self.anchor.1 + self.anchor.0.elapsed().as_micros() as i64)
                .max(self.time)
                .min(self.project.duration());
            if self.time >= self.project.duration() {
                self.playing = false;
                self.sync();
            }
        }
        if let Some(error) = self.engine.audio_error() {
            if self.playing {
                self.status = error;
            }
        }
    }
    fn import(&mut self, paths: Vec<PathBuf>) {
        if self.pending.is_some() {
            return;
        }
        let (tx, rx) = mpsc::channel();
        self.pending = Some(rx);
        self.status = "Loading media…".into();
        thread::spawn(move || {
            let clips = paths
                .into_iter()
                .map(|path| import_clip(path).map_err(|e| e.to_string()))
                .collect();
            let _ = tx.send(Event::Imported(clips));
        });
    }
    fn import_dialog(&mut self) {
        if self.pending.is_some() {
            return;
        }
        let (tx, rx) = mpsc::channel();
        self.pending = Some(rx);
        thread::spawn(move || {
            let paths = rfd::FileDialog::new()
                .set_title("Import video, image, or audio")
                .pick_files()
                .unwrap_or_default();
            let _ = tx.send(Event::Imported(
                paths
                    .into_iter()
                    .map(|p| import_clip(p).map_err(|e| e.to_string()))
                    .collect(),
            ));
        });
    }
    fn save(&mut self) {
        if self.pending.is_some() {
            return;
        }
        let project = self.project.clone();
        let path = self.saved.clone();
        let (tx, rx) = mpsc::channel();
        self.pending = Some(rx);
        thread::spawn(move || {
            let path = path.or_else(|| {
                rfd::FileDialog::new()
                    .add_filter("Slicer project", &["slicer"])
                    .set_file_name("Untitled.slicer")
                    .save_file()
            });
            let result = path
                .ok_or_else(|| "Save cancelled".to_string())
                .and_then(|p| project.save(&p).map(|_| p).map_err(|e| e.to_string()));
            let _ = tx.send(Event::Saved(result));
        });
    }
    fn load(&mut self) {
        if self.pending.is_some() {
            return;
        }
        let (tx, rx) = mpsc::channel();
        self.pending = Some(rx);
        thread::spawn(move || {
            let result = rfd::FileDialog::new()
                .add_filter("Slicer project", &["slicer"])
                .pick_file()
                .ok_or_else(|| "Open cancelled".to_string())
                .and_then(|p| Project::load(&p).map(|x| (x, p)).map_err(|e| e.to_string()));
            let _ = tx.send(Event::Loaded(result));
        });
    }
    fn export_dialog(&mut self) {
        if let Some(h) = &self.export {
            h.cancel();
            return;
        }
        if self.pending.is_some() || self.project.duration() == 0 {
            return;
        }
        let (tx, rx) = mpsc::channel();
        self.pending = Some(rx);
        thread::spawn(move || {
            let p = rfd::FileDialog::new()
                .set_file_name("Timeline.mp4")
                .add_filter("MP4", &["mp4"])
                .save_file();
            let _ = tx.send(Event::ExportPath(p));
        });
    }
    fn delete(&mut self) {
        self.delete_selection(false);
    }
    fn split(&mut self) {
        let ids: Vec<_> = self
            .selection()
            .into_iter()
            .filter(|id| {
                self.project.editable(*id)
                    && self
                        .project
                        .clip(*id)
                        .is_some_and(|c| self.time > c.start && self.time < c.end())
            })
            .collect();
        if ids.is_empty() {
            return;
        }
        self.checkpoint();
        for id in ids {
            self.project.split(id, self.time);
        }
        self.refresh_inputs = true;
        self.sync();
    }
    fn trim_at_playhead(&mut self, keep_after: bool) {
        let Some(id) = self.selected.filter(|id| self.project.editable(*id)) else {
            return;
        };
        let Some(c) = self.project.clip(id) else {
            return;
        };
        if self.time <= c.start || self.time >= c.end() {
            return;
        }
        self.checkpoint();
        let c = self.project.clip_mut(id).unwrap();
        if keep_after {
            let delta = self.time - c.start;
            c.start = self.time;
            c.duration -= delta;
            if !c.still {
                c.source_in += delta;
            }
        } else {
            c.duration = self.time - c.start;
        }
        self.refresh_inputs = true;
        self.sync();
    }
    fn duplicate(&mut self) {
        let old = self.clipboard.clone();
        let end = self
            .selection()
            .iter()
            .filter_map(|id| self.project.clip(*id))
            .map(Clip::end)
            .max();
        self.copy_selection();
        if let Some(end) = end {
            self.paste(end);
        }
        self.clipboard = old;
    }
}
fn import_clip(path: PathBuf) -> anyhow::Result<Clip> {
    let path = std::fs::canonicalize(path)?;
    let still = path.extension().and_then(|s| s.to_str()).is_some_and(|x| {
        ["png", "jpg", "jpeg", "webp", "bmp", "tif", "tiff"]
            .contains(&x.to_ascii_lowercase().as_str())
    });
    let decoder = Decoder::open(&path, false).or_else(|_| Decoder::open(&path, true))?;
    let info = decoder.info;
    let duration = if still { 5 * SECOND } else { info.duration_us };
    if duration <= 0 {
        anyhow::bail!("Media has no finite duration");
    }
    let mut transform = Transform::default();
    if info.width > 0 && info.height > 0 {
        let aspect = info.width as f32 / info.height as f32;
        let canvas = 1920. / 1080.;
        if aspect > canvas {
            transform.height = canvas / aspect;
        } else {
            transform.width = aspect / canvas;
        }
    }
    Ok(Clip {
        id: 0,
        path,
        start: 0,
        source_in: 0,
        duration,
        source_duration: duration,
        visual: info.video != 0,
        audio: info.audio != 0 && !still,
        still,
        transform,
        gain: 1.,
        graphic: None,
        fade_in: 0,
        fade_out: 0,
    })
}
impl SlicerApp {
    pub(super) fn studio_open_project(&mut self, path: PathBuf) {
        self.enter_studio();
        let (tx, rx) = mpsc::channel();
        self.studio.as_mut().unwrap().pending = Some(rx);
        thread::spawn(move || {
            let result = Project::load(&path)
                .map(|p| (p, path))
                .map_err(|e| e.to_string());
            let _ = tx.send(Event::Loaded(result));
        });
    }

    pub(super) fn enter_studio(&mut self) {
        self.native.pause();
        self.native.hide();
        self.export_modal = false;
        self.crop.open = false;
        if self.studio.is_none() {
            self.studio = Some(Studio::new());
        }
        self.screen = Screen::Studio;
    }
    pub(super) fn studio_import(&mut self, paths: Vec<PathBuf>) {
        self.enter_studio();
        self.studio.as_mut().unwrap().import(paths);
    }
    pub(super) fn studio_pause(&mut self) {
        if let Some(s) = self.studio.as_mut() {
            s.pause();
            s.preview = None;
            if let Some(surface) = &mut s.surface {
                surface.hide();
            }
        }
    }
    pub(super) fn studio_drag(
        &mut self,
        position: Point<Pixels>,
        pressed: bool,
        cx: &mut Context<Self>,
    ) {
        let Some(s) = self.studio.as_mut() else {
            return;
        };
        if !pressed {
            if let Some(drag) = s.drag.take() {
                if matches!(drag, Drag::Clip { .. }) {
                    s.refresh_inputs = true;
                }
                if !matches!(drag, Drag::Panel { .. } | Drag::Pan { .. }) {
                    s.sync();
                }
                cx.notify();
            }
            return;
        }
        match s.drag.clone() {
            Some(Drag::Pan { start, offset }) => {
                s.offset = (offset
                    - (f32::from(position.x - start.x) / s.zoom * SECOND as f32) as Time)
                    .clamp(0, s.project.duration());
            }
            Some(Drag::Panel {
                index,
                start,
                sizes,
            }) => {
                let delta = if index == 2 {
                    f32::from(start.y - position.y)
                } else if index == 1 {
                    f32::from(start.x - position.x)
                } else {
                    f32::from(position.x - start.x)
                };
                let mut requested = sizes;
                requested[index] += delta;
                let bounds = s.workspace.unwrap_or_default();
                s.panel_sizes = panel_sizes(
                    f32::from(bounds.size.width),
                    f32::from(bounds.size.height),
                    requested,
                );
            }
            Some(Drag::Canvas {
                start,
                original,
                resize,
            }) => {
                if let (Some(bounds), Some(id)) = (s.bounds, s.selected) {
                    let dx = f32::from(position.x - start.x) / f32::from(bounds.size.width);
                    let dy = f32::from(position.y - start.y) / f32::from(bounds.size.height);
                    if let Some(c) = s.project.clip_mut(id) {
                        c.transform = original.clone();
                        if resize {
                            let factor =
                                (1. + dx / original.width + dy / original.height).max(0.02);
                            c.transform.width = original.width * factor;
                            c.transform.height = original.height * factor;
                        } else {
                            c.transform.x += dx;
                            c.transform.y += dy;
                        }
                    }
                }
            }
            Some(Drag::Clip {
                start,
                original,
                track,
                centers,
                edge,
                others,
            }) => {
                let delta = (f32::from(position.x - start.x) / s.zoom * SECOND as f32) as i64;
                if edge == 0 {
                    let mut time = (original.start + delta).max(0);
                    if s.snapping {
                        time = snapped_start(&s.project, &original, time, s.time, s.zoom);
                    }
                    let y = centers[track] + f32::from(position.y - start.y);
                    let to = centers
                        .iter()
                        .enumerate()
                        .min_by(|(_, a), (_, b)| (*a - y).abs().total_cmp(&(*b - y).abs()))
                        .map_or(track, |(i, _)| i);
                    let ids: Vec<_> = std::iter::once(original.id)
                        .chain(others.iter().map(|(_, c)| c.id))
                        .collect();
                    let shift = to as isize - track as isize;
                    let can_move = std::iter::once(track)
                        .chain(others.iter().map(|(t, _)| *t))
                        .all(|t| {
                            let target = t as isize + shift;
                            target >= 0
                                && s.project
                                    .tracks
                                    .get(target as usize)
                                    .is_some_and(|t| !t.locked)
                        });
                    if can_move {
                        let min = others
                            .iter()
                            .map(|(_, c)| c.start)
                            .chain([original.start])
                            .min()
                            .unwrap();
                        let delta = (time - original.start).max(-min);
                        s.project.move_clip(original.id, to, original.start + delta);
                        for (t, c) in others {
                            s.project.move_clip(
                                c.id,
                                (t as isize + shift) as usize,
                                c.start + delta,
                            );
                        }
                        s.selection = ids.into_iter().collect();
                    }
                } else if let Some(c) = s.project.clip_mut(original.id) {
                    if edge < 0 {
                        let min = if c.still {
                            -original.start
                        } else {
                            (-original.source_in).max(-original.start)
                        };
                        let d = delta.clamp(min, original.duration - original.duration.min(1000));
                        c.start = original.start + d;
                        c.source_in = if c.still { 0 } else { original.source_in + d };
                        c.duration = original.duration - d;
                    } else {
                        let max = if c.still {
                            24 * 3600 * SECOND
                        } else {
                            original.source_duration - original.source_in
                        };
                        c.duration =
                            (original.duration + delta).clamp(original.duration.min(1000), max);
                    }
                }
                s.sync();
            }
            Some(Drag::Seek) => {
                if let Some(b) = s.timeline {
                    let x = f32::from(position.x - b.origin.x) - 100.;
                    s.seek(s.offset + (x / s.zoom * SECOND as f32) as i64);
                }
            }
            None => {}
        }
        cx.notify();
    }
    fn studio_canvas_down(&mut self, event: &MouseDownEvent, cx: &mut Context<Self>) {
        let s = self.studio.as_mut().unwrap();
        let Some(b) = s.bounds else {
            return;
        };
        let x = f32::from(event.position.x - b.origin.x) / f32::from(b.size.width);
        let y = f32::from(event.position.y - b.origin.y) / f32::from(b.size.height);
        let selected = s
            .project
            .active(s.time)
            .filter(|(t, c, _)| !t.hidden && !t.locked && c.visual)
            .filter(|(_, c, _)| {
                let t = &c.transform;
                let angle = -t.rotation.to_radians();
                let dx = (x - t.x) * f32::from(b.size.width);
                let dy = (y - t.y) * f32::from(b.size.height);
                let rx = dx * angle.cos() - dy * angle.sin();
                let ry = dx * angle.sin() + dy * angle.cos();
                rx.abs() <= t.width * f32::from(b.size.width) / 2.
                    && ry.abs() <= t.height * f32::from(b.size.height) / 2.
            })
            .last()
            .map(|(_, c, _)| c.id);
        s.selected = selected;
        s.selection.clear();
        s.canvas_settings = false;
        s.shortcuts_open = false;
        if let Some(id) = selected {
            let original = s.project.clip(id).unwrap().transform.clone();
            let resize = event.modifiers.shift
                || (x - (original.x + original.width / 2.)).abs() < 0.025
                    && (y - (original.y + original.height / 2.)).abs() < 0.025;
            s.checkpoint();
            s.drag = Some(Drag::Canvas {
                start: event.position,
                original,
                resize,
            });
        }
        cx.notify();
    }
    fn studio_timeline_down(
        &mut self,
        track: usize,
        event: &MouseDownEvent,
        cx: &mut Context<Self>,
    ) {
        let s = self.studio.as_mut().unwrap();
        let Some(b) = s.timeline else {
            return;
        };
        let x = f32::from(event.position.x - b.origin.x) - 100.;
        let t = s.offset + (x / s.zoom * SECOND as f32) as i64;
        let clip = s.project.tracks[track]
            .clips
            .iter()
            .rev()
            .find(|c| c.start <= t && c.end() > t)
            .cloned();
        if let Some(c) = clip {
            s.canvas_settings = false;
            s.shortcuts_open = false;
            if event.modifiers.control || event.modifiers.platform {
                let mut selection = s.selection();
                if !selection.insert(c.id) {
                    selection.remove(&c.id);
                }
                s.selection = selection;
                s.selected = s.selection.iter().next().copied();
                cx.notify();
                return;
            } else if event.modifiers.shift {
                let a = s
                    .selected
                    .and_then(|id| s.project.clip(id))
                    .map_or(c.start, |c| c.start);
                s.selection = s.project.tracks[track]
                    .clips
                    .iter()
                    .filter(|other| other.start >= a.min(c.start) && other.start <= a.max(c.start))
                    .map(|c| c.id)
                    .collect();
            } else if !s.selection().contains(&c.id) {
                s.selection.clear();
            }
            s.selected = Some(c.id);
            if !s.project.tracks[track].locked {
                let start = (c.start - s.offset) as f32 / SECOND as f32 * s.zoom;
                let end = (c.end() - s.offset) as f32 / SECOND as f32 * s.zoom;
                let edge = if x - start < 8. {
                    -1
                } else if end - x < 8. {
                    1
                } else {
                    0
                };
                s.checkpoint();
                s.pause();
                let ids = s.selection();
                let others = s
                    .project
                    .tracks
                    .iter()
                    .enumerate()
                    .filter(|(_, t)| !t.locked)
                    .flat_map(|(index, t)| {
                        t.clips
                            .iter()
                            .filter(|other| other.id != c.id && ids.contains(&other.id))
                            .cloned()
                            .map(move |other| (index, other))
                    })
                    .collect();
                s.drag = Some(Drag::Clip {
                    others,
                    start: event.position,
                    original: c,
                    track,
                    centers: row_centers(&s.project),
                    edge,
                });
            }
        } else {
            s.seek(t);
            s.drag = Some(Drag::Seek);
        }
        cx.notify();
    }
    pub(super) fn studio_title_tools(&self, cx: &mut Context<Self>) -> AnyElement {
        let s = self.studio.as_ref().unwrap();
        // Only buttons consume title-bar gestures; the space between groups
        // still moves the window and supports double-click to maximize.
        let group = |id| {
            h_flex()
                .id(id)
                .gap_1()
                .occlude()
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_double_click(|_, _, cx| cx.stop_propagation())
        };
        let mut left = group("studio-title-files");
        for (id, label, icon, action) in [
            ("studio-home", "Home", IconName::House, 0),
            ("studio-open", "Open project", IconName::FolderOpen, 1),
            ("studio-save", "Save project", IconName::Save, 2),
        ] {
            left = left.child(
                Button::new(id)
                    .ghost()
                    .compact()
                    .icon(icon)
                    .tooltip(label)
                    .accessibility_label(label)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        match action {
                            0 => this.show_home(),
                            1 => this.studio.as_mut().unwrap().load(),
                            _ => this.studio.as_mut().unwrap().save(),
                        }
                        cx.notify();
                    })),
            );
        }
        let export_label = if s.export.is_some() {
            "Cancel export"
        } else {
            "Export"
        };
        let right = group("studio-title-actions")
            .child(
                Button::new("studio-export")
                    .ghost()
                    .compact()
                    .icon(if s.export.is_some() {
                        IconName::X
                    } else {
                        IconName::Upload
                    })
                    .tooltip(export_label)
                    .accessibility_label(export_label)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.studio.as_mut().unwrap().export_dialog();
                        cx.notify();
                    })),
            )
            .child(
                Button::new("studio-settings")
                    .ghost()
                    .compact()
                    .icon(IconName::Settings)
                    .tooltip("Settings")
                    .accessibility_label("Settings")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.studio_pause();
                        this.screen = Screen::Settings;
                        cx.notify();
                    })),
            );
        h_flex()
            .w_full()
            .h_full()
            .pr_2()
            .child(left)
            .child(div().flex_1())
            .child(right)
            .into_any_element()
    }

    pub(super) fn studio_view(&self, cx: &mut Context<Self>) -> AnyElement {
        let s = self.studio.as_ref().unwrap();
        let workspace_size = s.workspace.map_or(size(px(1260.), px(700.)), |b| b.size);
        let sizes = panel_sizes(
            f32::from(workspace_size.width),
            f32::from(workspace_size.height),
            s.panel_sizes,
        );
        let mut edit_tools = h_flex()
            .gap_1()
            .child(
                Button::new("add-text")
                    .ghost()
                    .compact()
                    .icon(IconName::Type)
                    .tooltip("Add text (T)")
                    .accessibility_label("Add text")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.studio.as_mut().unwrap().add_graphic(true);
                        cx.notify();
                    })),
            )
            .child(
                Button::new("add-background")
                    .ghost()
                    .compact()
                    .icon(IconName::Square)
                    .tooltip("Add color background")
                    .accessibility_label("Add color background")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.studio.as_mut().unwrap().add_graphic(false);
                        cx.notify();
                    })),
            );
        let can_edit = s.selected.is_some_and(|id| s.project.editable(id));
        let can_cut = s
            .selected
            .and_then(|id| s.project.clip(id))
            .is_some_and(|c| can_edit && s.time > c.start && s.time < c.end());
        for (id, label, icon, action) in [
            ("undo", "Undo", IconName::Undo2, 3),
            ("redo", "Redo", IconName::Redo2, 4),
            ("split", "Split at playhead", IconName::Scissors, 5),
            ("duplicate", "Duplicate", IconName::Copy, 6),
            ("delete", "Delete", IconName::Trash, 7),
            ("add-track", "Add track", IconName::ListPlus, 8),
        ] {
            let button = Button::new(id)
                .compact()
                .ghost()
                .icon(icon)
                .tooltip(label)
                .accessibility_label(label)
                .disabled(match action {
                    5 => !can_cut,
                    6 | 7 => !can_edit,
                    _ => false,
                })
                .on_click(cx.listener(move |this, _, _, cx| {
                    let s = this.studio.as_mut().unwrap();
                    match action {
                        3 => {
                            s.history.undo(&mut s.project);
                            s.refresh_inputs = true;
                            s.dirty = true;
                            s.sync();
                        }
                        4 => {
                            s.history.redo(&mut s.project);
                            s.refresh_inputs = true;
                            s.dirty = true;
                            s.sync();
                        }
                        5 => s.split(),
                        6 => s.duplicate(),
                        7 => s.delete(),
                        8 => {
                            s.checkpoint();
                            s.project
                                .tracks
                                .push(Track::new(&format!("Track {}", s.project.tracks.len() + 1)));
                        }
                        _ => {}
                    }
                    cx.notify();
                }));
            if (3..=8).contains(&action) {
                if action == 5 || action == 8 {
                    edit_tools =
                        edit_tools.child(div().h(px(18.)).w(px(1.)).mx_1().bg(ink(BORDER)));
                }
                edit_tools = edit_tools.child(button);
                if action == 5 {
                    for (id, label, icon, after) in [
                        (
                            "trim-start",
                            "Trim start to playhead",
                            IconName::ArrowRightToLine,
                            true,
                        ),
                        (
                            "trim-end",
                            "Trim end to playhead",
                            IconName::ArrowLeftToLine,
                            false,
                        ),
                    ] {
                        edit_tools = edit_tools.child(
                            Button::new(id)
                                .ghost()
                                .compact()
                                .icon(icon)
                                .tooltip(label)
                                .accessibility_label(label)
                                .disabled(!can_cut)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.studio.as_mut().unwrap().trim_at_playhead(after);
                                    cx.notify();
                                })),
                        );
                    }
                }
            }
        }
        edit_tools = edit_tools
            .child(div().h(px(18.)).w(px(1.)).mx_1().bg(ink(BORDER)))
            .child(
                Button::new("timeline-snapping")
                    .ghost()
                    .compact()
                    .icon(IconName::Magnet)
                    .tooltip("Snap clips to edges and playhead")
                    .accessibility_label("Timeline snapping")
                    .selected(s.snapping)
                    .toggled(s.snapping)
                    .on_click(cx.listener(|this, _, _, cx| {
                        let s = this.studio.as_mut().unwrap();
                        s.snapping = !s.snapping;
                        cx.notify();
                    })),
            );

        edit_tools = edit_tools.child(
            Button::new("timeline-shortcuts-button")
                .ghost()
                .compact()
                .icon(IconName::Keyboard)
                .selected(s.shortcuts_open)
                .tooltip("Timeline keyboard and mouse controls")
                .accessibility_label("Timeline keyboard and mouse controls")
                .on_click(cx.listener(|this, _, window, cx| {
                    let s = this.studio.as_mut().unwrap();
                    s.shortcuts_open = !s.shortcuts_open;
                    this.focus_handle.focus(window, cx);
                    cx.notify();
                })),
        );
        let owner = cx.entity().downgrade();
        let project_aspect = s.project.width as f32 / s.project.height as f32;
        let preview = canvas(
            move |bounds, window, cx| {
                let mut b = bounds;
                let aspect = project_aspect;
                let w = (f32::from(bounds.size.width) - 32.).max(1.);
                let h = (f32::from(bounds.size.height) - 32.).max(1.);
                let fit_w = w.min(h * aspect);
                let fit_h = fit_w / aspect;
                b.origin.x += px(16. + (w - fit_w) / 2.);
                b.origin.y += px(16. + (h - fit_h) / 2.);
                b.size = size(px(fit_w), px(fit_h));
                let _ = owner.update(cx, |this, _| {
                    if let Some(s) = this.studio.as_mut() {
                        s.bounds = Some(b);
                        if let Some(surface) = &mut s.surface {
                            if let Err(error) = surface.update(b, window.scale_factor(), true) {
                                s.status = error.to_string();
                            }
                        }
                    }
                });
                b
            },
            move |_, b, window, _| {
                window.paint_quad(fill(
                    Bounds::new(
                        b.origin - point(px(1.), px(1.)),
                        b.size + size(px(2.), px(2.)),
                    ),
                    ink(0x777777ff),
                ));
                window.paint_quad(fill(b, ink(0x000000ff)));
            },
        )
        .size_full();
        let center = h_flex()
            .flex_1()
            .min_h(px(0.))
            .min_w(px(0.))
            .overflow_hidden()
            .child(
                div()
                    .flex_1()
                    .h_full()
                    .bg(ink(0x242424ff))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, event, window, cx| {
                            this.focus_handle.focus(window, cx);
                            this.studio_canvas_down(event, cx)
                        }),
                    )
                    .child(preview),
            );
        let controls = h_flex()
            .absolute()
            .inset_0()
            .items_center()
            .justify_center()
            .gap_3()
            .child(
                Button::new("studio-start")
                    .ghost()
                    .rounded_full()
                    .icon(IconName::SkipBack)
                    .tooltip("Go to clip start")
                    .accessibility_label("Go to clip start")
                    .on_click(cx.listener(|this, _, _, cx| {
                        let s = this.studio.as_mut().unwrap();
                        let start = s
                            .selected
                            .and_then(|id| s.project.clip(id))
                            .map_or(0, |c| c.start);
                        s.seek(start);
                        cx.notify();
                    })),
            )
            .child(
                Button::new("studio-play")
                    .tooltip(if s.playing { "Pause" } else { "Play" })
                    .accessibility_label(if s.playing { "Pause" } else { "Play" })
                    .secondary()
                    .rounded_full()
                    .icon(if s.playing {
                        gpui_kit::assets::IconName::Pause
                    } else {
                        gpui_kit::assets::IconName::Play
                    })
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.studio.as_mut().unwrap().toggle();
                        cx.notify();
                    })),
            )
            .child(
                Button::new("studio-end")
                    .ghost()
                    .rounded_full()
                    .icon(IconName::SkipForward)
                    .tooltip("Go to clip end")
                    .accessibility_label("Go to clip end")
                    .on_click(cx.listener(|this, _, _, cx| {
                        let s = this.studio.as_mut().unwrap();
                        let end = s
                            .selected
                            .and_then(|id| s.project.clip(id))
                            .map_or(s.project.duration(), |c| c.end());
                        s.seek((end - 1).max(0));
                        cx.notify();
                    })),
            );
        let mut zoom_controls = h_flex().items_center();
        for (id, label, icon, action) in [
            (
                "earlier",
                "Scroll timeline earlier",
                IconName::ChevronLeft,
                0,
            ),
            ("later", "Scroll timeline later", IconName::ChevronRight, 1),
            ("zoom-out", "Zoom out", IconName::ZoomOut, 2),
            ("zoom-in", "Zoom in", IconName::ZoomIn, 3),
            ("zoom-fit", "Fit entire timeline", IconName::Scan, 4),
        ] {
            zoom_controls = zoom_controls.child(
                Button::new(id)
                    .ghost()
                    .compact()
                    .icon(icon)
                    .tooltip(label)
                    .accessibility_label(label)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        let s = this.studio.as_mut().unwrap();
                        match action {
                            0 => s.offset = (s.offset - 5 * SECOND).max(0),
                            1 => s.offset += 5 * SECOND,
                            2 => {
                                let width =
                                    s.timeline.map_or(600., |b| f32::from(b.size.width) - 120.);
                                s.zoom = (s.zoom / 1.4)
                                    .max(fitted_zoom(s.project.duration(), width) / 16.);
                            }
                            3 => s.zoom = (s.zoom * 1.4).min(600.),
                            _ => {
                                let width =
                                    s.timeline.map_or(600., |b| f32::from(b.size.width) - 120.);
                                s.zoom = fitted_zoom(s.project.duration(), width);
                                s.offset = 0;
                            }
                        }
                        cx.notify();
                    })),
            );
        }
        let transport = div().relative().w_full().h(px(40.)).child(controls).child(
            div()
                .absolute()
                .left_0()
                .h_full()
                .flex()
                .items_center()
                .text_sm()
                .text_color(ink(MUTED))
                .child(format!(
                    "{} / {}",
                    format_timestamp(s.time as f64 / 1e6),
                    format_timestamp(s.project.duration() as f64 / 1e6)
                )),
        );
        let timeline_tools = h_flex()
            .h(px(34.))
            .flex_shrink_0()
            .px_2()
            .child(edit_tools)
            .child(div().flex_1())
            .child(zoom_controls);
        let owner = cx.entity().downgrade();
        let mut rows = v_flex().gap_1();
        let width = s.timeline.map_or(600., |b| f32::from(b.size.width) - 100.);
        let visible_end = s.offset + (width / s.zoom * SECOND as f32) as i64;
        let step = ruler_step(s.zoom);
        let mut ruler = div()
            .relative()
            .h(px(22.))
            .ml(px(100.))
            .overflow_hidden()
            .text_xs()
            .text_color(ink(MUTED));
        let first = (s.offset as f64 / 1e6 / step).ceil() as i64;
        for i in first..first + ((width as f64 / s.zoom as f64 / step).ceil() as i64 + 1) {
            let seconds = i as f64 * step;
            let x = (seconds - s.offset as f64 / 1e6) * s.zoom as f64;
            ruler = ruler.child(
                div()
                    .absolute()
                    .left(px(x as f32))
                    .child(format_timestamp(seconds)),
            );
        }
        let ruler = ruler.on_mouse_down(
            MouseButton::Left,
            cx.listener(|this, e: &MouseDownEvent, window, cx| {
                this.focus_handle.focus(window, cx);
                let s = this.studio.as_mut().unwrap();
                if let Some(b) = s.timeline {
                    let x = f32::from(e.position.x - b.origin.x) - 100.;
                    s.seek(s.offset + (x / s.zoom * SECOND as f32) as i64);
                    s.drag = Some(Drag::Seek);
                }
                cx.notify();
            }),
        );
        for (i, t) in s.project.tracks.iter().enumerate() {
            let height = row_height(t);
            let mut lane = div()
                .id(("studio-track", i))
                .drag_over::<MediaDrag>(|style, _, _, _| style.bg(ink(0x354347ff)))
                .on_drop(cx.listener(move |this, media: &MediaDrag, window, cx| {
                    let s = this.studio.as_mut().unwrap();
                    if s.project.tracks[i].locked {
                        return;
                    }
                    if let Some(bounds) = s.timeline {
                        let x = f32::from(window.mouse_position().x - bounds.origin.x) - 100.;
                        let time = (s.offset + (x / s.zoom * SECOND as f32) as Time).max(0);
                        s.checkpoint();
                        if let Some(id) = s.project.insert_media(&media.path, i, time) {
                            s.selected = Some(id);
                            s.pause();
                            s.sync();
                        }
                        cx.notify();
                    }
                }))
                .relative()
                .flex_1()
                .h(px(height))
                .overflow_hidden()
                .rounded(px(10.))
                .bg(ink(BG))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, event, window, cx| {
                        this.focus_handle.focus(window, cx);
                        this.studio_timeline_down(i, event, cx)
                    }),
                );
            lane = lane.child(
                div()
                    .absolute()
                    .top(px(2.))
                    .w_full()
                    .h(px(height - 4.))
                    .rounded(px(5.))
                    .bg(ink(SURFACE_RAISED)),
            );
            for c in &t.clips {
                if c.end() < s.offset || c.start > visible_end {
                    continue;
                }
                let x = (c.start - s.offset) as f32 / SECOND as f32 * s.zoom;
                let w = c.duration as f32 / SECOND as f32 * s.zoom;
                let mut clip = div()
                    .absolute()
                    .left(px(x))
                    .top(px(2.))
                    .w(px(w.max(3.)))
                    .h(px(clip_height(c)))
                    .rounded(px(5.))
                    .border_1()
                    .border_color(ink(if s.selection().contains(&c.id) {
                        ACCENT_STRONG
                    } else {
                        0x626262ff
                    }))
                    .bg(ink(if c.visual { 0x343434ff } else { 0x354347ff }))
                    .overflow_hidden();
                for tile in super::timeline_thumbnails::tiles(
                    c,
                    s.offset,
                    s.zoom,
                    width,
                    s.thumbnail_width(c),
                ) {
                    if let Some(image) = s.thumbnails.image(&tile.key) {
                        clip = clip.child(
                            div()
                                .absolute()
                                .left(px(tile.x))
                                .top(px(17.))
                                .w(px(tile.width))
                                .h(px(if c.audio { 31. } else { 49. }))
                                .overflow_hidden()
                                .child(
                                    img(image)
                                        .w(px(s.thumbnail_width(c)))
                                        .h_full()
                                        .object_fit(ObjectFit::Contain),
                                ),
                        );
                    }
                }
                if let Some(graphic) = &c.graphic {
                    let (label, color) = match graphic {
                        slicer::engine::project::Graphic::Text(t) => (t.text.clone(), 0x304257ff),
                        slicer::engine::project::Graphic::Color { color } => {
                            ("Color".into(), u32::from_be_bytes(*color))
                        }
                    };
                    clip = clip.child(
                        div()
                            .absolute()
                            .top(px(17.))
                            .left_0()
                            .right_0()
                            .bottom_0()
                            .bg(ink(color))
                            .px_2()
                            .overflow_hidden()
                            .text_xs()
                            .child(label),
                    );
                }
                clip = clip.child(
                    div()
                        .absolute()
                        .left_0()
                        .right_0()
                        .top_0()
                        .h(px(17.))
                        .px_1()
                        .text_xs()
                        .text_color(ink(TEXT))
                        .bg(ink(0x111111cf))
                        .overflow_hidden()
                        .child(format!("{} · {:.1}s", c.label(), c.duration as f64 / 1e6)),
                );
                if c.audio {
                    let wave = s.waveforms.get(&c.path);
                    let source_in = c.source_in as f64 / SECOND as f64;
                    let visible_left = (-x).max(0.);
                    let visible_width = (w - visible_left).min(width - x.max(0.)).max(0.);
                    let seconds_per_px = 1. / s.zoom as f64;
                    let gain = c.gain;
                    let muted = t.muted;
                    clip = clip.child(
                        canvas(
                            |_, _, _| (),
                            move |bounds, _, window, _| {
                                window.paint_quad(fill(bounds, ink(0x192d30ff)));
                                let middle = bounds.origin.y + bounds.size.height / 2.;
                                window.paint_quad(fill(
                                    Bounds::new(
                                        point(bounds.origin.x, middle),
                                        size(bounds.size.width, px(1.)),
                                    ),
                                    ink(0x365153ff),
                                ));
                                if let Some(wave) = &wave {
                                    let columns =
                                        (visible_width / 2.).ceil().clamp(1., 2048.) as usize;
                                    for column in 0..columns {
                                        let left = visible_width * column as f32 / columns as f32;
                                        let right =
                                            visible_width * (column + 1) as f32 / columns as f32;
                                        let peak = wave.peak_between(
                                            source_in
                                                + (visible_left + left) as f64 * seconds_per_px,
                                            source_in
                                                + (visible_left + right) as f64 * seconds_per_px,
                                        );
                                        let extent = super::timeline::visible_waveform_extent(
                                            peak.min.abs().max(peak.max.abs()) * gain,
                                        );
                                        if extent <= 0. {
                                            continue;
                                        }
                                        let amplitude = px(7. * extent);
                                        window.paint_quad(fill(
                                            Bounds::new(
                                                point(
                                                    bounds.origin.x + px(left),
                                                    middle - amplitude,
                                                ),
                                                size(px((right - left).max(1.)), amplitude * 2.),
                                            ),
                                            ink(if muted { 0x657577ff } else { 0x95c9c9ff }),
                                        ));
                                    }
                                }
                            },
                        )
                        .absolute()
                        .left(px(visible_left))
                        .top(px(if c.visual { 48. } else { 17. }))
                        .w(px(visible_width))
                        .h(px(18.)),
                    );
                }
                lane = lane.child(clip);
            }
            let mut label = v_flex()
                .w(px(96.))
                .flex_shrink_0()
                .child(div().text_xs().child(t.name.clone()));
            let mut buttons = h_flex();
            for (key, tooltip, icon, active, action) in [
                (
                    "mute",
                    if t.muted {
                        "Unmute track"
                    } else {
                        "Mute track"
                    },
                    if t.muted {
                        IconName::VolumeX
                    } else {
                        IconName::Volume2
                    },
                    t.muted,
                    0,
                ),
                (
                    "hide",
                    if t.hidden { "Show track" } else { "Hide track" },
                    if t.hidden {
                        IconName::EyeOff
                    } else {
                        IconName::Eye
                    },
                    t.hidden,
                    1,
                ),
                (
                    "lock",
                    if t.locked {
                        "Unlock track"
                    } else {
                        "Lock track"
                    },
                    if t.locked {
                        IconName::Lock
                    } else {
                        IconName::LockOpen
                    },
                    t.locked,
                    2,
                ),
            ] {
                buttons = buttons.child(
                    Button::new((key, i))
                        .ghost()
                        .compact()
                        .icon(icon)
                        .tooltip(tooltip)
                        .accessibility_label(tooltip)
                        .selected(active)
                        .toggled(active)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            let s = this.studio.as_mut().unwrap();
                            s.checkpoint();
                            let t = &mut s.project.tracks[i];
                            match action {
                                0 => t.muted = !t.muted,
                                1 => t.hidden = !t.hidden,
                                _ => t.locked = !t.locked,
                            }
                            s.sync();
                            cx.notify();
                        })),
                );
            }
            label = label.child(buttons);
            rows = rows.child(h_flex().h(px(height)).gap_1().child(label).child(lane));
        }
        let track_height = (sizes[2] - 62.).max(40.);
        let play_x = (s.time - s.offset) as f32 / SECOND as f32 * s.zoom;
        let timeline = div()
            .id("studio-timeline")
            .on_mouse_down(
                MouseButton::Middle,
                cx.listener(|this, event: &MouseDownEvent, window, cx| {
                    this.focus_handle.focus(window, cx);
                    let s = this.studio.as_mut().unwrap();
                    s.drag = Some(Drag::Pan {
                        start: event.position,
                        offset: s.offset,
                    });
                    cx.stop_propagation();
                    cx.notify();
                }),
            )
            .on_scroll_wheel(cx.listener(|this, event, _, cx| this.studio_scroll(event, cx)))
            .on_pinch(cx.listener(|this, event: &PinchEvent, _, cx| {
                let s = this.studio.as_mut().unwrap();
                let x = s
                    .timeline
                    .map_or(0., |b| f32::from(event.position.x - b.origin.x) - 100.);
                s.zoom_at((1. + event.delta).max(0.1), x);
                cx.stop_propagation();
                cx.notify();
            }))
            .h(px(26. + track_height))
            .flex_shrink_0()
            .relative()
            .child(
                canvas(
                    move |b, _, cx| {
                        let _ = owner.update(cx, |this, _| {
                            this.studio.as_mut().unwrap().timeline = Some(b)
                        });
                    },
                    |_, _, _, _| {},
                )
                .absolute()
                .top_0()
                .left_0()
                .size_full(),
            )
            .child(ruler)
            .child(
                div()
                    .id("studio-track-scroll")
                    .track_scroll(&s.track_scroll)
                    .mt_1()
                    .h(px(track_height))
                    .overflow_y_scroll()
                    .on_scroll_wheel(
                        cx.listener(|this, event, _, cx| this.studio_scroll(event, cx)),
                    )
                    .child(rows),
            )
            .child(
                canvas(
                    |_, _, _| (),
                    move |bounds, _, window, _| {
                        let width = f32::from(bounds.size.width) - 100.;
                        if play_x >= 0. && play_x <= width {
                            let x = bounds.origin.x + px(100. + play_x);
                            window.paint_quad(fill(
                                Bounds::new(
                                    point(x - px(1.), bounds.origin.y + px(6.)),
                                    size(px(2.), bounds.size.height - px(6.)),
                                ),
                                ink(TEXT),
                            ));
                            window.paint_quad(
                                fill(
                                    Bounds::new(
                                        point(x - px(5.), bounds.origin.y + px(2.)),
                                        size(px(10.), px(8.)),
                                    ),
                                    ink(TEXT),
                                )
                                .corner_radii(px(5.)),
                            );
                        }
                    },
                )
                .absolute()
                .top_0()
                .left_0()
                .size_full(),
            );
        let timeline = if play_x >= 0. && play_x <= width {
            timeline.child(
                div()
                    .absolute()
                    .left(px(100. + play_x - 5.))
                    .top_0()
                    .w(px(10.))
                    .h_full()
                    .cursor_pointer()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _, _, cx| {
                            let s = this.studio.as_mut().unwrap();
                            s.pause();
                            s.drag = Some(Drag::Seek);
                            cx.notify();
                        }),
                    ),
            )
        } else {
            timeline
        };
        let preview_panel = v_flex()
            .flex_1()
            .min_w(px(0.))
            .h_full()
            .bg(ink(SURFACE))
            .border_1()
            .border_color(ink(BORDER))
            .rounded(px(WINDOW_RADIUS))
            .overflow_hidden()
            .child(
                h_flex()
                    .h(px(28.))
                    .px_2()
                    .child(
                        div()
                            .flex_1()
                            .text_xs()
                            .text_color(ink(MUTED))
                            .child(format!(
                                "Preview · {} × {}",
                                s.project.width, s.project.height
                            )),
                    )
                    .child(
                        Button::new("canvas-settings")
                            .ghost()
                            .compact()
                            .icon(IconName::Crop)
                            .tooltip("Output size / aspect ratio")
                            .accessibility_label("Output size / aspect ratio")
                            .on_click(cx.listener(|this, _, _, cx| {
                                let s = this.studio.as_mut().unwrap();
                                s.canvas_settings = true;
                                s.shortcuts_open = false;
                                cx.notify();
                            })),
                    ),
            )
            .child(center)
            .child(transport);
        let timeline_panel = v_flex()
            .h(px(sizes[2]))
            .flex_shrink_0()
            .w_full()
            .bg(ink(SURFACE))
            .border_1()
            .border_color(ink(BORDER))
            .rounded(px(WINDOW_RADIUS))
            .overflow_hidden()
            .child(timeline_tools)
            .child(timeline);
        let owner = cx.entity().downgrade();
        let workspace = v_flex()
            .relative()
            .flex_1()
            .min_h(px(0.))
            .w_full()
            .child(
                canvas(
                    move |b, _, cx| {
                        let _ = owner.update(cx, |this, _| {
                            this.studio.as_mut().unwrap().workspace = Some(b)
                        });
                    },
                    |_, _, _, _| {},
                )
                .absolute()
                .top_0()
                .left_0()
                .size_full(),
            )
            .child(
                h_flex()
                    .flex_1()
                    .min_h(px(0.))
                    .w_full()
                    .child(self.studio_files(sizes[0], cx))
                    .child(self.studio_splitter(0, sizes, cx))
                    .child(preview_panel)
                    .child(self.studio_splitter(1, sizes, cx))
                    .child(self.studio_properties(sizes[1], cx)),
            )
            .child(self.studio_splitter(2, sizes, cx))
            .child(timeline_panel);
        v_flex()
            .size_full()
            .p(px(5.))
            .gap(px(5.))
            .child(workspace)
            .into_any_element()
    }

    pub(super) fn poll_studio(&mut self, window: &mut Window) {
        if self.screen == Screen::Studio {
            if let Some(s) = self.studio.as_mut() {
                if s.surface.is_none() {
                    match native_surface::NativeSurface::new(window) {
                        Ok(mut surface) => {
                            let _ = surface.set_corner_radius(0.);
                            s.preview = Some(Preview::new(surface.window_id()));
                            s.surface = Some(surface);
                        }
                        Err(e) => s.status = e.to_string(),
                    }
                }
                if s.preview.is_none() {
                    if let Some(surface) = &s.surface {
                        s.preview = Some(Preview::new(surface.window_id()));
                    }
                }
                s.poll();
                if let (Some(preview), Some(bounds)) = (&s.preview, s.bounds) {
                    let scale = window.scale_factor();
                    let size = [
                        (f32::from(bounds.size.width) * scale).round().max(1.) as u32,
                        (f32::from(bounds.size.height) * scale).round().max(1.) as u32,
                    ];
                    preview.request(
                        Arc::new(s.project.clone()),
                        s.time.min((s.project.duration() - 1).max(0)),
                        s.generation,
                        s.playing,
                        size,
                        s.selected,
                        matches!(s.drag, Some(Drag::Seek) | Some(Drag::Clip { .. })),
                    );
                    let diagnostics = preview.status();
                    s.preview_status = diagnostics.scrub_status;
                    if let Some(error) = diagnostics.error {
                        s.status = error;
                    }
                }
                if s.playing || s.drag.is_some() {
                    window.request_animation_frame();
                }
            }
        }
    }
}

#[cfg(test)]
mod transport_tests {
    use super::{
        Clip, Decoder, Studio, Transform, clip_height, fitted_zoom, row_centers, row_height,
        ruler_step, snapped_start,
    };
    use std::{
        path::PathBuf,
        thread,
        time::{Duration, Instant},
    };
    #[test]
    fn fit_includes_long_projects_and_keeps_ruler_work_bounded() {
        for seconds in [1_i64, 60, 3600, 86400] {
            let zoom = fitted_zoom(seconds * 1_000_000, 960.);
            assert!(seconds as f32 * zoom <= 960.01);
            let step = ruler_step(zoom);
            assert!(step * zoom as f64 >= 79.99);
            assert!(960. / zoom as f64 / step <= 13.);
        }
    }

    #[test]
    fn either_clip_edge_snaps_to_neighbors_and_the_playhead() {
        let mut s = Studio::new();
        let mut clip = Clip {
            id: 1,
            path: PathBuf::from("test.mp4"),
            start: 0,
            source_in: 0,
            duration: 2_000_000,
            source_duration: 10_000_000,
            visual: true,
            audio: false,
            still: false,
            transform: Transform::default(),
            gain: 1.,
            graphic: None,
            fade_in: 0,
            fade_out: 0,
        };
        s.project.tracks[0].clips.push(clip.clone());
        clip.id = 2;
        clip.start = 5_000_000;
        s.project.tracks[1].clips.push(clip);
        let moving = &s.project.tracks[0].clips[0];
        assert_eq!(
            snapped_start(&s.project, moving, 3_050_000, 9_000_000, 60.),
            3_000_000
        );
        assert_eq!(
            snapped_start(&s.project, moving, 7_050_000, 9_000_000, 60.),
            7_000_000
        );
        assert_eq!(
            snapped_start(&s.project, moving, 1_950_000, 4_000_000, 60.),
            2_000_000
        );
        assert_eq!(
            snapped_start(&s.project, moving, 3_400_000, 9_000_000, 60.),
            3_400_000
        );
    }

    #[test]
    fn trim_actions_preserve_source_alignment_and_can_be_undone() {
        let mut s = Studio::new();
        s.project.tracks[0].clips.push(Clip {
            id: 1,
            path: PathBuf::from("test.mp4"),
            start: 2_000_000,
            source_in: 3_000_000,
            duration: 6_000_000,
            source_duration: 12_000_000,
            visual: true,
            audio: false,
            still: false,
            transform: Transform::default(),
            gain: 1.,
            graphic: None,
            fade_in: 0,
            fade_out: 0,
        });
        s.selected = Some(1);
        s.time = 4_000_000;
        s.trim_at_playhead(true);
        let clip = s.project.clip(1).unwrap();
        assert_eq!(
            (clip.start, clip.source_in, clip.duration),
            (4_000_000, 5_000_000, 4_000_000)
        );
        s.history.undo(&mut s.project);
        s.trim_at_playhead(false);
        let clip = s.project.clip(1).unwrap();
        assert_eq!(
            (clip.start, clip.source_in, clip.duration),
            (2_000_000, 3_000_000, 2_000_000)
        );
        s.history.undo(&mut s.project);
        s.project.tracks[0].locked = true;
        let before = s.project.clone();
        s.trim_at_playhead(true);
        assert_eq!(s.project, before);
    }

    #[test]
    fn silent_video_uses_waveform_space_for_larger_thumbnails() {
        let mut s = Studio::new();
        let mut clip = Clip {
            id: 1,
            path: PathBuf::from("test.mp4"),
            start: 0,
            source_in: 0,
            duration: 1_000_000,
            source_duration: 1_000_000,
            visual: true,
            audio: false,
            still: false,
            transform: Transform::default(),
            gain: 1.,
            graphic: None,
            fade_in: 0,
            fade_out: 0,
        };
        assert_eq!(clip_height(&clip), 66.);
        s.project.tracks[0].clips.push(clip.clone());
        clip.audio = true;
        assert_eq!(clip_height(&clip), 66.);
        s.project.tracks[1].clips.push(clip);
        assert_eq!(row_height(&s.project.tracks[0]), 70.);
        assert_eq!(row_height(&s.project.tracks[1]), 70.);
        assert_eq!(row_centers(&s.project), vec![35., 109., 174.]);
    }

    #[test]
    #[ignore = "Requires PulseAudio and SLICER_TRANSPORT_TEST_VIDEO"]
    fn long_pause_does_not_advance_the_resumed_clock() {
        let path = PathBuf::from(std::env::var_os("SLICER_TRANSPORT_TEST_VIDEO").unwrap());
        let d = Decoder::open(&path, false).unwrap();
        let mut s = Studio::new();
        s.project.tracks[0].clips.push(Clip {
            id: 1,
            path,
            start: 0,
            source_in: 0,
            duration: d.info.duration_us,
            source_duration: d.info.duration_us,
            visual: true,
            audio: true,
            still: false,
            transform: Transform::default(),
            gain: 0.,
            graphic: None,
            fade_in: 0,
            fade_out: 0,
        });
        for _ in 0..3 {
            s.toggle();
            let start = Instant::now();
            let initial = s.time;
            while start.elapsed() < Duration::from_millis(450) {
                s.poll();
                thread::sleep(Duration::from_millis(5));
            }
            assert!(
                (s.time - initial) < 700_000,
                "playing clock jumped {}",
                s.time - initial
            );
            s.pause();
            let paused = s.time;
            thread::sleep(Duration::from_secs(1));
            s.poll();
            assert_eq!(s.time, paused);
            s.toggle();
            let start = Instant::now();
            while start.elapsed() < Duration::from_millis(150) {
                s.poll();
                thread::sleep(Duration::from_millis(5));
            }
            assert!(
                s.time - paused < 350_000,
                "resume jumped {}",
                s.time - paused
            );
            s.pause();
        }
    }
}

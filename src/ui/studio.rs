//! Multitrack workspace. The legacy trimmer remains a separate screen during migration.
use super::*;
use slicer::engine::{
    decoder::Decoder,
    gl_canvas::Preview,
    playback::Engine,
    project::{Clip, History, Project, SECOND, Time, Track, Transform},
};

pub(super) struct Studio {
    project: Project,
    history: History,
    engine: Engine,
    preview: Option<Preview>,
    surface: Option<native_surface::NativeSurface>,
    generation: u64,
    selected: Option<u64>,
    playing: bool,
    time: Time,
    anchor: (Instant, Time),
    bounds: Option<Bounds<Pixels>>,
    timeline: Option<Bounds<Pixels>>,
    drag: Option<Drag>,
    zoom: f32,
    offset: Time,
    status: String,
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
    Canvas {
        start: Point<Pixels>,
        original: Transform,
        resize: bool,
    },
    Clip {
        start: Point<Pixels>,
        original: Clip,
        track: usize,
        edge: i32,
    },
    Seek,
}
impl Studio {
    fn new() -> Self {
        Self {
            project: Project::default(),
            history: History::default(),
            engine: Engine::new(),
            preview: None,
            surface: None,
            generation: 0,
            selected: None,
            playing: false,
            time: 0,
            anchor: (Instant::now(), 0),
            bounds: None,
            timeline: None,
            drag: None,
            zoom: 60.,
            offset: 0,
            status: "Import videos, images, and audio to begin".into(),
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
                            Ok(mut c) => {
                                c.id = self.project.next_id;
                                self.project.next_id += 1;
                                c.start = self.time;
                                self.selected = Some(c.id);
                                let track = self
                                    .project
                                    .tracks
                                    .iter()
                                    .position(|t| !t.locked && t.clips.is_empty())
                                    .unwrap_or_else(|| {
                                        self.project.tracks.push(Track::new(&format!(
                                            "Track {}",
                                            self.project.tracks.len() + 1
                                        )));
                                        self.project.tracks.len() - 1
                                    });
                                self.project.tracks[track].clips.push(c);
                            }
                            Err(e) => errors.push(e),
                        }
                    }
                    self.status = if errors.is_empty() {
                        "Drag clips to arrange time; drag objects on the canvas".into()
                    } else {
                        errors.join("; ")
                    };
                    self.sync();
                }
                Event::Loaded(result) => match result {
                    Ok((p, path)) => {
                        self.checkpoint();
                        self.project = p;
                        self.saved = Some(path);
                        self.selected = None;
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
        if let Some(id) = self.selected {
            if self.project.editable(id) {
                self.checkpoint();
                for t in &mut self.project.tracks {
                    t.clips.retain(|c| c.id != id);
                }
                self.selected = None;
                self.sync();
            }
        }
    }
    fn split(&mut self) {
        if let Some(id) = self.selected {
            self.checkpoint();
            self.project.split(id, self.time);
            self.sync();
        }
    }
    fn duplicate(&mut self) {
        if let Some(mut c) = self.selected.and_then(|id| self.project.clip(id)).cloned() {
            self.checkpoint();
            c.id = self.project.next_id;
            self.project.next_id += 1;
            c.start = self.time;
            self.selected = Some(c.id);
            let mut track = Track::new(&format!("Track {}", self.project.tracks.len() + 1));
            track.clips.push(c);
            self.project.tracks.push(track);
            self.sync();
        }
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
    pub(super) fn studio_key(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) {
        let Some(s) = self.studio.as_mut() else {
            return;
        };
        let ctrl = event.keystroke.modifiers.control || event.keystroke.modifiers.platform;
        match event.keystroke.key.as_str() {
            "space" => s.toggle(),
            "delete" | "backspace" => s.delete(),
            "s" if ctrl => s.save(),
            "o" if ctrl => s.load(),
            "z" if ctrl => {
                if event.keystroke.modifiers.shift {
                    s.history.redo(&mut s.project)
                } else {
                    s.history.undo(&mut s.project)
                }
                s.sync();
            }
            "b" => s.split(),
            "left" | "arrowleft" => {
                s.seek(s.time - SECOND * s.project.fps_den as i64 / s.project.fps_num as i64)
            }
            "right" | "arrowright" => {
                s.seek(s.time + SECOND * s.project.fps_den as i64 / s.project.fps_num as i64)
            }
            _ => {}
        }
        cx.notify();
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
            if s.drag.take().is_some() {
                s.sync();
            }
            return;
        }
        match s.drag.clone() {
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
                edge,
            }) => {
                let delta = (f32::from(position.x - start.x) / s.zoom * SECOND as f32) as i64;
                if edge == 0 {
                    let mut time = (original.start + delta).max(0);
                    let snap = (8. / s.zoom * SECOND as f32) as i64;
                    let mut points = vec![s.time, 0];
                    for c in s.project.tracks.iter().flat_map(|t| &t.clips) {
                        if c.id != original.id {
                            points.extend([c.start, c.end()]);
                        }
                    }
                    if let Some(p) = points.into_iter().min_by_key(|p| (p - time).abs()) {
                        if (p - time).abs() < snap {
                            time = p;
                        }
                    }
                    let dy = (f32::from(position.y - start.y) / 52.).round() as isize;
                    let to = (track as isize + dy).clamp(0, s.project.tracks.len() as isize - 1)
                        as usize;
                    s.project.move_clip(original.id, to, time);
                } else if let Some(c) = s.project.clip_mut(original.id) {
                    if edge < 0 {
                        let min = if c.still {
                            -original.start
                        } else {
                            (-original.source_in).max(-original.start)
                        };
                        let d = delta.clamp(min, original.duration - 1000);
                        c.start = original.start + d;
                        c.source_in = if c.still { 0 } else { original.source_in + d };
                        c.duration = original.duration - d;
                    } else {
                        let max = if c.still {
                            24 * 3600 * SECOND
                        } else {
                            original.source_duration - original.source_in
                        };
                        c.duration = (original.duration + delta).clamp(1000, max);
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
                s.playing = false;
                s.drag = Some(Drag::Clip {
                    start: event.position,
                    original: c,
                    track,
                    edge,
                });
            }
        } else {
            s.seek(t);
            s.drag = Some(Drag::Seek);
        }
        cx.notify();
    }
    pub(super) fn studio_view(&self, cx: &mut Context<Self>) -> AnyElement {
        let s = self.studio.as_ref().unwrap();
        let mut tools = h_flex().gap_2().flex_wrap();
        for (id, label, action) in [
            ("import", "Import", 0),
            ("load", "Open project", 1),
            ("save", "Save", 2),
            ("undo", "Undo", 3),
            ("redo", "Redo", 4),
            ("split", "Split", 5),
            ("duplicate", "Duplicate", 6),
            ("delete", "Delete", 7),
            ("add-track", "+ Track", 8),
            (
                "export",
                if s.export.is_some() {
                    "Cancel export"
                } else {
                    "Export MP4"
                },
                9,
            ),
        ] {
            tools = tools.child(Button::new(id).compact().ghost().label(label).on_click(
                cx.listener(move |this, _, _, cx| {
                    let s = this.studio.as_mut().unwrap();
                    match action {
                        0 => s.import_dialog(),
                        1 => s.load(),
                        2 => s.save(),
                        3 => {
                            s.history.undo(&mut s.project);
                            s.dirty = true;
                            s.sync();
                        }
                        4 => {
                            s.history.redo(&mut s.project);
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
                        _ => s.export_dialog(),
                    }
                    cx.notify();
                }),
            ));
        }
        let mut inspector = v_flex()
            .id("studio-inspector")
            .h_full()
            .overflow_y_scroll()
            .w(px(155.))
            .gap_2()
            .p_2()
            .bg(ink(SURFACE))
            .child(div().font_semibold().child("Clip properties"));
        if let Some(c) = s.selected.and_then(|id| s.project.clip(id)) {
            inspector = inspector
                .child(
                    div().text_sm().overflow_hidden().child(
                        c.path
                            .file_name()
                            .unwrap_or_default()
                            .to_string_lossy()
                            .to_string(),
                    ),
                )
                .child(div().text_sm().child(format!(
                    "{}\nStart {:.2}s\nLength {:.2}s",
                    if c.visual { "Video / image" } else { "Audio" },
                    c.start as f64 / 1e6,
                    c.duration as f64 / 1e6
                )));
            for (id, label, action) in [
                ("smaller", "Scale −", 0),
                ("larger", "Scale +", 1),
                ("rotate-left", "Rotate −15°", 2),
                ("rotate-right", "Rotate +15°", 3),
                ("opacity-down", "Opacity −", 4),
                ("opacity-up", "Opacity +", 5),
                ("gain-down", "Volume −", 6),
                ("gain-up", "Volume +", 7),
                ("center", "Center", 8),
            ] {
                inspector =
                    inspector.child(Button::new(id).compact().ghost().label(label).on_click(
                        cx.listener(move |this, _, _, cx| {
                            let s = this.studio.as_mut().unwrap();
                            if let Some(id) = s.selected {
                                if s.project.editable(id) {
                                    s.checkpoint();
                                    if let Some(c) = s.project.clip_mut(id) {
                                        match action {
                                            0 => {
                                                c.transform.width *= 0.9;
                                                c.transform.height *= 0.9;
                                            }
                                            1 => {
                                                c.transform.width *= 1.1;
                                                c.transform.height *= 1.1;
                                            }
                                            2 => c.transform.rotation -= 15.,
                                            3 => c.transform.rotation += 15.,
                                            4 => {
                                                c.transform.opacity =
                                                    (c.transform.opacity - 0.1).max(0.)
                                            }
                                            5 => {
                                                c.transform.opacity =
                                                    (c.transform.opacity + 0.1).min(1.)
                                            }
                                            6 => c.gain = (c.gain - 0.1).max(0.),
                                            7 => c.gain = (c.gain + 0.1).min(4.),
                                            _ => {
                                                c.transform.x = 0.5;
                                                c.transform.y = 0.5;
                                            }
                                        }
                                    }
                                    s.sync();
                                }
                            }
                            cx.notify();
                        }),
                    ));
            }
            inspector = inspector.child(div().text_xs().text_color(ink(MUTED)).child(format!(
                "Opacity {:.0}% · Volume {:.0}%",
                c.transform.opacity * 100.,
                c.gain * 100.
            )));
        } else {
            inspector = inspector.child(
                div()
                    .text_sm()
                    .text_color(ink(MUTED))
                    .child("Select a clip. Drag it on the canvas to move; Shift-drag to scale."),
            );
        }
        let owner = cx.entity().downgrade();
        let project_aspect = s.project.width as f32 / s.project.height as f32;
        let preview = canvas(
            move |bounds, window, cx| {
                let mut b = bounds;
                let aspect = project_aspect;
                let w = f32::from(bounds.size.width);
                let h = f32::from(bounds.size.height);
                let fit_w = w.min(h * aspect);
                let fit_h = fit_w / aspect;
                b.origin.x += px((w - fit_w) / 2.);
                b.origin.y += px((h - fit_h) / 2.);
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
            move |_, _, _, _| {},
        )
        .size_full();
        let center = h_flex()
            .flex_1()
            .min_h(px(100.))
            .gap_2()
            .child(
                div()
                    .flex_1()
                    .h_full()
                    .bg(ink(0x111111ff))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, event, _, cx| this.studio_canvas_down(event, cx)),
                    )
                    .child(preview),
            )
            .child(inspector);
        let transport = h_flex()
            .gap_2()
            .child(
                Button::new("studio-play")
                    .compact()
                    .label(if s.playing { "Pause" } else { "Play" })
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.studio.as_mut().unwrap().toggle();
                        cx.notify();
                    })),
            )
            .child(div().text_sm().child(format!(
                "{:.3} / {:.3}s",
                s.time as f64 / 1e6,
                s.project.duration() as f64 / 1e6
            )));
        let mut transport = transport.child(div().flex_1());
        for (id, label, action) in [
            ("earlier", "←", 0),
            ("later", "→", 1),
            ("zoom-out", "−", 2),
            ("zoom-in", "+", 3),
        ] {
            transport = transport.child(Button::new(id).ghost().compact().label(label).on_click(
                cx.listener(move |this, _, _, cx| {
                    let s = this.studio.as_mut().unwrap();
                    match action {
                        0 => s.offset = (s.offset - 5 * SECOND).max(0),
                        1 => s.offset += 5 * SECOND,
                        2 => s.zoom = (s.zoom / 1.4).max(5.),
                        _ => s.zoom = (s.zoom * 1.4).min(600.),
                    }
                    cx.notify();
                }),
            ));
        }
        let owner = cx.entity().downgrade();
        let mut rows = v_flex().gap_1();
        let width = s.timeline.map_or(600., |b| f32::from(b.size.width) - 100.);
        let visible_end = s.offset + (width / s.zoom * SECOND as f32) as i64;
        let step = if s.zoom >= 120. {
            0.5
        } else if s.zoom >= 40. {
            1.
        } else if s.zoom >= 12. {
            5.
        } else {
            10.
        };
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
                    .child(format!("{seconds:.1}s")),
            );
        }
        rows = rows.child(ruler.on_mouse_down(
            MouseButton::Left,
            cx.listener(|this, e: &MouseDownEvent, _, cx| {
                let s = this.studio.as_mut().unwrap();
                if let Some(b) = s.timeline {
                    let x = f32::from(e.position.x - b.origin.x) - 100.;
                    s.seek(s.offset + (x / s.zoom * SECOND as f32) as i64);
                    s.drag = Some(Drag::Seek);
                }
                cx.notify();
            }),
        ));
        for (i, t) in s.project.tracks.iter().enumerate() {
            let mut lane = div()
                .relative()
                .flex_1()
                .h(px(48.))
                .overflow_hidden()
                .bg(ink(0x20242aff))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, event, _, cx| this.studio_timeline_down(i, event, cx)),
                );
            for c in &t.clips {
                if c.end() < s.offset || c.start > visible_end {
                    continue;
                }
                let x = (c.start - s.offset) as f32 / SECOND as f32 * s.zoom;
                let w = c.duration as f32 / SECOND as f32 * s.zoom;
                lane = lane.child(
                    div()
                        .absolute()
                        .left(px(x))
                        .top(px(3.))
                        .w(px(w.max(3.)))
                        .h(px(42.))
                        .rounded(px(4.))
                        .border_1()
                        .border_color(ink(if s.selected == Some(c.id) {
                            0xffffffff
                        } else {
                            0x52657fff
                        }))
                        .bg(ink(if c.visual { 0x254d73ff } else { 0x276449ff }))
                        .overflow_hidden()
                        .px_2()
                        .text_xs()
                        .child(
                            c.path
                                .file_name()
                                .unwrap_or_default()
                                .to_string_lossy()
                                .to_string(),
                        )
                        .child(
                            div()
                                .text_xs()
                                .child(format!("{:.1}s", c.duration as f64 / 1e6)),
                        ),
                );
            }
            let play_x = (s.time - s.offset) as f32 / SECOND as f32 * s.zoom;
            if play_x >= 0. && play_x <= width {
                lane = lane.child(
                    div()
                        .absolute()
                        .left(px(play_x))
                        .top_0()
                        .w(px(2.))
                        .h_full()
                        .bg(ink(0xffd56aff)),
                );
            }
            let mut label = v_flex()
                .w(px(96.))
                .flex_shrink_0()
                .child(div().text_xs().child(t.name.clone()));
            let mut buttons = h_flex();
            for (key, text, action) in [
                ("mute", if t.muted { "M●" } else { "M" }, 0),
                ("hide", if t.hidden { "H●" } else { "H" }, 1),
                ("lock", if t.locked { "L●" } else { "L" }, 2),
            ] {
                buttons = buttons.child(
                    Button::new((key, i))
                        .ghost()
                        .compact()
                        .label(text)
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
            rows = rows.child(h_flex().h(px(48.)).gap_1().child(label).child(lane));
        }
        let timeline = div()
            .id("studio-timeline")
            .h(px(205.))
            .overflow_y_scroll()
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
                .size_full(),
            )
            .child(rows);
        v_flex()
            .size_full()
            .p_3()
            .gap_2()
            .child(tools)
            .child(center)
            .child(transport)
            .child(timeline)
            .child(
                div()
                    .text_xs()
                    .text_color(ink(MUTED))
                    .child(format!("{}  ·  OpenGL canvas / libmpv video", s.status)),
            )
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
                    if let Some(error) = preview.status().error {
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

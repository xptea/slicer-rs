use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Integer microseconds. Source frame selection still uses its actual PTS.
pub type Time = i64;
pub const SECOND: Time = 1_000_000;
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Transform {
    /// Center in normalized project coordinates; dimensions relative to project size.
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    pub rotation: f32,
    pub opacity: f32,
}
impl Default for Transform {
    fn default() -> Self {
        Self {
            x: 0.5,
            y: 0.5,
            width: 1.,
            height: 1.,
            rotation: 0.,
            opacity: 1.,
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct TextStyle {
    pub text: String,
    pub font: String,
    pub size: f32,
    pub bold: bool,
    pub italic: bool,
    pub align: u8,
    pub color: [u8; 4],
    pub background: [u8; 4],
}
impl Default for TextStyle {
    fn default() -> Self {
        Self {
            text: "Your text".into(),
            font: "Sans".into(),
            size: 72.,
            bold: false,
            italic: false,
            align: 1,
            color: [255; 4],
            background: [0; 4],
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type")]
pub enum Graphic {
    Text(TextStyle),
    Color { color: [u8; 4] },
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Clip {
    pub id: u64,
    pub path: PathBuf,
    pub start: Time,
    pub source_in: Time,
    pub duration: Time,
    pub source_duration: Time,
    pub visual: bool,
    pub audio: bool,
    pub still: bool,
    pub transform: Transform,
    pub gain: f32,
    #[serde(default)]
    pub graphic: Option<Graphic>,
    #[serde(default)]
    pub fade_in: Time,
    #[serde(default)]
    pub fade_out: Time,
}
impl Clip {
    pub fn label(&self) -> String {
        match &self.graphic {
            Some(Graphic::Text(text)) => text
                .text
                .lines()
                .next()
                .filter(|s| !s.is_empty())
                .unwrap_or("Text")
                .chars()
                .take(80)
                .collect(),
            Some(Graphic::Color { .. }) => "Color background".into(),
            None => self
                .path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into(),
        }
    }
    pub fn fade(&self, time: Time) -> f32 {
        let elapsed = (time - self.start).max(0) as f32;
        let remaining = (self.end() - time).max(0) as f32;
        let fade_in = if self.fade_in > 0 {
            (elapsed / self.fade_in as f32).min(1.)
        } else {
            1.
        };
        let fade_out = if self.fade_out > 0 {
            (remaining / self.fade_out as f32).min(1.)
        } else {
            1.
        };
        fade_in.min(fade_out)
    }
    pub fn end(&self) -> Time {
        self.start.saturating_add(self.duration)
    }
    pub fn source_time(&self, time: Time) -> Option<Time> {
        (time >= self.start && time < self.end()).then(|| {
            if self.still {
                0
            } else {
                self.source_in + time - self.start
            }
        })
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Track {
    pub name: String,
    pub muted: bool,
    pub hidden: bool,
    pub locked: bool,
    pub clips: Vec<Clip>,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Project {
    pub version: u32,
    pub width: u32,
    pub height: u32,
    pub fps_num: u32,
    pub fps_den: u32,
    pub tracks: Vec<Track>,
    #[serde(default)]
    pub media: Vec<Clip>,
    pub next_id: u64,
}
impl Default for Project {
    fn default() -> Self {
        Self {
            version: 1,
            width: 1920,
            height: 1080,
            fps_num: 30,
            fps_den: 1,
            tracks: vec![
                Track::new("Video 1"),
                Track::new("Video 2"),
                Track::new("Audio"),
            ],
            media: Vec::new(),
            next_id: 1,
        }
    }
}
impl Track {
    pub fn new(name: &str) -> Self {
        Self {
            name: name.into(),
            muted: false,
            hidden: false,
            locked: false,
            clips: vec![],
        }
    }
}
impl Project {
    pub fn add_graphic(&mut self, graphic: Graphic, start: Time) -> u64 {
        let text = matches!(&graphic, Graphic::Text(_));
        let name = if text { "Text" } else { "Background" };
        let mut track = Track::new(name);
        let id = self.next_id;
        self.next_id += 1;
        track.clips.push(Clip {
            id,
            path: PathBuf::new(),
            start: start.max(0),
            source_in: 0,
            duration: 5 * SECOND,
            source_duration: 5 * SECOND,
            visual: true,
            audio: false,
            still: true,
            transform: if text {
                Transform {
                    width: 0.8,
                    height: 0.3,
                    ..Transform::default()
                }
            } else {
                Transform::default()
            },
            gain: 1.,
            graphic: Some(graphic),
            fade_in: 0,
            fade_out: 0,
        });
        if text {
            self.tracks.push(track);
        } else {
            self.tracks.insert(0, track);
        }
        id
    }
    pub fn resize_canvas(&mut self, width: u32, height: u32) -> Result<()> {
        if width < 2
            || height < 2
            || width > 8192
            || height > 8192
            || width % 2 != 0
            || height % 2 != 0
        {
            bail!("Use even dimensions between 2 and 8192 pixels");
        }
        let ratio = (self.width as f32 / self.height as f32) / (width as f32 / height as f32);
        for c in self.tracks.iter_mut().flat_map(|t| &mut t.clips) {
            c.transform.x = 0.5 + (c.transform.x - 0.5) * ratio;
            c.transform.width *= ratio;
            if matches!(c.graphic, Some(Graphic::Color { .. })) {
                c.transform = Transform::default();
            }
        }
        self.width = width;
        self.height = height;
        Ok(())
    }
    /// Media in the bin is independent of timeline instances.
    pub fn add_media(&mut self, mut clip: Clip) {
        if clip.graphic.is_some() {
            return;
        }
        if self.media.iter().any(|asset| asset.path == clip.path) {
            return;
        }
        clip.id = 0;
        clip.start = 0;
        clip.source_in = 0;
        if !clip.still {
            clip.duration = clip.source_duration;
        }
        // Bin items describe the original source, not one edited instance.
        let scale = clip.transform.width.max(clip.transform.height).max(0.01);
        clip.transform.width /= scale;
        clip.transform.height /= scale;
        clip.transform.x = 0.5;
        clip.transform.y = 0.5;
        clip.transform.rotation = 0.;
        clip.transform.opacity = 1.;
        clip.gain = 1.;
        self.media.push(clip);
    }
    pub fn populate_media(&mut self) {
        let media = std::mem::take(&mut self.media);
        for clip in media {
            self.add_media(clip);
        }
        let clips: Vec<_> = self.tracks.iter().flat_map(|t| &t.clips).cloned().collect();
        for clip in clips {
            self.add_media(clip);
        }
    }
    pub fn insert_media(&mut self, path: &Path, track: usize, start: Time) -> Option<u64> {
        if self.tracks.get(track)?.locked {
            return None;
        }
        let mut clip = self.media.iter().find(|clip| clip.path == path)?.clone();
        clip.id = self.next_id;
        self.next_id += 1;
        clip.start = start.max(0);
        // Bin transforms describe a source fitted to the canonical 16:9 canvas.
        // Fit each new instance to the current output without stretching it.
        if clip.visual {
            let aspect = clip.transform.width / clip.transform.height * 1920. / 1080.;
            let relative = aspect / (self.width as f32 / self.height as f32);
            clip.transform.width = relative.min(1.);
            clip.transform.height = (1. / relative).min(1.);
        }
        let id = clip.id;
        self.tracks[track].clips.push(clip);
        Some(id)
    }
    pub fn duration(&self) -> Time {
        self.tracks
            .iter()
            .flat_map(|t| &t.clips)
            .map(Clip::end)
            .max()
            .unwrap_or(0)
    }
    pub fn clip(&self, id: u64) -> Option<&Clip> {
        self.tracks
            .iter()
            .flat_map(|t| &t.clips)
            .find(|c| c.id == id)
    }
    pub fn clip_mut(&mut self, id: u64) -> Option<&mut Clip> {
        self.tracks
            .iter_mut()
            .flat_map(|t| &mut t.clips)
            .find(|c| c.id == id)
    }
    pub fn editable(&self, id: u64) -> bool {
        self.tracks
            .iter()
            .any(|t| !t.locked && t.clips.iter().any(|c| c.id == id))
    }
    /// Tracks in bottom-to-top composition order. Audio is evaluated independently of visibility.
    pub fn active(&self, time: Time) -> impl Iterator<Item = (&Track, &Clip, Time)> {
        self.tracks.iter().flat_map(move |t| {
            t.clips
                .iter()
                .filter_map(move |c| c.source_time(time).map(|s| (t, c, s)))
        })
    }
    pub fn split(&mut self, id: u64, at: Time) -> bool {
        if !self.editable(id) {
            return false;
        }
        let Some(c) = self.clip(id) else { return false };
        if at <= c.start || at >= c.end() {
            return false;
        }
        let mut right = c.clone();
        let delta = at - c.start;
        right.id = self.next_id;
        self.next_id += 1;
        right.start = at;
        right.duration -= delta;
        if !right.still {
            right.source_in += delta;
        }
        let track = self
            .tracks
            .iter_mut()
            .find(|t| t.clips.iter().any(|c| c.id == id))
            .unwrap();
        track
            .clips
            .iter_mut()
            .find(|c| c.id == id)
            .unwrap()
            .duration = delta;
        track.clips.push(right);
        true
    }
    pub fn move_clip(&mut self, id: u64, track: usize, start: Time) -> bool {
        if track >= self.tracks.len() || self.tracks[track].locked || !self.editable(id) {
            return false;
        }
        let from = self
            .tracks
            .iter()
            .position(|t| t.clips.iter().any(|c| c.id == id))
            .unwrap();
        let index = self.tracks[from]
            .clips
            .iter()
            .position(|c| c.id == id)
            .unwrap();
        let mut c = self.tracks[from].clips.remove(index);
        c.start = start.max(0);
        self.tracks[track].clips.push(c);
        true
    }
    pub fn validate(&self) -> Result<()> {
        if self.version != 1
            || self.width == 0
            || self.height == 0
            || self.width > 8192
            || self.height > 8192
            || self.fps_num == 0
            || self.fps_den == 0
            || self.fps_num > 240_000
            || self.fps_den > 10_000
            || self.fps_num as f64 / self.fps_den as f64 > 240.
            || self.duration() > 24 * 3600 * SECOND
        {
            bail!("Unsupported project settings");
        }
        let mut ids = std::collections::HashSet::new();
        for (c, timeline) in self
            .tracks
            .iter()
            .flat_map(|t| &t.clips)
            .map(|c| (c, true))
            .chain(self.media.iter().map(|c| (c, false)))
        {
            if let Some(graphic) = &c.graphic {
                if !c.still || !c.visual || c.audio {
                    bail!("Invalid graphic clip");
                }
                if let Graphic::Text(text) = graphic {
                    if !text.size.is_finite()
                        || !(4. ..=512.).contains(&text.size)
                        || text.text.len() > 16384
                        || text.font.len() > 256
                        || text.align > 2
                    {
                        bail!("Invalid text style");
                    }
                }
            }
            if c.fade_in < 0
                || c.fade_out < 0
                || c.fade_in > 24 * 3600 * SECOND
                || c.fade_out > 24 * 3600 * SECOND
            {
                bail!("Invalid fade duration");
            }
            let x = &c.transform;
            if (timeline && (!ids.insert(c.id) || c.id >= self.next_id))
                || c.start < 0
                || c.source_in < 0
                || c.duration <= 0
                || c.start.checked_add(c.duration).is_none()
                || c.source_in.checked_add(c.duration).is_none()
                || (!c.still && c.source_in + c.duration > c.source_duration)
            {
                bail!("Invalid timing or ID for clip {}", c.id);
            }
            if ![x.x, x.y, x.width, x.height, x.rotation, x.opacity, c.gain]
                .iter()
                .all(|v| v.is_finite())
                || x.width <= 0.
                || x.height <= 0.
                || !(0. ..=1.).contains(&x.opacity)
                || !(0. ..=4.).contains(&c.gain)
            {
                bail!("Invalid transform or gain");
            }
        }
        Ok(())
    }
    pub fn save(&self, path: &Path) -> Result<()> {
        self.validate()?;
        let temp = path.with_extension(format!("{}.tmp", std::process::id()));
        let result = (|| {
            use std::io::Write;
            let mut f = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temp)?;
            f.write_all(&serde_json::to_vec_pretty(self)?)?;
            f.sync_all()?;
            std::fs::rename(&temp, path)?;
            Ok(())
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(temp);
        }
        result
    }
    pub fn load(path: &Path) -> Result<Self> {
        let mut p: Self = serde_json::from_slice(&std::fs::read(path)?)?;
        p.validate()?;
        for c in p
            .tracks
            .iter_mut()
            .flat_map(|t| &mut t.clips)
            .chain(p.media.iter_mut())
        {
            if c.graphic.is_some() {
                continue;
            }
            if c.path.is_relative() {
                c.path = path.parent().unwrap_or(Path::new(".")).join(&c.path);
            }
            c.path.try_exists().context("Check media path")?;
        }
        Ok(p)
    }
}
#[derive(Default)]
pub struct History {
    undo: Vec<Project>,
    redo: Vec<Project>,
}
impl History {
    pub fn checkpoint(&mut self, p: &Project) {
        self.undo.push(p.clone());
        if self.undo.len() > 100 {
            self.undo.remove(0);
        }
        self.redo.clear();
    }
    pub fn undo(&mut self, p: &mut Project) {
        if let Some(old) = self.undo.pop() {
            self.redo.push(std::mem::replace(p, old));
        }
    }
    pub fn redo(&mut self, p: &mut Project) {
        if let Some(old) = self.redo.pop() {
            self.undo.push(std::mem::replace(p, old));
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn media_bin_is_saved_without_adding_timeline_clips() {
        let mut project = Project::default();
        let asset = fixture().tracks[0].clips[0].clone();
        project.add_media(asset.clone());
        project.add_media(asset);
        assert_eq!(project.media.len(), 1);
        assert_eq!(project.duration(), 0);
        let bytes = serde_json::to_vec(&project).unwrap();
        let mut restored: Project = serde_json::from_slice(&bytes).unwrap();
        restored.validate().unwrap();
        let first = restored
            .insert_media(Path::new("a.mp4"), 0, SECOND)
            .unwrap();
        let second = restored
            .insert_media(Path::new("a.mp4"), 1, 3 * SECOND)
            .unwrap();
        assert_ne!(first, second);
        restored.clip_mut(first).unwrap().transform.opacity = 0.5;
        assert_eq!(restored.clip(second).unwrap().transform.opacity, 1.);
        assert_eq!(restored.media[0].transform.opacity, 1.);
        restored.tracks[2].locked = true;
        assert!(restored.insert_media(Path::new("a.mp4"), 2, 0).is_none());
        restored.validate().unwrap();
    }

    #[test]
    fn old_projects_get_a_media_bin_without_changing_the_edit() {
        let original = fixture();
        let mut json = serde_json::to_value(&original).unwrap();
        json.as_object_mut().unwrap().remove("media");
        let mut loaded: Project = serde_json::from_value(json).unwrap();
        loaded.populate_media();
        assert_eq!(loaded.tracks, original.tracks);
        assert_eq!(loaded.media.len(), 1);
        assert_eq!(loaded.media[0].source_in, 0);
        assert_eq!(loaded.media[0].duration, loaded.media[0].source_duration);
        loaded.validate().unwrap();
    }

    fn fixture() -> Project {
        let mut p = Project::default();
        p.next_id = 2;
        p.tracks[0].clips.push(Clip {
            id: 1,
            path: "a.mp4".into(),
            start: SECOND,
            source_in: 2 * SECOND,
            duration: 4 * SECOND,
            source_duration: 8 * SECOND,
            visual: true,
            audio: true,
            still: false,
            transform: Transform::default(),
            gain: 1.,
            graphic: None,
            fade_in: 0,
            fade_out: 0,
        });
        p
    }
    #[test]
    fn new_media_instances_fit_the_current_canvas_without_stretching() {
        let mut p = fixture();
        let source = p.tracks[0].clips[0].clone();
        p.add_media(source.clone());
        p.resize_canvas(1080, 1920).unwrap();
        let id = p.insert_media(&source.path, 1, 0).unwrap();
        let t = &p.clip(id).unwrap().transform;
        let aspect = t.width * p.width as f32 / (t.height * p.height as f32);
        assert!((aspect - 16. / 9.).abs() < 0.001);
        assert!(t.width <= 1. && t.height <= 1.);
        p.validate().unwrap();
    }
    #[test]
    fn split_preserves_source_and_exclusive_bounds() {
        let mut p = fixture();
        assert!(p.split(1, 3 * SECOND));
        assert_eq!(
            p.active(3 * SECOND)
                .map(|(_, c, s)| (c.id, s))
                .collect::<Vec<_>>(),
            vec![(2, 4 * SECOND)]
        );
        assert_eq!(p.duration(), 5 * SECOND);
        p.validate().unwrap();
    }
    #[test]
    fn move_and_undo() {
        let mut p = fixture();
        let original = p.clone();
        let mut h = History::default();
        h.checkpoint(&p);
        assert!(p.move_clip(1, 1, 0));
        assert_eq!(p.clip(1).unwrap().source_in, 2 * SECOND);
        h.undo(&mut p);
        assert_eq!(p, original);
        h.redo(&mut p);
        assert_eq!(p.clip(1).unwrap().start, 0);
    }
    #[test]
    fn locked_and_invalid_projects() {
        let mut p = fixture();
        p.tracks[0].locked = true;
        assert!(!p.split(1, 2 * SECOND));
        p.clip_mut(1).unwrap().transform.opacity = f32::NAN;
        assert!(p.validate().is_err());
    }
}

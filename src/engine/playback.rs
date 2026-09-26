//! One mixed audio output and a latency-corrected timeline clock.
use super::{
    decoder::Decoder,
    project::{Project, Time},
};
use std::{
    collections::{HashMap, HashSet},
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering},
    },
    thread,
};
#[derive(Clone)]
struct Transport {
    project: Arc<Project>,
    time: Time,
    playing: bool,
    generation: u64,
}
struct State {
    transport: Transport,
    stopped: bool,
    audio_error: Option<String>,
}
struct Shared {
    generation: Arc<AtomicU64>,
    state: Mutex<State>,
    wake: Condvar,
    clock: AtomicI64,
    audio_ready: AtomicBool,
}
pub struct Engine {
    shared: Arc<Shared>,
    audio: Option<thread::JoinHandle<()>>,
}
impl Engine {
    pub fn new() -> Self {
        let shared = Arc::new(Shared {
            generation: Arc::new(AtomicU64::new(0)),
            state: Mutex::new(State {
                transport: Transport {
                    project: Arc::new(Project::default()),
                    time: 0,
                    playing: false,
                    generation: 0,
                },
                stopped: false,
                audio_error: None,
            }),
            wake: Condvar::new(),
            clock: AtomicI64::new(0),
            audio_ready: AtomicBool::new(false),
        });
        let a = shared.clone();
        let audio = thread::spawn(move || audio_loop(a));
        Self {
            shared,
            audio: Some(audio),
        }
    }
    pub fn transport(&self, project: Arc<Project>, time: Time, playing: bool) -> u64 {
        let mut s = self.shared.state.lock().unwrap();
        let generation = s.transport.generation + 1;
        self.shared.generation.store(generation, Ordering::Release);
        s.transport = Transport {
            project,
            time,
            playing,
            generation,
        };
        self.shared.clock.store(time, Ordering::Release);
        self.shared.audio_ready.store(false, Ordering::Release);
        self.shared.wake.notify_all();
        generation
    }
    pub fn audio_clock(&self) -> Option<Time> {
        self.shared
            .audio_ready
            .load(Ordering::Acquire)
            .then(|| self.shared.clock.load(Ordering::Acquire))
    }
    pub fn audio_error(&self) -> Option<String> {
        self.shared.state.lock().unwrap().audio_error.clone()
    }
}
impl Default for Engine {
    fn default() -> Self {
        Self::new()
    }
}
impl Drop for Engine {
    fn drop(&mut self) {
        self.shared.state.lock().unwrap().stopped = true;
        self.shared.generation.fetch_add(1, Ordering::Release);
        self.shared.wake.notify_all();
        if let Some(t) = self.audio.take() {
            let _ = t.join();
        }
    }
}
#[repr(C)]
struct SampleSpec {
    format: i32,
    rate: u32,
    channels: u8,
}
#[repr(C)]
struct BufferAttr {
    maxlength: u32,
    tlength: u32,
    prebuf: u32,
    minreq: u32,
    fragsize: u32,
}
struct AudioOutput {
    _lib: libloading::Library,
    raw: *mut std::ffi::c_void,
    write: unsafe extern "C" fn(
        *mut std::ffi::c_void,
        *const std::ffi::c_void,
        usize,
        *mut i32,
    ) -> i32,
    latency: unsafe extern "C" fn(*mut std::ffi::c_void, *mut i32) -> u64,
    free: unsafe extern "C" fn(*mut std::ffi::c_void),
}
impl AudioOutput {
    fn new() -> anyhow::Result<Self> {
        unsafe {
            let lib = libloading::Library::new("libpulse-simple.so.0")?;
            let new: *const () = *lib.get(b"pa_simple_new\0")?;
            let new: unsafe extern "C" fn(
                *const i8,
                *const i8,
                i32,
                *const i8,
                *const i8,
                *const SampleSpec,
                *const (),
                *const BufferAttr,
                *mut i32,
            ) -> *mut std::ffi::c_void = std::mem::transmute(new);
            let mut error = 0;
            let spec = SampleSpec {
                format: 5,
                rate: 48000,
                channels: 2,
            }; // PA_SAMPLE_FLOAT32LE
            let attr = BufferAttr {
                maxlength: 38400,
                tlength: 15360,
                // Wait for a real audio buffer before starting the device clock.
                // A zero prebuffer runs through underruns while media is paused.
                prebuf: 15360,
                minreq: 7680,
                fragsize: u32::MAX,
            };
            let raw = new(
                std::ptr::null(),
                c"Slicer".as_ptr(),
                1,
                std::ptr::null(),
                c"Timeline".as_ptr(),
                &spec,
                std::ptr::null(),
                &attr,
                &mut error,
            );
            if raw.is_null() {
                anyhow::bail!("Audio output unavailable (PulseAudio error {error})");
            }
            Ok(Self {
                raw,
                write: *lib.get(b"pa_simple_write\0")?,
                latency: *lib.get(b"pa_simple_get_latency\0")?,
                free: *lib.get(b"pa_simple_free\0")?,
                _lib: lib,
            })
        }
    }
    fn write(&mut self, data: &[f32]) -> anyhow::Result<i64> {
        unsafe {
            let mut e = 0;
            if (self.write)(
                self.raw,
                data.as_ptr().cast(),
                std::mem::size_of_val(data),
                &mut e,
            ) < 0
            {
                anyhow::bail!("Audio output failed ({e})");
            }
            let latency = (self.latency)(self.raw, &mut e);
            if latency == u64::MAX {
                anyhow::bail!("Audio clock unavailable ({e})");
            }
            Ok(latency as i64)
        }
    }
}
impl Drop for AudioOutput {
    fn drop(&mut self) {
        unsafe { (self.free)(self.raw) }
    }
}
fn audio_loop(shared: Arc<Shared>) {
    let mut output: Option<AudioOutput> = None;
    let mut generation = u64::MAX;
    let mut time = 0;
    let mut decoders: HashMap<u64, (std::path::PathBuf, Decoder)> = HashMap::new();
    let mut mixed = vec![0f32; 1920];
    let mut samples = vec![0f32; 1920];
    loop {
        let tr = {
            let mut s = shared.state.lock().unwrap();
            while !s.stopped && !s.transport.playing {
                // PulseAudio can block. Never hold the UI transport mutex during I/O.
                drop(s);
                // Flushing keeps PulseAudio's old stream timeline. After a long
                // pause it can accept an underrun's worth of samples at once,
                // advancing our clock by the whole paused interval on resume.
                // Close the stream; the next transport gets a fresh clock.
                output = None;
                s = shared.state.lock().unwrap();
                if !s.stopped && !s.transport.playing {
                    s = shared.wake.wait(s).unwrap();
                }
            }
            if s.stopped {
                return;
            }
            s.transport.clone()
        };
        if generation != tr.generation {
            generation = tr.generation;
            time = tr.time;
            output = None;
        }
        if output.is_none() {
            match AudioOutput::new() {
                Ok(o) => {
                    output = Some(o);
                    shared.state.lock().unwrap().audio_error = None;
                }
                Err(e) => {
                    shared.state.lock().unwrap().audio_error = Some(e.to_string());
                    thread::sleep(std::time::Duration::from_millis(50));
                    continue;
                }
            }
        }
        mixed.fill(0.);
        let mut active = HashSet::new();
        for track in &tr.project.tracks {
            if track.muted {
                continue;
            }
            for clip in &track.clips {
                if !clip.audio || clip.end() <= time || clip.start >= time + 20_000 {
                    continue;
                }
                active.insert(clip.id);
                let offset = ((clip.start - time).max(0) * 48000 / 1_000_000) as usize;
                let end = (((clip.end() - time) * 48000 / 1_000_000).clamp(0, 960)) as usize;
                if end <= offset {
                    continue;
                }
                if decoders
                    .get(&clip.id)
                    .is_some_and(|(path, _)| path != &clip.path)
                {
                    decoders.remove(&clip.id);
                }
                if let std::collections::hash_map::Entry::Vacant(entry) = decoders.entry(clip.id) {
                    match Decoder::open(&clip.path, true) {
                        Ok(d) => {
                            entry.insert((clip.path.clone(), d));
                        }
                        Err(e) => {
                            shared.state.lock().unwrap().audio_error = Some(e.to_string());
                            continue;
                        }
                    }
                }
                let source = clip.source_in + (time - clip.start).max(0);
                if let Err(e) = decoders
                    .get_mut(&clip.id)
                    .unwrap()
                    .1
                    .audio(source, &mut samples[..(end - offset) * 2])
                {
                    shared.state.lock().unwrap().audio_error = Some(e.to_string());
                    continue;
                }
                for (sample, (dst, src)) in mixed[offset * 2..end * 2]
                    .iter_mut()
                    .zip(&samples)
                    .enumerate()
                {
                    *dst += src
                        * clip.gain
                        * clip.fade(time + (offset + sample / 2) as i64 * 1_000_000 / 48000);
                }
            }
        }
        decoders.retain(|id, _| active.contains(id));
        for x in &mut mixed {
            *x = x.clamp(-1., 1.);
        }
        {
            let s = shared.state.lock().unwrap();
            if s.stopped {
                return;
            }
            if s.transport.generation != generation {
                continue;
            }
        }
        match output.as_mut().unwrap().write(&mixed) {
            Ok(latency) => {
                time += 20_000;
                let s = shared.state.lock().unwrap();
                if s.transport.generation == generation {
                    shared
                        .clock
                        .store((time - latency).max(tr.time), Ordering::Release);
                    shared.audio_ready.store(true, Ordering::Release);
                }
            }
            Err(e) => {
                shared.state.lock().unwrap().audio_error = Some(e.to_string());
                shared.audio_ready.store(false, Ordering::Release);
                output = None;
            }
        }
    }
}

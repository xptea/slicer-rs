//! GPUI coordination for a persistent native video player and its drawable.
use super::*;

#[derive(Default)]
pub(super) struct NativePreview {
    // Rust drops fields in declaration order: detach the renderer before destroying its drawable.
    player: Option<native_player::NativePlayer>,
    // libmpv loads a sizeable dependency graph and initializes the GPU backend. Keep that work
    // off the GPUI thread and overlap it with ffprobe metadata inspection.
    player_start: Option<std::thread::JoinHandle<Result<native_player::NativePlayer, String>>>,
    surface: Option<native_surface::NativeSurface>,
    path: Option<PathBuf>,
    loaded_path: Option<PathBuf>,
    pending_seek: Option<(f64, bool)>,
    seek_target: Option<(f64, Instant)>,
    range: Option<(f64, f64)>,
    pub paused: bool,
    pub muted: bool,
    pub ready: bool,
    pub error: Option<String>,
    failed: bool,
    clock: super::playhead_clock::PlayheadClock,
}

impl NativePreview {
    /// Start the native player while Home is visible so libmpv's GPU backend
    /// is ready before the first video is opened. The drawable is a one-pixel
    /// off-screen X11 child during this warm-up and is resized/mapped to the
    /// editor preview on demand.
    pub fn prewarm(&mut self, window: &mut Window) {
        if self.failed || self.player.is_some() || self.player_start.is_some() {
            return;
        }
        let Ok(mut surface) = native_surface::NativeSurface::new(window) else {
            // Wayland and unsupported window backends report the normal
            // native-surface error when the editor is actually opened.
            return;
        };
        let bounds = Bounds::new(point(px(-1000.), px(-1000.)), size(px(1.), px(1.)));
        if surface.update(bounds, window.scale_factor(), true).is_err() {
            return;
        }
        // Keep the off-screen parent mapped until libmpv finishes creating its
        // child. An unmapped X11 parent can prevent the GPU video output from
        // initializing even though the player itself reports success.
        let window_id = surface.window_id();
        self.surface = Some(surface);
        self.player_start = Some(std::thread::spawn(move || {
            native_player::NativePlayer::new(window_id)
        }));
    }

    fn finish_player_start(&mut self) -> bool {
        if self.player.is_some()
            || !self
                .player_start
                .as_ref()
                .is_some_and(std::thread::JoinHandle::is_finished)
        {
            return self.player.is_some();
        }
        let start = self
            .player_start
            .take()
            .expect("finished player startup handle exists");
        match start.join() {
            Ok(Ok(player)) => {
                self.player = Some(player);
                self.paused = true;
                true
            }
            Ok(Err(error)) => {
                self.hide();
                self.error = Some(error);
                self.failed = true;
                false
            }
            Err(_) => {
                self.hide();
                self.error = Some("native player startup thread panicked".to_owned());
                self.failed = true;
                false
            }
        }
    }

    pub fn shutdown(&mut self) {
        // The renderer must detach while its GPUI parent window still exists.
        self.ready = false;
        self.paused = true;
        // The player constructor receives the surface XID. Join its startup before either the
        // player or the X11 drawable can be destroyed, otherwise a just-completed constructor
        // could hand libmpv a child window that has already gone away.
        if let Some(start) = self.player_start.take() {
            match start.join() {
                Ok(Ok(player)) => drop(player),
                Ok(Err(error)) => self.error = Some(error),
                Err(_) => self.error = Some("native player startup thread panicked".to_owned()),
            }
        }
        #[cfg(target_os = "macos")]
        if let Some(surface) = &mut self.surface {
            surface.detach_player();
        }
        drop(self.player.take());
        self.surface.take();
    }

    fn record(&mut self, result: Result<(), String>) {
        if let Err(error) = result {
            self.error = Some(error);
        }
    }
    pub fn prepare_file(&mut self, path: &Path) {
        self.pause();
        self.hide();
        self.path = Some(path.to_owned());
        self.loaded_path = None;
        // A normal load starts at zero. Queueing an exact seek to zero here (and again from
        // finish_inspection) needlessly restarts a decoder that has already presented its first
        // frame. Non-zero user requests are still queued by seek() as usual.
        self.pending_seek = None;
        self.seek_target = None;
        self.clock.reset();
        self.ready = false;
        self.error = None;
        self.range = None;
    }
    pub fn hide(&mut self) {
        // The warm-up surface lives off-screen while libmpv initializes. Keep
        // its parent mapped until the constructor finishes; otherwise some
        // X11/VO combinations defer GPU setup until the first visible load.
        if self.player.is_none() && self.player_start.is_some() {
            return;
        }
        if let Some(surface) = &mut self.surface {
            surface.hide();
        }
    }
    pub fn pause(&mut self) {
        self.paused = true;
        if let Some(player) = &self.player {
            let result = player.set_paused(true);
            self.record(result);
        }
    }
    pub fn play(&mut self) {
        if !self.ready {
            return;
        }
        if let Some(player) = &self.player {
            let result = player.set_paused(false);
            self.record(result);
            self.paused = false;
        }
    }
    pub fn set_crop(&mut self, crop: Option<job::CropRect>) {
        if let Some(player) = &self.player {
            let result = player.set_crop(crop.map(|r| (r.x, r.y, r.width, r.height)));
            self.record(result);
        }
    }
    pub fn toggle_mute(&mut self) {
        self.muted = !self.muted;
        if let Some(player) = &self.player {
            let result = player.set_mute(self.muted);
            self.record(result);
        }
    }
    pub fn set_range(&mut self, start: f64, end: f64) {
        self.range = Some((start, end));
        if let Some(player) = &self.player {
            let result = player.set_range(start, end);
            self.record(result);
        }
    }
    pub fn seek(&mut self, seconds: f64, exact: bool) {
        self.clock.reset();
        self.pending_seek = Some((seconds, exact));
        self.seek_target = Some((seconds, Instant::now()));
        if self.ready
            && let Some(player) = &self.player
        {
            let result = player.seek(seconds, exact);
            self.record(result);
            self.pending_seek = None;
        }
    }
    pub fn update(&mut self, bounds: Bounds<Pixels>, window: &mut Window, visible: bool) {
        if self.failed || !visible {
            self.hide();
            return;
        }
        if self.surface.is_none() {
            match native_surface::NativeSurface::new(window) {
                Ok(surface) => self.surface = Some(surface),
                Err(error) => {
                    self.error = Some(error.to_string());
                    self.failed = true;
                    return;
                }
            }
        }

        // Establish a visible, correctly sized drawable before mpv creates its child. This is a
        // short X11 operation; the expensive libmpv load/initialize is started below on a worker.
        if let Err(error) = self
            .surface
            .as_mut()
            .expect("native surface exists")
            .update(bounds, window.scale_factor(), visible)
        {
            self.error = Some(error.to_string());
            self.failed = true;
            return;
        }

        if self.player.is_none() {
            if self.player_start.is_some() && !self.finish_player_start() && self.failed {
                return;
            }

            if self.player.is_none() && self.player_start.is_none() {
                let window_id = self
                    .surface
                    .as_ref()
                    .expect("native surface exists")
                    .window_id();
                self.player_start = Some(std::thread::spawn(move || {
                    native_player::NativePlayer::new(window_id)
                }));
            }
        }

        #[cfg(target_os = "macos")]
        if let Some(player) = self.player.as_ref()
            && let Err(error) = self
                .surface
                .as_mut()
                .expect("native surface exists")
                .attach_player(player)
        {
            self.error = Some(error.to_string());
            self.failed = true;
            self.hide();
            return;
        }

        if let Some(player) = self.player.as_ref()
            && self.loaded_path != self.path
            && let Some(path) = self.path.as_ref()
        {
            let result = match self.range {
                Some((start, end)) => player.load_file(path, start, end),
                // Metadata can still be in flight. Start demuxing immediately and add the trim
                // range once ffprobe completes; the player is paused, so this cannot race a play.
                None => player.load(path),
            };
            if let Err(error) = result {
                self.error = Some(error);
                self.hide();
                self.failed = true;
                return;
            }
            self.loaded_path = Some(path.clone());
            self.ready = false;
        }

        if let Err(error) = self
            .surface
            .as_mut()
            .expect("native surface exists")
            .update(
                bounds,
                window.scale_factor(),
                visible && self.error.is_none(),
            )
        {
            self.error = Some(error.to_string());
            self.hide();
        }

        // Metadata may finish before the background player constructor. Keep a lightweight frame
        // heartbeat until startup and the first asynchronous load have both been observed.
        if self.player_start.is_some()
            || self.seek_target.is_some()
            || (self.path.is_some() && !self.ready && self.error.is_none())
        {
            window.request_animation_frame();
        }
    }
    pub fn poll(&mut self) -> Option<f64> {
        self.finish_player_start();
        let player = self.player.as_ref()?;
        let snapshot = player.snapshot();
        // A replacement file clears loaded_path before its asynchronous load is queued. Ignore
        // the previous file's last snapshot during that gap so the transport cannot briefly
        // re-enable or paint stale media while the new path is still being handed to mpv.
        if self.path.is_none() || self.loaded_path.as_ref() != self.path.as_ref() {
            self.ready = false;
            self.paused = true;
            return None;
        }
        self.ready = snapshot.loaded;
        self.paused = snapshot.paused;
        if let Some(error) = snapshot.error {
            self.error = Some(error);
            self.hide();
            return None;
        }
        if self.ready
            && let Some((seconds, exact)) = self.pending_seek.take()
            && let Err(error) = player.seek(seconds, exact)
        {
            self.error = Some(error);
        }
        // Variable-frame-rate recordings can hold a frame longer than 150 ms.
        // Allow that presentation gap while still rejecting stale seek positions.
        if let Some((target, began)) = self.seek_target {
            if snapshot.seeking
                || ((snapshot.position - target).abs() > 0.25
                    && began.elapsed() < Duration::from_secs(2))
            {
                return None;
            }
            self.seek_target = None;
            self.clock.reset();
        }
        let position = self.clock.position(
            snapshot.position,
            self.ready && !snapshot.paused && !snapshot.seeking,
            Instant::now(),
        );
        Some(
            self.range
                .map_or(position, |(start, end)| position.clamp(start, end)),
        )
    }
}

impl Drop for NativePreview {
    fn drop(&mut self) {
        self.shutdown();
    }
}

impl SlicerApp {
    pub(super) fn update_native_surface(
        &mut self,
        bounds: Bounds<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(_path) = &self.editor_path else {
            self.native.hide();
            return;
        };
        if let Some(media) = &self.media {
            if media.streams.iter().all(|stream| stream.kind != "video") {
                self.preview_error = Some("This file has no video track".into());
                self.native.hide();
                return;
            }
            if self.native.range.is_none() {
                let (start, end) = self.trim_range(cx);
                self.native.set_range(start, end);
            }
        }
        self.native.update(
            bounds,
            window,
            self.screen == Screen::Editor
                && !self.export_modal
                && !self.crop.is_ready()
                && self.external_drop.is_none(),
        );
    }
}

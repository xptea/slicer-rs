//! Background job polling, library scanning, and thumbnail updates.

use super::*;

impl SlicerApp {
    pub(super) fn poll_background(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(result) = self.update_rx.as_ref().and_then(|rx| rx.try_recv().ok()) {
            self.update_rx = None;
            self.available_update = result;
            cx.notify();
        }
        if let Some(path) = self
            .opened_files
            .as_ref()
            .and_then(|rx| rx.try_iter().last())
        {
            self.open_file(path, window, cx);
        }
        self.poll_playback(cx);
        self.poll_crop();
        self.poll_export_toast();
        if let Some(result) = self.settings_rx.as_ref().and_then(|rx| rx.try_recv().ok()) {
            self.settings_rx = None;
            self.settings_loading = false;
            match result {
                Ok(settings) => {
                    self.default_quality_slider.update(cx, |state, cx| {
                        state.set_value(settings.export_defaults.quality as f32, window, cx)
                    });
                    self.settings_error = None;
                    if let Some(directory) = settings.library_directory.clone() {
                        self.settings = settings;
                        self.scan_library(directory);
                    } else {
                        self.settings = settings;
                        self.status =
                            "Set a library folder in Settings to see your newest videos".to_owned();
                    }
                }
                Err(error) => {
                    self.settings_error = Some(error.clone());
                    self.status = format!("Settings could not be loaded: {error}");
                }
            }
            cx.notify();
        }

        if let Some(result) = self
            .settings_save_rx
            .as_ref()
            .and_then(|rx| rx.try_recv().ok())
        {
            self.settings_save_rx = None;
            match result {
                Ok(()) => {
                    self.settings_error = None;
                    self.status = "Settings saved".to_owned();
                }
                Err(error) => {
                    self.settings_error = Some(error.clone());
                    self.status = format!("Could not save settings: {error}");
                }
            }
            if self.settings_save_pending {
                self.save_settings();
            }
            cx.notify();
        }

        if let Some(result) = self.scan_rx.as_ref().and_then(|rx| rx.try_recv().ok()) {
            self.scan_rx = None;
            self.library_scanning = false;
            if result.generation == self.scan_generation
                && self.settings.library_directory.as_deref() == Some(result.directory.as_path())
            {
                match result.videos {
                    Ok(videos) => {
                        self.library_error = None;
                        self.install_recent(videos);
                        self.status = if self.recent.is_empty() {
                            "No videos found in this folder".to_owned()
                        } else {
                            format!("Showing the {} newest videos", self.recent.len())
                        };
                        self.request_next_thumbnail();
                    }
                    Err(error) => {
                        self.library_error = Some(error.clone());
                        self.recent.clear();
                        self.thumbnails.clear();
                        self.thumbnail_pending = None;
                        self.status = error;
                    }
                }
            }
            cx.notify();
        }

        if self.screen == Screen::Home
            && self.settings.library_directory.is_some()
            && !self.library_scanning
            && self.library_refresh_at.elapsed() >= Duration::from_secs(5)
        {
            self.library_refresh_at = Instant::now();
            if let Some(directory) = self.settings.library_directory.clone() {
                self.scan_library(directory);
            }
        }

        if let Some(result) = self.inspect_rx.as_ref().and_then(|rx| rx.try_recv().ok()) {
            self.inspect_rx = None;
            if self.editor_path.as_deref() == Some(result.path.as_path()) {
                self.inspecting = false;
                match result.info {
                    Ok(info) => self.finish_inspection(info, window, cx),
                    Err(error) => {
                        self.media = None;
                        self.media_error = Some(error.clone());
                        self.status = format!("Could not inspect media: {error}");
                    }
                }
            }
            cx.notify();
        }

        if let Some(path) = self
            .open_dialog_rx
            .as_ref()
            .and_then(|rx| rx.try_recv().ok())
        {
            self.open_dialog_rx = None;
            if let Some(path) = path {
                self.open_file(path, window, cx);
            }
        }
        if let Some(path) = self
            .folder_dialog_rx
            .as_ref()
            .and_then(|rx| rx.try_recv().ok())
        {
            self.folder_dialog_rx = None;
            if let Some(directory) = path {
                // A user selection made before the initial settings read
                // completes is authoritative. Drop the old receiver so a
                // late disk read cannot restore the previous folder.
                self.settings_rx = None;
                self.settings_loading = false;
                self.settings.library_directory = Some(directory.clone());
                self.settings_error = None;
                self.save_settings();
                self.scan_library(directory);
                self.status = "Scanning library…".to_owned();
            }
        }
        if let Some(path) = self
            .output_dialog_rx
            .as_ref()
            .and_then(|rx| rx.try_recv().ok())
        {
            self.output_dialog_rx = None;
            if let Some(path) = path {
                let path = path.with_extension(self.format.extension());
                if let Some(name) = path.file_stem().and_then(|s| s.to_str()) {
                    self.set_input_value(&self.filename_input.clone(), name.to_owned(), window, cx);
                }
                self.output_touched = true;
                self.set_input_value(
                    &self.output_input.clone(),
                    path.display().to_string(),
                    window,
                    cx,
                );
                self.status = "Output destination selected".to_owned();
            }
        }

        if let Some(path) = self
            .default_output_dialog_rx
            .as_ref()
            .and_then(|rx| rx.try_recv().ok())
        {
            self.default_output_dialog_rx = None;
            if let Some(path) = path {
                self.settings.export_defaults.output_directory = Some(path);
                self.save_settings();
            }
        }
        self.poll_thumbnail_events(cx);
        self.poll_waveform_events(cx);
        self.poll_export_events(cx);
    }

    pub(super) fn poll_waveform_events(&mut self, cx: &mut Context<Self>) {
        let mut events = Vec::new();
        if let Some(worker) = self.waveform_worker.as_ref() {
            while let Ok(event) = worker.events.try_recv() {
                events.push(event);
            }
        }
        for event in events {
            if event.generation != self.waveform_generation
                || self.editor_path.as_deref() != Some(event.path.as_path())
                || self.waveform_path.as_deref() != Some(event.path.as_path())
            {
                continue;
            }
            match event.result {
                Ok(waveform) => {
                    self.waveform = Some(Arc::new(waveform));
                    self.waveform_error = None;
                }
                Err(error) => {
                    self.waveform = None;
                    self.waveform_error = Some(error);
                }
            }
            cx.notify();
        }
    }

    pub(super) fn install_recent(&mut self, videos: Vec<home::RecentVideo>) {
        let old_pending = self.thumbnail_pending.take();
        let previous = std::mem::take(&mut self.thumbnails);
        let same_identity = previous.len() == videos.len()
            && previous
                .iter()
                .zip(videos.iter())
                .all(|(slot, video)| slot.path == video.path && slot.modified == video.modified);
        if !same_identity {
            // A path can be replaced in place between two scans. Recreate the
            // worker so a late frame for the old file cannot be attached to a
            // new card with the same pathname.
            self.thumbnail_worker = self.binaries.clone().map(preview::PreviewWorker::new);
        }
        self.recent = videos;
        self.thumbnails = self
            .recent
            .iter()
            .map(|video| {
                previous
                    .iter()
                    .find(|slot| slot.path == video.path && slot.modified == video.modified)
                    .cloned()
                    .unwrap_or_else(|| ThumbnailSlot {
                        path: video.path.clone(),
                        modified: video.modified,
                        image: None,
                        error: None,
                    })
            })
            .collect();
        self.thumbnail_pending = if same_identity {
            old_pending.filter(|(path, modified)| {
                self.thumbnails.iter().any(|slot| {
                    &slot.path == path
                        && &slot.modified == modified
                        && slot.image.is_none()
                        && slot.error.is_none()
                })
            })
        } else {
            None
        };
    }

    pub(super) fn scan_library(&mut self, directory: PathBuf) {
        self.scan_generation = self.scan_generation.wrapping_add(1);
        let generation = self.scan_generation;
        self.library_scanning = true;
        self.library_error = None;
        let (tx, rx) = mpsc::channel();
        self.scan_rx = Some(rx);
        thread::spawn(move || {
            let videos = home::recent_videos(&directory).map_err(|error| format!("{error:#}"));
            let _ = tx.send(ScanResult {
                generation,
                directory,
                videos,
            });
        });
    }

    pub(super) fn request_next_thumbnail(&mut self) {
        if self.thumbnail_pending.is_some() {
            return;
        }
        let Some(worker) = self.thumbnail_worker.as_ref() else {
            return;
        };
        let Some(slot) = self
            .thumbnails
            .iter()
            .find(|slot| slot.image.is_none() && slot.error.is_none())
        else {
            return;
        };
        self.thumbnail_pending = Some((slot.path.clone(), slot.modified));
        worker.request(slot.path.clone(), 0.0);
    }

    pub(super) fn poll_thumbnail_events(&mut self, cx: &mut Context<Self>) {
        let mut events = Vec::new();
        if let Some(worker) = self.thumbnail_worker.as_ref() {
            while let Ok(event) = worker.events.try_recv() {
                events.push(event);
            }
        }
        for event in events {
            let matches_pending = self
                .thumbnail_pending
                .as_ref()
                .is_some_and(|(path, _)| *path == event.path);
            let Some(slot) = self
                .thumbnails
                .iter_mut()
                .find(|slot| slot.path == event.path)
            else {
                continue;
            };
            if !matches_pending {
                continue;
            }
            self.thumbnail_pending = None;
            match event.result {
                Ok(bytes) => {
                    slot.image = Some(Arc::new(Image::from_bytes(ImageFormat::Png, bytes)))
                }
                Err(error) => slot.error = Some(error),
            }
            self.request_next_thumbnail();
            cx.notify();
        }
    }

    pub(super) fn poll_export_events(&mut self, cx: &mut Context<Self>) {
        let mut events = Vec::new();
        if let Some(job) = self.export_job.as_ref() {
            while let Ok(event) = job.events.try_recv() {
                events.push(event);
            }
        }
        for event in events {
            match event {
                job::JobEvent::Progress(progress) => {
                    self.export_progress = progress.clamp(0.0, 1.0);
                    self.status = format!("Exporting… {:.0}%", self.export_progress * 100.0);
                }
                job::JobEvent::Completed(path) => {
                    self.export_progress = 1.0;
                    self.exported_path = Some(path.clone());
                    self.export_state = ExportState::Completed;
                    self.status = format!("Export complete · {}", path.display());
                    self.export_job = None;
                    self.export_modal = false;
                    self.clipboard_rx = None;
                    self.toast = Some(export_toast::ExportToast {
                        message: "Exported".into(),
                        until: Instant::now() + Duration::from_secs(10),
                    });
                    if self.settings.export_defaults.copy_to_clipboard {
                        let (owner, rx) = file_clipboard::FileClipboard::copy(path);
                        self.clipboard = Some(owner);
                        self.clipboard_rx = Some(rx);
                    }
                }
                job::JobEvent::Cancelled => {
                    self.export_state = ExportState::Cancelled;
                    self.status = "Export cancelled".to_owned();
                    self.export_job = None;
                }
                job::JobEvent::Failed(error) => {
                    self.native.pause();
                    self.export_modal = true;
                    self.export_state = ExportState::Failed;
                    self.status = format!("Export failed: {error}");
                    self.export_job = None;
                }
            }
            cx.notify();
        }
    }
}

//! User actions and their dispatch to media workers and native dialogs.

use super::*;

impl SlicerApp {
    pub(super) fn save_settings(&mut self) {
        if self.settings_save_rx.is_some() {
            self.settings_save_pending = true;
            return;
        }
        self.settings_save_pending = false;
        let settings = self.settings.clone();
        let (tx, rx) = mpsc::channel();
        self.settings_save_rx = Some(rx);
        thread::spawn(move || {
            let _ = tx.send(settings.save().map_err(|error| format!("{error:#}")));
        });
    }

    pub(super) fn open_file(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        if self.export_job.is_some() {
            self.screen = Screen::Editor;
            self.status = "Finish or cancel current export before opening another".to_owned();
            return;
        }
        self.native.pause();
        self.waveform_generation = self
            .waveform_worker
            .as_ref()
            .map(waveform::WaveformWorker::cancel)
            .unwrap_or_else(|| self.waveform_generation.wrapping_add(1));
        self.waveform = None;
        self.waveform_path = None;
        self.waveform_error = None;
        self.export_modal = false;
        self.screen = Screen::Editor;
        self.editor_path = Some(path.clone());
        self.media = None;
        self.media_error = None;
        self.inspecting = true;
        self.inspect_rx = None;
        self.native.set_crop(None);
        self.crop = crop::CropEditor::default();
        self.native.prepare_file(&path);
        self.preview_error = None;
        self.preview_seconds = 0.0;
        self.export_state = ExportState::Idle;
        self.export_progress = 0.0;
        self.exported_path = None;
        self.output_touched = false;
        self.set_input_value(&self.start_input.clone(), "0:00".to_owned(), window, cx);
        self.set_input_value(&self.end_input.clone(), "0:00".to_owned(), window, cx);
        self.set_input_value(
            &self.output_input.clone(),
            default_output_path(&path, self.format)
                .display()
                .to_string(),
            window,
            cx,
        );
        self.status = format!("Inspecting {}…", path.display());

        let bins = self.binaries.clone();
        let (tx, rx) = mpsc::channel();
        self.inspect_rx = Some(rx);
        thread::spawn(move || {
            let info = match bins {
                Some(bins) => media::inspect(&bins, &path).map_err(|error| format!("{error:#}")),
                None => Err("FFmpeg binaries are unavailable".to_owned()),
            };
            let _ = tx.send(InspectResult { path, info });
        });
    }

    pub(super) fn finish_inspection(
        &mut self,
        info: media::MediaInfo,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let duration = info.duration.max(0.0);
        self.media = Some(info);
        self.media_error = None;
        let has_audio = self
            .media
            .as_ref()
            .is_some_and(|media| media.streams.iter().any(|stream| stream.kind == "audio"));
        if let Some(path) = self.editor_path.clone() {
            self.waveform_path = Some(path.clone());
            self.waveform_error = None;
            if let Some(worker) = self.waveform_worker.as_ref() {
                self.waveform_generation = worker.request(path, duration, has_audio);
            } else {
                self.waveform_generation = self.waveform_generation.wrapping_add(1);
                self.waveform = Some(Arc::new(if has_audio {
                    waveform::Waveform::from_peaks(duration, Vec::new(), true)
                } else {
                    waveform::Waveform::silence(duration)
                }));
            }
        }
        self.set_input_value(
            &self.end_input.clone(),
            format_timestamp(duration),
            window,
            cx,
        );
        if let Some(path) = self.editor_path.clone()
            && !self.output_touched
        {
            self.set_input_value(
                &self.output_input.clone(),
                default_output_path(&path, self.format)
                    .display()
                    .to_string(),
                window,
                cx,
            );
        }
        // Native loading already presents frame zero; avoid restarting the decoder.
        self.status = "Ready to trim".to_owned();
    }

    pub(super) fn set_input_value(
        &mut self,
        input: &Entity<InputState>,
        value: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        input.update(cx, |state, cx| state.set_value(value, window, cx));
    }

    pub(super) fn request_preview(&mut self, path: PathBuf, seconds: f64) {
        let duration = self
            .media
            .as_ref()
            .map(|info| info.duration)
            .unwrap_or(f64::MAX);
        let max_seek = if duration.is_finite() {
            (duration - 0.001).max(0.0)
        } else {
            duration
        };
        let seconds = seconds.clamp(0.0, max_seek);
        self.preview_seconds = seconds;
        self.native.pause();
        self.preview_error = None;
        self.native.seek(seconds, self.timeline_drag.is_none());
        let _ = path;
    }

    pub(super) fn sync_preview_from_input(
        &mut self,
        input: &Entity<InputState>,
        cx: &mut Context<Self>,
    ) {
        let Some(path) = self.editor_path.clone() else {
            return;
        };
        let value = input.read(cx).value().to_string();
        let Some(seconds) = parse_timestamp(&value) else {
            return;
        };
        self.request_preview(path, seconds);
    }

    pub(super) fn seek_to_trim_start(&mut self, cx: &mut Context<Self>) {
        let Some(path) = self.editor_path.clone() else {
            return;
        };
        let (start, _) = self.trim_range(cx);
        self.request_preview(path, start);
    }

    pub(super) fn seek_to_trim_end(&mut self, cx: &mut Context<Self>) {
        let Some(path) = self.editor_path.clone() else {
            return;
        };
        let end = self.trim_end_position(cx);
        self.request_preview(path, end);
    }

    pub(super) fn mark_start(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let value = format_timestamp(self.preview_seconds);
        self.set_input_value(&self.start_input.clone(), value, window, cx);
        self.status = "Start marked at the current frame".to_owned();
    }

    pub(super) fn mark_end(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let value = format_timestamp(self.preview_seconds);
        self.set_input_value(&self.end_input.clone(), value, window, cx);
        self.status = "End marked at the current frame".to_owned();
    }

    pub(super) fn handle_key(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.screen == Screen::Studio {
            self.studio_key(event, cx);
            return;
        }
        if self.screen != Screen::Editor {
            return;
        }
        if self.crop.open {
            match event.keystroke.key.as_str() {
                "escape" => self.crop.open = false,
                "left" | "arrowleft" => {
                    self.nudge_crop_pixels(-2, 0);
                    cx.notify();
                }
                "right" | "arrowright" => {
                    self.nudge_crop_pixels(2, 0);
                    cx.notify();
                }
                "up" | "arrowup" => {
                    self.nudge_crop_pixels(0, -2);
                    cx.notify();
                }
                "down" | "arrowdown" => {
                    self.nudge_crop_pixels(0, 2);
                    cx.notify();
                }
                _ => {}
            }
            return;
        }
        if self.export_modal {
            if event.keystroke.key == "escape" && self.export_job.is_none() {
                self.export_modal = false;
                cx.stop_propagation();
            }
            return;
        }
        let input_is_focused = self
            .start_input
            .read(cx)
            .presentation()
            .focus_handle()
            .is_focused(window)
            || self
                .end_input
                .read(cx)
                .presentation()
                .focus_handle()
                .is_focused(window)
            || self
                .output_input
                .read(cx)
                .presentation()
                .focus_handle()
                .is_focused(window);
        if input_is_focused || self.export_modal {
            return;
        }
        let key = event.keystroke.key.as_str();
        match key {
            "left" | "arrowleft" => self.seek_to_trim_start(cx),
            "right" | "arrowright" => self.seek_to_trim_end(cx),
            "i" => self.mark_start(window, cx),
            "o" => self.mark_end(window, cx),
            "space" => {
                self.toggle_playback(cx);
                cx.stop_propagation();
            }
            _ => {}
        }
    }

    pub(super) fn prepare_export_defaults(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let defaults = self.settings.export_defaults.clone();
        self.format = match defaults.format {
            home::ExportFormat::Mkv => job::OutputFormat::Mkv,
            home::ExportFormat::Wav => job::OutputFormat::Wav,
            home::ExportFormat::Gif => job::OutputFormat::Gif,
            _ => job::OutputFormat::Mp4,
        };
        self.mode = job::TrimMode::Exact;
        self.quality = defaults.quality.clamp(50, 100);
        self.quality_slider.update(cx, |state, cx| {
            state.set_value(self.quality as f32, window, cx)
        });
        if let Some(input) = self.editor_path.as_ref() {
            let mut output = default_output_path(input, self.format);
            if let Some(directory) = defaults.output_directory {
                output = directory.join(output.file_name().unwrap());
            }
            let stem = output
                .file_stem()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned();
            let mut suffix = 2;
            while output.exists() {
                output.set_file_name(format!("{stem}-{suffix}.{}", self.format.extension()));
                suffix += 1;
            }
            self.set_input_value(
                &self.filename_input.clone(),
                output
                    .file_stem()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned(),
                window,
                cx,
            );
            self.set_input_value(
                &self.output_input.clone(),
                output.to_string_lossy().into_owned(),
                window,
                cx,
            );
            self.output_touched = true;
        }
    }
    pub(super) fn start_default_export(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.export_job.is_some() || self.settings_loading {
            return;
        }
        self.prepare_export_defaults(window, cx);
        self.start_export(cx);
        if self.export_state == ExportState::Failed {
            self.export_modal = true;
        }
    }

    pub(super) fn start_export(&mut self, cx: &mut Context<Self>) {
        if self.export_job.is_some() {
            return;
        }
        self.mode = job::TrimMode::Exact;
        let Some(input) = self.editor_path.clone() else {
            return;
        };
        let Some(info) = self.media.as_ref() else {
            self.status = "Wait for media inspection to finish".to_owned();
            return;
        };
        let start_text = self.start_input.read(cx).value().to_string();
        let end_text = self.end_input.read(cx).value().to_string();
        let start = parse_timestamp(&start_text);
        let end = parse_timestamp(&end_text);
        let (Some(start), Some(end)) = (start, end) else {
            self.status = "Enter valid timestamps such as 0:12 or 00:01:24".to_owned();
            self.export_state = ExportState::Failed;
            return;
        };
        if start < 0.0 || end <= start || end > info.duration + 0.05 {
            self.status = format!(
                "Choose a range between 0:00 and {}",
                format_timestamp(info.duration)
            );
            self.export_state = ExportState::Failed;
            return;
        }
        let output_text = self.output_input.read(cx).value().to_string();
        let destination = if output_text.trim().is_empty() {
            self.status = "Choose an output destination".to_owned();
            return;
        } else {
            PathBuf::from(output_text)
        };
        let name = self.filename_input.read(cx).value().to_string();
        let name = name.trim();
        if name.is_empty() || name == "." || name == ".." || name.contains(['/', '\\']) {
            self.status = "Enter a file name without folder separators".into();
            self.export_state = ExportState::Failed;
            return;
        }
        let mut filename = PathBuf::from(name);
        if filename
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|extension| {
                ["mp4", "mkv", "wav", "webm", "mp3", "gif"]
                    .iter()
                    .any(|known| extension.eq_ignore_ascii_case(known))
            })
        {
            filename.set_extension(self.format.extension());
        } else {
            filename = PathBuf::from(format!("{name}.{}", self.format.extension()));
        }
        let output = destination
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join(filename);
        let Some(binaries) = self.binaries.clone() else {
            self.status = self
                .binaries_error
                .clone()
                .unwrap_or_else(|| "FFmpeg binaries are unavailable".to_owned());
            self.export_state = ExportState::Failed;
            return;
        };

        let request = job::ExportRequest {
            input,
            output,
            start,
            end,
            mode: self.mode,
            format: self.format,
            quality: self.quality,
            mute_audio: self.native.muted,
            crop: if self.format == job::OutputFormat::Wav {
                None
            } else {
                self.crop.applied
            },
        };
        match job::JobHandle::spawn(binaries, request) {
            Ok(handle) => {
                self.export_job = Some(handle);
                self.export_state = ExportState::Running;
                self.export_progress = 0.0;
                self.exported_path = None;
                self.status = "Export queued in the background".to_owned();
            }
            Err(error) => {
                self.export_state = ExportState::Failed;
                self.status = format!("Could not start export: {error:#}");
            }
        }
    }

    pub(super) fn change_format(
        &mut self,
        format: job::OutputFormat,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.format = format;
        if let Some(input) = self.editor_path.clone() {
            let current = self.output_input.read(cx).value().to_string();
            let output = if self.output_touched && !current.trim().is_empty() {
                PathBuf::from(current).with_extension(format.extension())
            } else {
                default_output_path(&input, format)
            };
            self.set_input_value(
                &self.output_input.clone(),
                output.display().to_string(),
                window,
                cx,
            );
        }
        self.status = format!("Output format: {}", format_label(format));
    }

    pub(super) fn launch_dialog(&mut self, kind: DialogKind) {
        let (tx, rx) = mpsc::channel();
        match kind {
            DialogKind::Open => self.open_dialog_rx = Some(rx),
            DialogKind::Folder => self.folder_dialog_rx = Some(rx),
            DialogKind::Output => self.output_dialog_rx = Some(rx),
            DialogKind::DefaultOutput => self.default_output_dialog_rx = Some(rx),
        }
        thread::spawn(move || {
            let path = match kind {
                DialogKind::Open => rfd::FileDialog::new()
                    .set_title("Open video")
                    .add_filter("Video", &["mp4", "mkv", "mov", "webm", "avi", "m4v"])
                    .pick_file(),
                DialogKind::Folder => rfd::FileDialog::new()
                    .set_title("Choose video library")
                    .pick_folder(),
                DialogKind::DefaultOutput => rfd::FileDialog::new()
                    .set_title("Default export folder")
                    .pick_folder(),
                DialogKind::Output => rfd::FileDialog::new()
                    .set_title("Choose export destination")
                    .add_filter("MP4 video", &["mp4"])
                    .add_filter("MKV video", &["mkv"])
                    .add_filter("WAV audio", &["wav"])
                    .save_file(),
            };
            let _ = tx.send(path);
        });
    }

    pub(super) fn show_home(&mut self) {
        self.studio_pause();
        self.native.pause();
        if self.export_job.is_some() {
            self.screen = Screen::Editor;
            self.status = "Finish or cancel current export before returning Home".to_owned();
            return;
        }
        self.waveform_generation = self
            .waveform_worker
            .as_ref()
            .map(waveform::WaveformWorker::cancel)
            .unwrap_or_else(|| self.waveform_generation.wrapping_add(1));
        self.waveform = None;
        self.waveform_path = None;
        self.waveform_error = None;
        self.screen = Screen::Home;
        self.library_refresh_at = Instant::now() - Duration::from_secs(5);
        if let Some(directory) = self.settings.library_directory.clone()
            && !self.library_scanning
        {
            self.scan_library(directory);
        }
        self.status = "Home · newest videos from your library".to_owned();
    }

    pub(super) fn open_folder(&mut self) {
        let Some(path) = self.exported_path.clone() else {
            return;
        };
        let folder = path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .to_path_buf();
        thread::spawn(move || {
            #[cfg(target_os = "linux")]
            let _ = Command::new("xdg-open").arg(&folder).status();
            #[cfg(target_os = "macos")]
            let _ = Command::new("open").arg(&folder).status();
            #[cfg(target_os = "windows")]
            let _ = Command::new("explorer").arg(&folder).status();
        });
    }
}

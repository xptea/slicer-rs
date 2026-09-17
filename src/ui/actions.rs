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

    fn is_project_path(path: &Path) -> bool {
        path.file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.to_ascii_lowercase().ends_with(".slicer.json"))
    }

    /// Inspect selected media off the UI thread, then import each result into
    /// the current model-owned session as an independent timeline instance.
    pub(super) fn add_media_files(&mut self, paths: Vec<PathBuf>) {
        let paths = paths
            .into_iter()
            .filter(|path| path.is_file())
            .collect::<Vec<_>>();
        if paths.is_empty() {
            self.status = "No regular media files were selected".to_owned();
            return;
        }
        let Some(binaries) = self.binaries.clone() else {
            self.status = self
                .binaries_error
                .clone()
                .unwrap_or_else(|| "FFmpeg binaries are unavailable".to_owned());
            return;
        };
        if self.project_session.is_none() {
            self.status = "Open a video before adding media layers".to_owned();
            return;
        }
        let (tx, rx) = mpsc::channel();
        self.import_pending = paths.len();
        self.import_rx = Some(rx);
        if self.uses_composition_preview() {
            self.composition_preview_generation =
                self.composition_preview_generation.wrapping_add(1);
            self.composition_preview_loading = true;
            self.composition_preview_error = None;
        }
        self.status = format!("Inspecting {} media layer(s)…", paths.len());
        for path in paths {
            let binaries = binaries.clone();
            let tx = tx.clone();
            thread::spawn(move || {
                let asset = media::inspect_project_asset(&binaries, &path)
                    .map_err(|error| format!("{error:#}"));
                let _ = tx.send(ImportResult { path, asset });
            });
        }
    }

    fn open_project_file(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        self.native.pause();
        self.screen = Screen::Editor;
        self.export_modal = false;
        self.export_state = ExportState::Idle;
        self.export_progress = 0.0;
        self.exported_path = None;
        self.output_touched = false;
        self.media = None;
        self.media_error = None;
        self.inspecting = false;
        self.inspect_rx = None;
        self.import_rx = None;
        self.import_pending = 0;
        self.waveform = None;
        self.waveform_path = None;
        self.waveform_error = None;
        self.native.set_crop(None);
        self.crop = crop::CropEditor::default();
        self.project_session = None;
        self.composition_preview_image = None;
        self.composition_preview_loading = false;
        self.composition_preview_error = None;
        self.composition_preview_generation = self.composition_preview_generation.wrapping_add(1);
        self.set_input_value(&self.start_input.clone(), "0:00".to_owned(), window, cx);
        self.set_input_value(&self.end_input.clone(), "0:00".to_owned(), window, cx);
        self.set_input_value(
            &self.output_input.clone(),
            path.with_extension("mp4").display().to_string(),
            window,
            cx,
        );
        self.set_input_value(
            &self.filename_input.clone(),
            path.file_stem()
                .and_then(|stem| stem.to_str())
                .unwrap_or("project")
                .to_owned(),
            window,
            cx,
        );
        match session::ProjectSession::open(&path) {
            Ok((loaded, report)) => {
                let video = loaded
                    .project()
                    .assets
                    .values()
                    .find(|asset| asset.kind == project::AssetKind::Video)
                    .cloned();
                let saved_video_edit = loaded
                    .project()
                    .tracks
                    .iter()
                    .flat_map(|track| track.clips.iter())
                    .find_map(|clip| {
                        let project::ClipKind::Video(video) = &clip.kind else {
                            return None;
                        };
                        Some((video.source_range, clip.transform.crop))
                    });
                let missing = report.missing_assets.len();
                self.project_session = Some(loaded);
                if let Some((range, crop)) = saved_video_edit {
                    self.set_input_value(
                        &self.start_input.clone(),
                        format_timestamp(range.start.to_f64()),
                        window,
                        cx,
                    );
                    self.set_input_value(
                        &self.end_input.clone(),
                        format_timestamp(range.end.to_f64()),
                        window,
                        cx,
                    );
                    self.crop.applied = crop.map(|crop| job::CropRect {
                        x: crop.x,
                        y: crop.y,
                        width: crop.width,
                        height: crop.height,
                    });
                    self.native.set_crop(self.crop.applied);
                }
                if let Some(video) = video {
                    let video_path = video.path.clone();
                    self.editor_path = Some(video_path.clone());
                    self.native.prepare_file(&video_path);
                    self.inspecting = true;
                    self.status = if missing == 0 {
                        format!("Opening project {}…", path.display())
                    } else {
                        format!("Project opened with {missing} missing asset(s)")
                    };
                    let bins = self.binaries.clone();
                    let (tx, rx) = mpsc::channel();
                    self.inspect_rx = Some(rx);
                    thread::spawn(move || {
                        let info = match bins {
                            Some(bins) => media::inspect(&bins, &video_path)
                                .map_err(|error| format!("{error:#}")),
                            None => Err("FFmpeg binaries are unavailable".to_owned()),
                        };
                        let _ = tx.send(InspectResult {
                            path: video_path,
                            info,
                        });
                    });
                } else {
                    self.editor_path = None;
                    self.status = format!("Project opened · {missing} missing asset(s)");
                }
                self.request_composition_preview();
            }
            Err(error) => {
                self.editor_path = None;
                self.media_error = Some(error.to_string());
                self.status = format!("Could not open project: {error}");
            }
        }
    }

    pub(super) fn start_project_video_preview(&mut self, path: PathBuf) {
        self.editor_path = Some(path.clone());
        self.native.prepare_file(&path);
        self.inspecting = true;
        let binaries = self.binaries.clone();
        let (tx, rx) = mpsc::channel();
        self.inspect_rx = Some(rx);
        thread::spawn(move || {
            let info = match binaries {
                Some(binaries) => {
                    media::inspect(&binaries, &path).map_err(|error| format!("{error:#}"))
                }
                None => Err("FFmpeg binaries are unavailable".to_owned()),
            };
            let _ = tx.send(InspectResult { path, info });
        });
    }

    pub(super) fn open_file(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        if self.export_job.is_some() {
            self.screen = Screen::Editor;
            self.status = "Finish or cancel current export before opening another".to_owned();
            return;
        }
        if Self::is_project_path(&path) {
            self.open_project_file(path, window, cx);
            return;
        }
        if self.project_session.is_some() {
            self.add_media_files(vec![path]);
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
        self.project_session = None;
        self.composition_preview_image = None;
        self.composition_preview_loading = false;
        self.composition_preview_error = None;
        self.composition_preview_generation = self.composition_preview_generation.wrapping_add(1);
        self.media = None;
        self.media_error = None;
        self.inspecting = true;
        self.inspect_rx = None;
        self.import_rx = None;
        self.import_pending = 0;
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
        let had_project_session = self.project_session.is_some();
        let has_audio = self
            .media
            .as_ref()
            .is_some_and(|media| media.streams.iter().any(|stream| stream.kind == "audio"));
        if let Some(path) = self.editor_path.clone() {
            let dimensions = self.media.as_ref().and_then(|media| {
                media.streams.iter().find_map(|stream| {
                    (stream.kind == "video").then_some((stream.width?, stream.height?))
                })
            });
            if self.project_session.is_none()
                && let (Some((width, height)), Ok(duration_time)) =
                    (dimensions, project::Rational::from_seconds(duration))
                && let Ok(mut layered) = project::SingleVideoAdapter::from_media(
                    path.clone(),
                    duration_time,
                    width,
                    height,
                )
            {
                let asset_id = layered.assets.values().next().map(|asset| asset.id);
                if let Some(asset_id) = asset_id
                    && let Some(asset) = layered.assets.get_mut(asset_id)
                    && let project::AssetMetadata::Video(metadata) = &mut asset.metadata
                {
                    metadata.has_audio = has_audio;
                }
                self.project_session = session::ProjectSession::new(layered).ok();
            }
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
        if !had_project_session {
            self.set_input_value(
                &self.end_input.clone(),
                format_timestamp(duration),
                window,
                cx,
            );
        }
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
        self.request_composition_preview();
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
        self.request_composition_preview();
        let _ = path;
    }

    pub(super) fn request_composition_preview(&mut self) {
        if !self.uses_composition_preview() {
            return;
        }
        let Some(project) = self
            .project_session
            .as_ref()
            .map(|session| session.project().clone())
        else {
            return;
        };
        let Ok(time) = project::Rational::from_seconds(self.preview_seconds.max(0.0)) else {
            self.composition_preview_loading = false;
            self.composition_preview_error = Some("Layered preview time is invalid".to_owned());
            return;
        };
        let generation = self.composition_preview_generation.wrapping_add(1);
        self.composition_preview_generation = generation;
        self.composition_preview_loading = true;
        self.composition_preview_error = None;
        let Some(worker) = self.composition_preview_worker.as_ref() else {
            self.composition_preview_loading = false;
            self.composition_preview_error = Some(
                self.binaries_error
                    .clone()
                    .unwrap_or_else(|| "Layered preview worker is unavailable".to_owned()),
            );
            return;
        };
        worker.request(generation, project, time);
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
        if let (Some(start), Some(end)) = (
            parse_timestamp(&self.start_input.read(cx).value()),
            parse_timestamp(&self.end_input.read(cx).value()),
        ) {
            self.sync_project_trim(start, end);
        }
    }

    /// Keep the layered model in sync with the legacy trim controls while the
    /// native player remains the compatible preview backend for one video.
    fn sync_project_trim(&mut self, start: f64, end: f64) {
        if !start.is_finite() || !end.is_finite() || start < 0.0 || end <= start {
            return;
        }
        let (Some(session), Ok(source_start), Ok(source_end)) = (
            self.project_session.as_mut(),
            project::Rational::from_seconds(start),
            project::Rational::from_seconds(end),
        ) else {
            return;
        };
        let Ok(source_range) = project::TimeRange::new(source_start, source_end) else {
            return;
        };
        let Ok(timeline_range) = project::TimeRange::new(
            project::Time::ZERO,
            source_end
                .checked_sub(source_start)
                .unwrap_or(project::Time::ZERO),
        ) else {
            return;
        };
        let Some(clip_id) = session
            .project()
            .tracks
            .iter()
            .flat_map(|track| track.clips.iter())
            .find(|clip| matches!(clip.kind, project::ClipKind::Video(_)))
            .map(|clip| clip.id)
        else {
            return;
        };
        let current = session.project().clip(clip_id).and_then(|clip| {
            let project::ClipKind::Video(video) = &clip.kind else {
                return None;
            };
            Some((clip.range, video.source_range))
        });
        if current == Some((timeline_range, source_range)) {
            return;
        }
        let _ = session.execute(project::EditCommand::TrimClip {
            clip_id,
            range: timeline_range,
            source_range: Some(source_range),
        });
    }

    pub(super) fn sync_project_crop(&mut self) {
        let Some(session) = self.project_session.as_mut() else {
            return;
        };
        let Some(clip_id) = session
            .project()
            .tracks
            .iter()
            .flat_map(|track| track.clips.iter())
            .find(|clip| matches!(clip.kind, project::ClipKind::Video(_)))
            .map(|clip| clip.id)
        else {
            return;
        };
        let Some(mut transform) = session.project().clip(clip_id).map(|clip| clip.transform) else {
            return;
        };
        transform.crop = self.crop.applied.map(|crop| project::CropRect {
            x: crop.x,
            y: crop.y,
            width: crop.width,
            height: crop.height,
        });
        let _ = session.execute(project::EditCommand::SetClipTransform { clip_id, transform });
    }

    pub(super) fn save_project(&mut self) {
        if self.import_pending > 0 {
            self.status = format!(
                "Wait for {} media inspection(s) to finish before saving",
                self.import_pending
            );
            return;
        }
        let Some(session) = self.project_session.as_mut() else {
            self.status = "Open media before saving a project".to_owned();
            return;
        };
        let path = session.project_path().map(Path::to_path_buf).or_else(|| {
            self.editor_path
                .as_ref()
                .map(|path| path.with_extension("slicer.json"))
        });
        let Some(path) = path else {
            self.status = "Choose media before saving a project".to_owned();
            return;
        };
        match session.save_as(path.clone()) {
            Ok(path) => self.status = format!("Project saved · {}", path.display()),
            Err(error) => self.status = format!("Could not save project: {error}"),
        }
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
        if event.keystroke.key == "s" && event.keystroke.modifiers.secondary() {
            self.save_project();
            cx.stop_propagation();
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
        let input = self.editor_path.clone();
        let (_media_duration, start, end) = if let Some(info) = self.media.as_ref() {
            let media_duration = info.duration;
            let start_text = self.start_input.read(cx).value().to_string();
            let end_text = self.end_input.read(cx).value().to_string();
            let (Some(start), Some(end)) =
                (parse_timestamp(&start_text), parse_timestamp(&end_text))
            else {
                self.status = "Enter valid timestamps such as 0:12 or 00:01:24".to_owned();
                self.export_state = ExportState::Failed;
                return;
            };
            self.sync_project_trim(start, end);
            if start < 0.0 || end <= start || end > media_duration + 0.05 {
                self.status = format!(
                    "Choose a range between 0:00 and {}",
                    format_timestamp(media_duration)
                );
                self.export_state = ExportState::Failed;
                return;
            }
            (media_duration, start, end)
        } else if let Some(session) = self.project_session.as_ref()
            && let Some(range) = session.project().output_range()
        {
            // Audio-only projects have no native preview/media inspector. The
            // persisted output range is already exact and is sufficient for
            // the composition audio exporter.
            (range.end.to_f64(), range.start.to_f64(), range.end.to_f64())
        } else {
            self.status = "Wait for media inspection to finish".to_owned();
            return;
        };
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

        let layered_project = self.project_session.as_ref().filter(|session| {
            // A reopened project must render from its persisted graph even
            // when it happens to contain one video clip; the legacy controls
            // are only a fast path for an unsaved, freshly opened source.
            session.project_path().is_some()
                || session
                    .project()
                    .tracks
                    .iter()
                    .flat_map(|track| track.clips.iter())
                    .count()
                    != 1
                || session
                    .project()
                    .tracks
                    .iter()
                    .flat_map(|track| track.clips.iter())
                    .any(|clip| !matches!(clip.kind, project::ClipKind::Video(_)))
        });
        if let Some(session) = layered_project {
            let Some(format) = (match self.format {
                job::OutputFormat::Mp4 => Some(export::CompositionFormat::Mp4),
                job::OutputFormat::Mkv => Some(export::CompositionFormat::Mkv),
                job::OutputFormat::Gif => Some(export::CompositionFormat::Gif),
                job::OutputFormat::Wav => Some(export::CompositionFormat::Wav),
                job::OutputFormat::Webm | job::OutputFormat::Mp3 => None,
            }) else {
                self.export_state = ExportState::Failed;
                self.status =
                    "Layered composition export currently supports MP4, MKV, GIF, and WAV"
                        .to_owned();
                return;
            };
            let request = export::CompositionExportRequest {
                project: session.project().clone(),
                binaries,
                output,
                range: None,
                format,
                quality: self.quality,
                render_options: Default::default(),
            };
            match export::spawn_composition_export(request) {
                Ok(handle) => {
                    self.export_job = Some(UiExportJob::Composition(handle));
                    self.export_state = ExportState::Running;
                    self.export_progress = 0.0;
                    self.exported_path = None;
                    self.status = "Layered export queued in the background".to_owned();
                }
                Err(error) => {
                    self.export_state = ExportState::Failed;
                    self.status = format!("Could not start composition export: {error}");
                }
            }
            return;
        }

        let Some(input) = input else {
            self.status = "A native media input is required for this export".to_owned();
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
                self.export_job = Some(UiExportJob::Legacy(handle));
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
        match kind {
            DialogKind::Open => {
                let (tx, rx) = mpsc::channel();
                self.open_dialog_rx = Some(rx);
                thread::spawn(move || {
                    let path = rfd::FileDialog::new()
                        .set_title("Open media or project")
                        .add_filter(
                            "Media",
                            &[
                                "mp4", "mkv", "mov", "webm", "avi", "m4v", "png", "jpg", "jpeg",
                                "wav", "mp3",
                            ],
                        )
                        .add_filter("Slicer project", &["slicer.json"])
                        .pick_file();
                    let _ = tx.send(path);
                });
            }
            DialogKind::AddMedia => {
                let (tx, rx) = mpsc::channel();
                self.add_media_dialog_rx = Some(rx);
                thread::spawn(move || {
                    let paths = rfd::FileDialog::new()
                        .set_title("Add media layers")
                        .add_filter(
                            "Media",
                            &[
                                "mp4", "mkv", "mov", "webm", "avi", "m4v", "png", "jpg", "jpeg",
                                "wav", "mp3",
                            ],
                        )
                        .pick_files();
                    let _ = tx.send(paths);
                });
            }
            DialogKind::Folder => {
                let (tx, rx) = mpsc::channel();
                self.folder_dialog_rx = Some(rx);
                thread::spawn(move || {
                    let path = rfd::FileDialog::new()
                        .set_title("Choose video library")
                        .pick_folder();
                    let _ = tx.send(path);
                });
            }
            DialogKind::DefaultOutput => {
                let (tx, rx) = mpsc::channel();
                self.default_output_dialog_rx = Some(rx);
                thread::spawn(move || {
                    let path = rfd::FileDialog::new()
                        .set_title("Choose default export folder")
                        .pick_folder();
                    let _ = tx.send(path);
                });
            }
            DialogKind::Output => {
                let (tx, rx) = mpsc::channel();
                self.output_dialog_rx = Some(rx);
                thread::spawn(move || {
                    let path = rfd::FileDialog::new()
                        .set_title("Choose export destination")
                        .add_filter("MP4 video", &["mp4"])
                        .add_filter("MKV video", &["mkv"])
                        .add_filter("WAV audio", &["wav"])
                        .save_file();
                    let _ = tx.send(path);
                });
            }
        }
    }

    pub(super) fn show_home(&mut self) {
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

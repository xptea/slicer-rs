//! Native GPUI Kit desktop interface for Slicer.
//!
//! The view deliberately keeps media inspection, preview generation, library
//! scanning, dialogs, and exports outside the render path.  GPUI is used only
//! to present the current state and to dispatch user actions back to the
//! worker queues.

mod actions;
mod crop;
mod editor;
mod export_controls;
mod export_toast;
mod file_clipboard;
mod file_drop;
mod formatting;
mod home_view;
mod native_preview;
mod native_surface;
mod navigation;
mod playhead_clock;
mod preview_panel;
mod settings_view;
mod studio;
mod theme;
mod timeline;
mod window_frame;
mod workers;

use formatting::*;
use theme::*;

use crate::{home, job, media, preview, waveform};
use gpui_kit::component::Size as KitSize;
use gpui_kit::component::slider::{SliderEvent, SliderState, SliderValue};
use gpui_kit::component::{
    button::{Button, ButtonVariants},
    input::{Input, InputEvent, InputState},
    progress::Progress,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{component::*, gpui::*, platform};
use slicer::native_player;
use std::{
    path::{Path, PathBuf},
    process::Command,
    sync::{Arc, mpsc},
    thread,
    time::{Duration, Instant, SystemTime},
};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Screen {
    Home,
    Editor,
    Settings,
    Studio,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ExportState {
    Idle,
    Running,
    Completed,
    Cancelled,
    Failed,
}

#[derive(Clone)]
struct ThumbnailSlot {
    path: PathBuf,
    modified: SystemTime,
    image: Option<Arc<Image>>,
    error: Option<String>,
}

struct ScanResult {
    generation: u64,
    directory: PathBuf,
    videos: Result<Vec<home::RecentVideo>, String>,
}

struct InspectResult {
    path: PathBuf,
    info: Result<media::MediaInfo, String>,
}

enum DialogKind {
    Open,
    Folder,
    Output,
    DefaultOutput,
}

/// The application view.  All receivers are polled from a lightweight GPUI
/// timer so no filesystem or FFmpeg call can hold up a frame.
pub struct SlicerApp {
    studio: Option<studio::Studio>,
    binaries: Option<media::Binaries>,
    binaries_error: Option<String>,
    screen: Screen,
    external_drop: Option<ExternalPaths>,
    focus_handle: FocusHandle,

    settings: home::Settings,
    settings_loading: bool,
    settings_error: Option<String>,
    settings_rx: Option<mpsc::Receiver<Result<home::Settings, String>>>,
    settings_save_rx: Option<mpsc::Receiver<Result<(), String>>>,
    settings_save_pending: bool,

    recent: Vec<home::RecentVideo>,
    thumbnails: Vec<ThumbnailSlot>,
    library_scanning: bool,
    library_error: Option<String>,
    scan_rx: Option<mpsc::Receiver<ScanResult>>,
    scan_generation: u64,
    library_refresh_at: Instant,
    thumbnail_worker: Option<preview::PreviewWorker>,
    thumbnail_pending: Option<(PathBuf, SystemTime)>,
    waveform_worker: Option<waveform::WaveformWorker>,
    waveform: Option<Arc<waveform::Waveform>>,
    waveform_path: Option<PathBuf>,
    waveform_generation: u64,
    waveform_error: Option<String>,

    editor_path: Option<PathBuf>,
    media: Option<media::MediaInfo>,
    media_error: Option<String>,
    inspecting: bool,
    inspect_rx: Option<mpsc::Receiver<InspectResult>>,
    preview_seconds: f64,
    preview_error: Option<String>,
    native: native_preview::NativePreview,
    export_modal: bool,
    crop: crop::CropEditor,
    toast: Option<export_toast::ExportToast>,
    clipboard: Option<file_clipboard::FileClipboard>,
    clipboard_rx: Option<mpsc::Receiver<Result<(), String>>>,
    filename_input: Entity<InputState>,
    timeline_bounds: Option<Bounds<Pixels>>,
    timeline_drag: Option<timeline::DragTarget>,

    start_input: Entity<InputState>,
    end_input: Entity<InputState>,
    output_input: Entity<InputState>,
    output_touched: bool,
    format: job::OutputFormat,
    mode: job::TrimMode,
    quality: u8,
    quality_slider: Entity<SliderState>,
    default_quality_slider: Entity<SliderState>,

    export_job: Option<job::JobHandle>,
    export_state: ExportState,
    export_progress: f64,
    exported_path: Option<PathBuf>,
    status: String,

    open_dialog_rx: Option<mpsc::Receiver<Option<PathBuf>>>,
    folder_dialog_rx: Option<mpsc::Receiver<Option<PathBuf>>>,
    output_dialog_rx: Option<mpsc::Receiver<Option<PathBuf>>>,
    default_output_dialog_rx: Option<mpsc::Receiver<Option<PathBuf>>>,
    _subscriptions: Vec<Subscription>,
}

impl SlicerApp {
    fn new(
        window: &mut Window,
        cx: &mut Context<Self>,
        binaries: Option<media::Binaries>,
        binaries_error: Option<String>,
    ) -> Self {
        let start_input = cx.new(|cx| InputState::new(window, cx).default_value("0:00"));
        let end_input = cx.new(|cx| InputState::new(window, cx).default_value("0:00"));
        let output_input = cx.new(|cx| InputState::new(window, cx).default_value(""));

        let quality_slider = cx.new(|_| {
            SliderState::new()
                .min(50.)
                .max(100.)
                .step(1.)
                .default_value(100.)
        });
        let default_quality_slider = cx.new(|_| {
            SliderState::new()
                .min(50.)
                .max(100.)
                .step(1.)
                .default_value(100.)
        });
        let mut subscriptions = Vec::new();
        subscriptions.push(cx.subscribe(&quality_slider, |this, _, event, cx| {
            if let SliderEvent::Change(SliderValue::Single(value)) = event {
                this.quality = (*value as u8).clamp(50, 100);
                cx.notify();
            }
        }));
        subscriptions.push(cx.subscribe(
            &default_quality_slider,
            |this, _, event, cx| match event {
                SliderEvent::Change(SliderValue::Single(value)) => {
                    this.settings.export_defaults.quality = (*value as u8).clamp(50, 100);
                    cx.notify();
                }
                SliderEvent::Release(_) => this.save_settings(),
                _ => {}
            },
        ));
        subscriptions.push(cx.subscribe(&start_input, |this, input, event, cx| {
            if matches!(event, InputEvent::Change) {
                this.sync_preview_from_input(&input, cx);
                cx.notify();
            }
        }));
        subscriptions.push(cx.subscribe(&end_input, |this, input, event, cx| {
            if matches!(event, InputEvent::Change) {
                this.sync_preview_from_input(&input, cx);
                cx.notify();
            }
        }));
        subscriptions.push(cx.subscribe(&output_input, |this, _, event, cx| {
            if matches!(event, InputEvent::Change) {
                this.output_touched = true;
                cx.notify();
            }
        }));

        let settings_rx = spawn_settings_load();
        let thumbnail_worker = binaries.clone().map(preview::PreviewWorker::new);
        let mut app = Self {
            binaries: binaries.clone(),
            binaries_error,
            studio: None,
            screen: Screen::Home,
            external_drop: None,
            focus_handle: cx.focus_handle(),
            settings: home::Settings::default(),
            settings_loading: true,
            settings_error: None,
            settings_rx: Some(settings_rx),
            settings_save_rx: None,
            settings_save_pending: false,
            recent: Vec::new(),
            thumbnails: Vec::new(),
            library_scanning: false,
            library_error: None,
            scan_rx: None,
            scan_generation: 0,
            library_refresh_at: Instant::now(),
            thumbnail_worker,
            thumbnail_pending: None,
            waveform_worker: binaries.clone().map(waveform::WaveformWorker::new),
            waveform: None,
            waveform_path: None,
            waveform_generation: 0,
            waveform_error: None,
            editor_path: None,
            media: None,
            media_error: None,
            inspecting: false,
            inspect_rx: None,
            preview_seconds: 0.0,
            preview_error: None,
            native: native_preview::NativePreview::default(),
            export_modal: false,
            crop: crop::CropEditor::default(),
            toast: None,
            clipboard: None,
            clipboard_rx: None,
            filename_input: cx.new(|cx| InputState::new(window, cx)),
            timeline_bounds: None,
            timeline_drag: None,
            start_input,
            end_input,
            output_input,
            output_touched: false,
            format: job::OutputFormat::Mp4,
            mode: job::TrimMode::Exact,
            quality: 100,
            quality_slider,
            default_quality_slider,
            export_job: None,
            export_state: ExportState::Idle,
            export_progress: 0.0,
            exported_path: None,
            status: "Choose a library folder or drop a video to get started".to_owned(),
            open_dialog_rx: None,
            folder_dialog_rx: None,
            output_dialog_rx: None,
            default_output_dialog_rx: None,
            _subscriptions: subscriptions,
        };

        // Keep Home lightweight by default. The native player now loads the
        // reduced runtime quickly enough that allocating a second GPU context
        // before a video is chosen is not worth the idle memory cost. Set
        // SLICER_PREWARM=1 for machines where instant first-frame startup is
        // more important than the extra graphics allocation.
        if std::env::var_os("SLICER_PREWARM").is_some_and(|value| value == "1") {
            app.native.prewarm(window);
        }

        // Keep the app repainting while worker channels have pending results.
        // This is a timer only; all substantive work occurs in the workers.
        cx.spawn(async move |view, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(33))
                    .await;
                if view.update(cx, |_, cx| cx.notify()).is_err() {
                    break;
                }
            }
        })
        .detach();

        window.focus(&app.focus_handle, cx);
        app
    }
}

impl Render for SlicerApp {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.poll_background(window, cx);
        self.poll_studio(window);
        if !cx.has_active_drag() {
            self.external_drop = None;
        }
        if self.screen == Screen::Editor && !self.export_modal && !self.native.paused {
            window.request_animation_frame();
        }
        if self.screen != Screen::Editor
            || self.export_modal
            || self.crop.is_ready()
            || self.external_drop.is_some()
        {
            self.native.hide();
        }
        let content = match self.screen {
            Screen::Home => self.home_view(cx),
            Screen::Editor => self.editor_view(cx),
            Screen::Settings => self.settings_view(cx),
            Screen::Studio => self.studio_view(cx),
        };
        let view = v_flex()
            .size_full()
            .text_color(ink(TEXT))
            .key_context("Slicer")
            .track_focus(&self.focus_handle)
            .on_key_down(cx.listener(|this, event, window, cx| this.handle_key(event, window, cx)))
            .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, window, cx| {
                if this.screen == Screen::Studio {
                    this.studio_drag(
                        event.position,
                        event.pressed_button == Some(MouseButton::Left),
                        cx,
                    );
                } else if this.crop.open {
                    if event.pressed_button == Some(MouseButton::Left) {
                        this.drag_crop(event.position, event.modifiers.shift);
                    } else {
                        this.finish_crop_drag();
                    }
                    cx.notify();
                } else if this.timeline_drag.is_some() {
                    // A release over the native video child can bypass GPUI.
                    // Never keep issuing seeks after the physical drag ended.
                    if event.pressed_button == Some(MouseButton::Left) {
                        this.drag_timeline(event.position.x, window, cx);
                    } else {
                        this.finish_timeline_drag(cx);
                    }
                }
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, event: &MouseUpEvent, _, cx| {
                    if this.screen == Screen::Studio {
                        this.studio_drag(event.position, false, cx);
                    }
                    this.finish_crop_drag();
                    this.finish_timeline_drag(cx);
                }),
            )
            .on_drag_move::<ExternalPaths>(cx.listener(
                |this, event: &DragMoveEvent<ExternalPaths>, _, cx| {
                    if let Some(paths) = event.dragged_item().downcast_ref::<ExternalPaths>() {
                        this.external_drop = Some(paths.clone());
                        cx.notify();
                    }
                },
            ))
            .child(self.header(cx))
            .child(
                div()
                    .flex_1()
                    .min_h(px(0.))
                    .overflow_hidden()
                    .child(content),
            );
        let mut layer = div()
            .relative()
            .size_full()
            .child(view)
            .child(self.file_drop_listener(cx));
        if self.export_modal {
            layer = layer.child(self.export_dialog(window, cx));
        }
        if self.toast.is_some() {
            layer = layer.child(self.export_toast(cx));
        }
        if self.external_drop.is_some() && self.screen != Screen::Home {
            layer = layer.child(
                div()
                    .absolute()
                    .inset_0()
                    .m(px(CONTENT_GUTTER))
                    .rounded(px(18.))
                    .border_3()
                    .border_color(ink(TEXT))
                    .bg(rgba(0x090909ee))
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(div().text_xl().font_bold().child("Drop video to open")),
            );
        }
        window_frame::frame(layer, window, cx)
    }
}

pub fn run(initial_file: Option<PathBuf>, studio: bool) {
    let binaries = match media::Binaries::resolve() {
        Ok(binaries) => Some(binaries),
        Err(error) => {
            eprintln!("Slicer: {error:#}");
            None
        }
    };
    let binaries_error = binaries.is_none().then(|| {
        "Bundled FFmpeg was not found. Set SLICER_FFMPEG_DIR for development or install the packaged codecs.".to_owned()
    });
    platform::application()
        .with_assets(gpui_kit::assets::AllAssets)
        .run(move |cx| {
            gpui_kit::init(cx);
            theme::apply(cx);
            let options = WindowOptions {
                window_bounds: Some(WindowBounds::centered(
                    size(
                        px(if studio { 1280. } else { 800. }),
                        px(if studio { 840. } else { 720. }),
                    ),
                    cx,
                )),
                titlebar: Some(TitlebarOptions {
                    title: Some("Slicer".into()),
                    ..TitlebarOptions::default()
                }),
                window_min_size: Some(size(px(580.), px(600.))),
                #[cfg(target_os = "linux")]
                window_decorations: Some(WindowDecorations::Client),
                #[cfg(feature = "desktop")]
                icon: app_icon(),
                app_id: Some("com.slicer.Slicer".to_owned()),
                window_background: WindowBackgroundAppearance::Transparent,
                ..WindowOptions::default()
            };
            cx.open_window(options, move |window, cx| {
                let view = cx.new(|cx| {
                    let mut app =
                        SlicerApp::new(window, cx, binaries.clone(), binaries_error.clone());
                    if studio {
                        app.enter_studio();
                        if let Some(path) = initial_file.clone() {
                            app.studio_open_project(path);
                        }
                    } else if let Some(path) = initial_file.clone() {
                        app.open_file(path, window, cx);
                    }
                    app
                });
                let closing_view = view.downgrade();
                window.on_window_should_close(cx, move |_, cx| {
                    let _ = closing_view.update(cx, |this, _| {
                        this.studio = None;
                        this.native.shutdown()
                    });
                    true
                });
                cx.new(|cx| {
                    Root::new(view, window, cx)
                        .bordered(false)
                        .bg(transparent_black())
                })
            })
            .expect("failed to open Slicer window");
        });
}

/// The same checked-in image is used for the native X11 window icon and the
/// desktop launcher. Decode it once while constructing the window so the
/// render loop never performs file or image work.
fn app_icon() -> Option<Arc<::image::RgbaImage>> {
    ::image::load_from_memory(include_bytes!(
        "../resources/icons/hicolor/256x256/apps/slicer.png"
    ))
    .ok()
    .map(|image| Arc::new(image.to_rgba8()))
}

fn spawn_settings_load() -> mpsc::Receiver<Result<home::Settings, String>> {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let _ = tx.send(home::Settings::load().map_err(|error| format!("{error:#}")));
    });
    rx
}

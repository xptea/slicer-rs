//! Export controls, format selection, export customization, and progress.

use super::*;
use gpui_kit::component::slider::Slider;

impl SlicerApp {
    /// Open the export customization dialog and seed its file name from the
    /// current destination. The main header action uses the default-export
    /// path supplied by `start_default_export` instead.
    pub(super) fn open_export_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.native.pause();
        if !self.export_modal && self.export_job.is_none() {
            self.prepare_export_defaults(window, cx);
            let output = PathBuf::from(self.output_input.read(cx).value().to_string());
            let name = output
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("clip");
            self.set_input_value(&self.filename_input.clone(), name.to_owned(), window, cx);
        }
        self.export_modal = true;
        window.focus(
            self.filename_input.read(cx).presentation().focus_handle(),
            cx,
        );
    }

    pub(super) fn export_dialog(&self, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let busy = self.export_job.is_some();
        let destination = PathBuf::from(self.output_input.read(cx).value().to_string());
        let folder = destination.parent().unwrap_or_else(|| Path::new("."));

        div()
            .absolute()
            .inset_0()
            .size_full()
            .bg(rgba(0x000000b0))
            .rounded_b(if window.is_maximized() || window.is_fullscreen() {
                px(0.)
            } else {
                px(18.)
            })
            .id("export-overlay")
            .occlude()
            .flex()
            .items_center()
            .justify_center()
            .p_4()
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(
                v_flex()
                    .w_full()
                    .max_w(px(540.))
                    .max_h(relative(1.))
                    .p_5()
                    .gap_4()
                    .rounded(px(18.))
                    .border_1()
                    .border_color(ink(BORDER))
                    .bg(ink(SURFACE))
                    .id("export-dialog")
                    .overflow_y_scroll()
                    .child(
                        h_flex()
                            .w_full()
                            .justify_between()
                            .items_center()
                            .child(div().text_lg().font_semibold().child("Customize export"))
                            .child(
                                Button::new("close-export")
                                    .ghost()
                                    .with_size(KitSize::Large)
                                    .icon(gpui_kit::assets::IconName::X)
                                    .accessibility_label("Close customize export")
                                    .disabled(busy)
                                    .on_click(
                                        cx.listener(|this, _, _, _| this.export_modal = false),
                                    ),
                            ),
                    )
                    .child(field_label(
                        "File name",
                        Input::new(&self.filename_input)
                            .disabled(busy)
                            .into_any_element(),
                    ))
                    .child(
                        h_flex()
                            .gap_2()
                            .child(
                                div()
                                    .flex_1()
                                    .min_w(px(0.))
                                    .text_sm()
                                    .text_color(ink(MUTED))
                                    .truncate()
                                    .child(folder.display().to_string()),
                            )
                            .child(
                                Button::new("output-browse")
                                    .secondary()
                                    .compact()
                                    .label("Choose location")
                                    .disabled(busy)
                                    .on_click(cx.listener(|this, _, _, _| {
                                        this.launch_dialog(DialogKind::Output)
                                    })),
                            ),
                    )
                    .child(
                        v_flex()
                            .gap_2()
                            .child(div().text_color(ink(MUTED)).text_sm().child("Format"))
                            .child(
                                h_flex()
                                    .gap_2()
                                    .child(self.format_button(job::OutputFormat::Mp4, cx))
                                    .child(self.format_button(job::OutputFormat::Mkv, cx))
                                    .child(self.format_button(job::OutputFormat::Wav, cx))
                                    .child(self.format_button(job::OutputFormat::Gif, cx)),
                            ),
                    )
                    .child(
                        v_flex()
                            .gap_2()
                            .child(
                                h_flex()
                                    .justify_between()
                                    .child(div().text_color(ink(MUTED)).text_sm().child("Quality"))
                                    .child(
                                        div()
                                            .text_sm()
                                            .text_color(ink(TEXT))
                                            .child(format!("{}%", self.quality.clamp(50, 100))),
                                    ),
                            )
                            .child(
                                Slider::new(&self.quality_slider)
                                    .disabled(busy)
                                    .h(px(28.))
                                    .w_full(),
                            )
                            .child(
                                h_flex()
                                    .w_full()
                                    .justify_between()
                                    .text_xs()
                                    .text_color(ink(MUTED))
                                    .child("50 · Smaller file")
                                    .child("100 · Best quality"),
                            ),
                    )
                    .child(self.export_status(cx))
                    .child(
                        Button::new("export-start")
                            .primary()
                            .label(if busy { "Exporting…" } else { "Export" })
                            .disabled(busy)
                            .on_click(cx.listener(|this, _, _, cx| this.start_export(cx))),
                    ),
            )
            .into_any_element()
    }

    pub(super) fn format_button(
        &self,
        format: job::OutputFormat,
        cx: &mut Context<Self>,
    ) -> Button {
        Button::new(format!(
            "format-{}",
            format_label(format).to_ascii_lowercase()
        ))
        .when(self.format == format, |button| button.primary())
        .when(self.format != format, |button| button.secondary())
        .disabled(self.export_job.is_some())
        .label(format_label(format))
        .on_click(cx.listener(move |this, _, window, cx| this.change_format(format, window, cx)))
    }

    pub(super) fn export_status(&self, cx: &mut Context<Self>) -> AnyElement {
        match self.export_state {
            ExportState::Running => div()
                .v_flex()
                .gap_2()
                .child(
                    h_flex()
                        .justify_between()
                        .text_color(ink(MUTED))
                        .text_sm()
                        .child("Export in progress")
                        .child(format!("{:.0}%", self.export_progress * 100.0)),
                )
                .child(
                    h_flex()
                        .gap_3()
                        .child(
                            Progress::new("export-progress")
                                .value((self.export_progress * 100.0) as f32)
                                .color(ink(ACCENT_STRONG))
                                .with_size(KitSize::Small)
                                .w_full(),
                        )
                        .child(
                            Button::new("export-cancel")
                                .danger()
                                .compact()
                                .label("Cancel")
                                .on_click(cx.listener(|this, _, _, _| {
                                    if let Some(job) = this.export_job.as_ref() {
                                        job.cancel();
                                        this.status = "Cancelling export…".to_owned();
                                    }
                                })),
                        ),
                )
                .into_any_element(),
            ExportState::Completed => h_flex()
                .justify_between()
                .text_color(ink(GOOD))
                .child("Export complete")
                .child(
                    Button::new("export-open-folder")
                        .ghost()
                        .compact()
                        .label("Open folder")
                        .on_click(cx.listener(|this, _, _, _| this.open_folder())),
                )
                .into_any_element(),
            ExportState::Cancelled => div()
                .text_color(ink(WARN))
                .text_sm()
                .child("Export cancelled")
                .into_any_element(),
            ExportState::Failed => div()
                .text_color(ink(BAD))
                .text_sm()
                .child(self.status.clone())
                .into_any_element(),
            ExportState::Idle => div().into_any_element(),
        }
    }
}

pub(super) fn field_label(label: &'static str, input: AnyElement) -> AnyElement {
    v_flex()
        .gap_1()
        .child(div().text_color(ink(MUTED)).text_sm().child(label))
        .child(input)
        .into_any_element()
}

//! Library and default-export settings.

use super::*;
use gpui_kit::component::{checkbox::Checkbox, slider::Slider};

impl SlicerApp {
    pub(super) fn settings_view(&self, cx: &mut Context<Self>) -> AnyElement {
        let current_library = self
            .settings
            .library_directory
            .as_ref()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| "No folder selected".to_owned());
        let current_output = self
            .settings
            .export_defaults
            .output_directory
            .as_ref()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| "Next to the source video".to_owned());

        let choose_library = Button::new("settings-choose-library")
            .primary()
            .label("Choose folder")
            .on_click(cx.listener(|this, _, _, _| this.launch_dialog(DialogKind::Folder)));
        let choose_output = Button::new("settings-choose-output")
            .primary()
            .label("Choose folder")
            .on_click(cx.listener(|this, _, _, _| this.launch_dialog(DialogKind::DefaultOutput)));
        let home = Button::new("settings-home")
            .ghost()
            .disabled(self.export_job.is_some())
            .label("Back to Home")
            .on_click(cx.listener(|this, _, _, _| this.show_home()));

        v_flex()
            .size_full()
            .id("settings-scroll")
            .overflow_y_scroll()
            .px_6()
            .py_4()
            .gap_6()
            .child(
                v_flex()
                    .gap_4()
                    .max_w(px(760.))
                    .child(
                        v_flex().gap_1().child(
                            div()
                                .text_color(ink(TEXT))
                                .font_semibold()
                                .child("Video library"),
                        ),
                    )
                    .child(
                        h_flex()
                            .gap_3()
                            .p_3()
                            .rounded(px(8.))
                            .bg(ink(SURFACE))
                            .border_1()
                            .border_color(ink(BORDER))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w(px(0.))
                                    .text_color(ink(TEXT))
                                    .truncate()
                                    .child(current_library),
                            )
                            .child(choose_library),
                    ),
            )
            .child(
                v_flex()
                    .gap_4()
                    .max_w(px(760.))
                    .child(
                        v_flex().gap_1().child(
                            div()
                                .text_color(ink(TEXT))
                                .font_semibold()
                                .child("Export defaults"),
                        ),
                    )
                    .child(
                        v_flex()
                            .gap_2()
                            .child(div().text_color(ink(MUTED)).text_sm().child("Format"))
                            .child(
                                h_flex()
                                    .gap_2()
                                    .child(self.default_format_button(home::ExportFormat::Mp4, cx))
                                    .child(self.default_format_button(home::ExportFormat::Mkv, cx))
                                    .child(self.default_format_button(home::ExportFormat::Wav, cx))
                                    .child(self.default_format_button(home::ExportFormat::Gif, cx)),
                            ),
                    )
                    .child(
                        v_flex()
                            .gap_2()
                            .child(
                                h_flex()
                                    .justify_between()
                                    .child(div().text_color(ink(MUTED)).text_sm().child("Quality"))
                                    .child(div().text_sm().text_color(ink(TEXT)).child(format!(
                                        "{}%",
                                        self.settings.export_defaults.quality.clamp(50, 100)
                                    ))),
                            )
                            .child(
                                Slider::new(&self.default_quality_slider)
                                    .disabled(self.export_job.is_some())
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
                    .child(
                        div()
                            .text_sm()
                            .text_color(ink(MUTED))
                            .child("Export folder"),
                    )
                    .child(
                        h_flex()
                            .gap_3()
                            .p_3()
                            .rounded(px(8.))
                            .bg(ink(SURFACE))
                            .border_1()
                            .border_color(ink(BORDER))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w(px(0.))
                                    .text_color(ink(TEXT))
                                    .truncate()
                                    .child(current_output),
                            )
                            .child(choose_output),
                    )
                    .child(
                        Checkbox::new("settings-copy-to-clipboard")
                            .checked(self.settings.export_defaults.copy_to_clipboard)
                            .label("Copy exported clip to clipboard")
                            .on_click(cx.listener(|this, checked, _, _| {
                                this.settings.export_defaults.copy_to_clipboard = *checked;
                                this.save_settings();
                            })),
                    ),
            )
            .when_some(self.settings_error.clone(), |this, error| {
                this.child(
                    div()
                        .max_w(px(760.))
                        .text_color(ink(BAD))
                        .text_sm()
                        .child(error),
                )
            })
            .child(h_flex().w_full().justify_center().child(home))
            .into_any_element()
    }

    fn default_format_button(&self, format: home::ExportFormat, cx: &mut Context<Self>) -> Button {
        Button::new(format!("settings-format-{}", format.extension()))
            .when(self.settings.export_defaults.format == format, |button| {
                button.primary()
            })
            .when(self.settings.export_defaults.format != format, |button| {
                button.secondary()
            })
            .disabled(self.export_job.is_some())
            .label(format.label())
            .on_click(cx.listener(move |this, _, _, _| {
                this.settings.export_defaults.format = format;
                this.save_settings();
                this.status = format!("Default export format: {}", format.label());
            }))
    }
}

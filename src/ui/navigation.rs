//! Left-aligned navigation with a split editor export action on the right.

use super::*;

impl SlicerApp {
    pub(super) fn header(&self, cx: &mut Context<Self>) -> AnyElement {
        let media_import_busy = self.import_pending > 0;
        let export_ready = self.media.is_some()
            || self
                .project_session
                .as_ref()
                .is_some_and(|session| session.project().output_range().is_some());
        let home = Button::new("nav-home")
            .ghost()
            .compact()
            .rounded(px(10.))
            .accessibility_label(if self.screen == Screen::Editor {
                "Back"
            } else {
                "Home"
            })
            .child(
                div()
                    .text_size(px(16.))
                    .child(if self.screen == Screen::Editor {
                        "Back"
                    } else {
                        "Home"
                    }),
            )
            .px_3()
            .selected(self.screen == Screen::Home)
            .disabled(self.export_job.is_some())
            .on_click(cx.listener(|this, _, _, _| this.show_home()));
        let settings = Button::new("nav-settings")
            .ghost()
            .compact()
            .rounded(px(10.))
            .accessibility_label("Settings")
            .child(div().text_size(px(16.)).child("Settings"))
            .px_3()
            .selected(self.screen == Screen::Settings)
            .disabled(self.export_job.is_some())
            .on_click(cx.listener(|this, _, _, _| {
                if this.export_job.is_none() {
                    this.native.pause();
                    this.screen = Screen::Settings;
                }
            }));
        h_flex()
            .w_full()
            .h(px(56.))
            .flex_shrink_0()
            .px(px(CONTENT_GUTTER))
            .gap_2()
            .when(self.screen != Screen::Editor, |header| {
                header.justify_center()
            })
            .child(home)
            .when(self.screen != Screen::Editor, |header| {
                header.child(settings)
            })
            .when(self.screen == Screen::Editor, |header| {
                header.child(div().flex_1())
            })
            .when(self.screen == Screen::Editor, |header| {
                header.child(
                    Button::new("add-media")
                        .secondary()
                        .compact()
                        .label(if media_import_busy {
                            "Importing…"
                        } else {
                            "Add media"
                        })
                        .disabled(
                            self.project_session.is_none()
                                || self.export_job.is_some()
                                || media_import_busy,
                        )
                        .on_click(cx.listener(|this, _, _, _| {
                            if this.project_session.is_some()
                                && this.export_job.is_none()
                                && this.import_pending == 0
                            {
                                this.launch_dialog(DialogKind::AddMedia);
                            }
                        })),
                )
            })
            .when(self.screen == Screen::Editor, |header| {
                header.child(
                    Button::new("save-project")
                        .secondary()
                        .compact()
                        .label(
                            if self
                                .project_session
                                .as_ref()
                                .is_some_and(|session| session.is_dirty())
                            {
                                "Save*"
                            } else {
                                "Save"
                            },
                        )
                        .disabled(
                            self.project_session.is_none()
                                || self.export_job.is_some()
                                || media_import_busy,
                        )
                        .on_click(cx.listener(|this, _, _, _| this.save_project())),
                )
            })
            .when(self.screen == Screen::Editor, |header| {
                header.child(
                    h_flex()
                        .gap_0()
                        .bg(ink(TEXT))
                        .rounded(px(10.))
                        .overflow_hidden()
                        .child(
                            Button::new("export-default")
                                .primary()
                                .rounded(px(0.))
                                .rounded_l(px(10.))
                                .label(if self.export_job.is_some() {
                                    format!("Exporting {:.0}%", self.export_progress * 100.)
                                } else {
                                    "Export".into()
                                })
                                .disabled(!export_ready || self.settings_loading || self.crop.open)
                                .on_click(cx.listener(|this, _, window, cx| {
                                    if this.export_job.is_some() {
                                        this.open_export_dialog(window, cx);
                                    } else {
                                        this.start_default_export(window, cx);
                                    }
                                })),
                        )
                        .child(div().h(px(20.)).w(px(1.)).bg(ink(ACCENT_STRONG)))
                        .child(
                            Button::new("export-customize")
                                .primary()
                                .rounded(px(0.))
                                .rounded_r(px(10.))
                                .icon(gpui_kit::assets::IconName::ChevronDown)
                                .accessibility_label("Customize export")
                                .disabled(!export_ready || self.settings_loading || self.crop.open)
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.open_export_dialog(window, cx)
                                })),
                        ),
                )
            })
            .into_any_element()
    }
}

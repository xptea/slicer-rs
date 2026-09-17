//! Editor screen composition and its empty-state file picker.

use super::*;

impl SlicerApp {
    pub(super) fn uses_composition_preview(&self) -> bool {
        let Some(session) = self.project_session.as_ref() else {
            return false;
        };
        let clips = session
            .project()
            .tracks
            .iter()
            .flat_map(|track| track.clips.iter());
        let mut count = 0_usize;
        let mut has_non_video = false;
        for clip in clips {
            count = count.saturating_add(1);
            has_non_video |= !matches!(clip.kind, project::ClipKind::Video(_));
        }
        count != 1 || has_non_video
    }

    pub(super) fn editor_view(&self, cx: &mut Context<Self>) -> AnyElement {
        v_flex()
            .size_full()
            .px(px(CONTENT_GUTTER))
            .pb_4()
            .gap_3()
            .child(
                div()
                    .flex_1()
                    .min_h(px(120.))
                    .child(if self.editor_path.is_some() {
                        self.preview_panel(cx)
                    } else {
                        self.drop_zone(cx)
                    }),
            )
            .child(self.transport(cx))
            .child(self.timeline(cx))
            .child(self.layer_status())
            .into_any_element()
    }

    /// Make model changes visible even while the native compatibility preview
    /// is showing the first video layer.  Imports and saves are asynchronous,
    /// so this is also the durable place for their result messages instead of
    /// relying on a transient header interaction.
    pub(super) fn layer_status(&self) -> AnyElement {
        let (layer_count, layer_details) = self
            .project_session
            .as_ref()
            .map(|session| {
                let project = session.project();
                let mut details = Vec::new();
                for track in &project.tracks {
                    for clip in &track.clips {
                        let (kind, fallback) = match &clip.kind {
                            project::ClipKind::Video(_) => ("Video", "video".to_owned()),
                            project::ClipKind::Image(_) => ("Image", "image".to_owned()),
                            project::ClipKind::Text(text) => ("Text", text.text.clone()),
                            project::ClipKind::Shape(_) => ("Shape", "shape".to_owned()),
                            project::ClipKind::Audio(_) => ("Audio", "audio".to_owned()),
                        };
                        let source = clip
                            .kind
                            .asset_id()
                            .and_then(|asset_id| project.asset(asset_id))
                            .and_then(|asset| asset.path.file_name())
                            .map(|name| name.to_string_lossy().into_owned())
                            .unwrap_or(fallback);
                        details.push(format!("{kind} · {source}"));
                    }
                }
                (details.len(), details.join("  ·  "))
            })
            .unwrap_or((0, String::new()));

        let detail_text = if layer_details.is_empty() {
            if self.import_pending > 0 {
                "Imported layers will appear here when inspection finishes".to_owned()
            } else {
                "No media layers yet".to_owned()
            }
        } else {
            layer_details
        };
        let status_color = if self.status.starts_with("Could not") {
            ink(BAD)
        } else if self.import_pending > 0 {
            ink(WARN)
        } else if self.status.starts_with("Project saved") {
            ink(GOOD)
        } else {
            ink(MUTED)
        };
        let activity = if self.import_pending > 0 {
            format!("Inspecting {} file(s)…", self.import_pending)
        } else if self.uses_composition_preview() {
            "Layered preview renders all layers · export uses the same project".to_owned()
        } else {
            "Model-backed project".to_owned()
        };

        v_flex()
            .w_full()
            .gap_1()
            .px_3()
            .py_2()
            .rounded(px(12.))
            .bg(ink(SURFACE))
            .border_1()
            .border_color(ink(BORDER))
            .child(
                h_flex()
                    .w_full()
                    .justify_between()
                    .gap_2()
                    .child(
                        div()
                            .font_semibold()
                            .child(format!("Layers ({layer_count})")),
                    )
                    .child(div().text_sm().text_color(ink(MUTED)).child(activity)),
            )
            .child(div().text_sm().text_color(ink(TEXT)).child(detail_text))
            .child(
                div()
                    .text_sm()
                    .text_color(status_color)
                    .child(self.status.clone()),
            )
            .into_any_element()
    }

    pub(super) fn drop_zone(&self, cx: &mut Context<Self>) -> AnyElement {
        let browse = Button::new("drop-browse")
            .primary()
            .label("Choose media")
            .on_click(cx.listener(|this, _, _, _| this.launch_dialog(DialogKind::Open)));
        v_flex()
            .size_full()
            .min_h(px(360.))
            .items_center()
            .justify_center()
            .gap_3()
            .rounded(px(18.))
            .border_1()
            .border_color(ink(ACCENT_STRONG))
            .bg(ink(SURFACE))
            .on_drop::<ExternalPaths>(cx.listener(|this, paths: &ExternalPaths, window, cx| {
                if let Some(path) = paths.paths().first().cloned() {
                    this.open_file(path, window, cx);
                }
            }))
            .child(div().text_color(ink(ACCENT)).text_size(px(42.)).child("＋"))
            .child(
                div()
                    .text_color(ink(TEXT))
                    .font_semibold()
                    .text_lg()
                    .child("Drop media here"),
            )
            .child(
                div()
                    .text_color(ink(MUTED))
                    .child("or browse for a video, image, or audio file"),
            )
            .child(browse)
            .into_any_element()
    }
}

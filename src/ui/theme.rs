//! Neutral palette and GPUI component theme.

pub(super) const CONTENT_GUTTER: f32 = 24.;
use gpui_kit::{
    component::{Theme, ThemeMode},
    gpui::*,
};

pub(super) const BG: u32 = 0x090909ff;
pub(super) const SURFACE: u32 = 0x181818ff;
pub(super) const SURFACE_RAISED: u32 = 0x232323ff;
pub(super) const SURFACE_HOVER: u32 = 0x2c2c2cff;
pub(super) const BORDER: u32 = 0x292929ff;
pub(super) const TEXT: u32 = 0xf2f2f2ff;
pub(super) const MUTED: u32 = 0xa0a0a0ff;
pub(super) const ACCENT: u32 = 0xddddddff;
pub(super) const ACCENT_STRONG: u32 = 0xbbbbbbff;
pub(super) const GOOD: u32 = 0x86efacff;
pub(super) const WARN: u32 = 0xfcd34dff;
pub(super) const BAD: u32 = 0xfca5a5ff;

pub(super) fn ink(value: u32) -> Hsla {
    rgba(value).into()
}

pub(super) fn apply(cx: &mut App) {
    Theme::change(ThemeMode::Dark, None, cx);
    let colors = &mut Theme::global_mut(cx).colors;
    colors.primary = ink(ACCENT);
    colors.primary_hover = ink(TEXT);
    colors.primary_active = ink(ACCENT_STRONG);
    colors.primary_foreground = ink(BG);
    // The client-side title bar uses the danger foreground for the close
    // control while its hover background stays red. Keep the glyph dark so it
    // remains readable against that background instead of becoming red-on-red.
    colors.danger_foreground = ink(BG);
    colors.accent = ink(SURFACE_HOVER);
    colors.accent_foreground = ink(TEXT);
    colors.ring = ink(MUTED);
    colors.selection = ink(BORDER);
    Theme::sync_base(cx);
}

//! Rasterize editable graphics only when their content or output size changes.
//! The same transparent RGBA texture is used by preview and offline export.
use super::project::{Graphic, TextStyle};
use cosmic_text::{
    Align, Attrs, Buffer, Color, Family, FontSystem, Metrics, Shaping, Style, SwashCache, Weight,
};
use image::{Pixel, Rgba, RgbaImage};

pub struct Graphics {
    fonts: FontSystem,
    cache: SwashCache,
}
impl Default for Graphics {
    fn default() -> Self {
        Self {
            fonts: FontSystem::new(),
            cache: SwashCache::new(),
        }
    }
}
impl Graphics {
    pub fn render(
        &mut self,
        graphic: &Graphic,
        width: u32,
        height: u32,
        project_height: u32,
    ) -> RgbaImage {
        match graphic {
            Graphic::Color { color } => RgbaImage::from_pixel(2, 2, Rgba(*color)),
            Graphic::Text(text) => self.text(text, width, height, project_height),
        }
    }
    fn text(
        &mut self,
        text: &TextStyle,
        width: u32,
        height: u32,
        project_height: u32,
    ) -> RgbaImage {
        let mut pixels = RgbaImage::from_pixel(width, height, Rgba(text.background));
        let font_size = text.size * project_height as f32 / 1080.;
        let mut buffer = Buffer::new(&mut self.fonts, Metrics::new(font_size, font_size * 1.25));
        buffer.set_size(Some(width as f32), Some(height as f32));
        let family = match text.font.as_str() {
            "Sans" => Family::SansSerif,
            "Serif" => Family::Serif,
            "Mono" => Family::Monospace,
            name => Family::Name(name),
        };
        let attrs = Attrs::new()
            .family(family)
            .weight(if text.bold {
                Weight::BOLD
            } else {
                Weight::NORMAL
            })
            .style(if text.italic {
                Style::Italic
            } else {
                Style::Normal
            });
        buffer.set_text(
            &text.text,
            &attrs,
            Shaping::Advanced,
            Some(match text.align {
                0 => Align::Left,
                2 => Align::Right,
                _ => Align::Center,
            }),
        );
        buffer.shape_until_scroll(&mut self.fonts, false);
        let used = buffer
            .layout_runs()
            .map(|run| run.line_y + run.line_height)
            .fold(0_f32, f32::max);
        let top = ((height as f32 - used) / 2.).max(0.) as i32;
        buffer.draw(
            &mut self.fonts,
            &mut self.cache,
            Color::rgba(text.color[0], text.color[1], text.color[2], text.color[3]),
            |x, y, w, h, color| {
                for dy in 0..h as i32 {
                    for dx in 0..w as i32 {
                        let (x, y) = (x + dx, y + dy + top);
                        if x >= 0 && y >= 0 && x < width as i32 && y < height as i32 {
                            pixels
                                .get_pixel_mut(x as u32, y as u32)
                                .blend(&Rgba(color.as_rgba()));
                        }
                    }
                }
            },
        );
        pixels
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn text_is_transparent_outside_glyphs_and_style_changes_pixels() {
        let mut renderer = Graphics::default();
        let mut style = TextStyle {
            text: "Hello\n世界".into(),
            ..TextStyle::default()
        };
        let first = renderer.render(&Graphic::Text(style.clone()), 640, 240, 1080);
        assert!(first.pixels().any(|p| p[3] > 200));
        assert!(first.pixels().filter(|p| p[3] == 0).count() > 640 * 100);
        style.bold = true;
        style.color = [255, 80, 0, 255];
        let styled = renderer.render(&Graphic::Text(style), 640, 240, 1080);
        assert_ne!(first, styled);
        assert!(
            styled
                .pixels()
                .any(|p| p[0] > 200 && p[1] < 120 && p[3] > 200)
        );
    }
}

//! Deterministic text layout used when a platform font rasterizer is absent.
//!
//! The project keeps the requested font family in its serialized style.  The
//! reference renderer deliberately uses a tiny embedded 5x7 fallback so that
//! headless preview/export tests have stable output on every host.  A GPU/UI
//! backend may replace the glyph rasterization while retaining these layout
//! coordinates.

use crate::project::{TextAlignment, TextStyle};

#[derive(Clone, Debug, PartialEq)]
pub struct TextGlyph {
    pub character: char,
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
    pub advance: f64,
    pub supported: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TextLine {
    pub text: String,
    pub y: f64,
    pub width: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TextLayout {
    pub glyphs: Vec<TextGlyph>,
    pub lines: Vec<TextLine>,
    pub width: f64,
    pub height: f64,
    pub line_height: f64,
    pub font_size: f64,
    pub unsupported_characters: usize,
}

impl TextLayout {
    pub fn new(text: &str, style: &TextStyle) -> Self {
        let size = style.font_size;
        let advance = size * 0.6;
        let line_height = size * style.line_height;
        let max_width = style
            .wrapping_width
            .filter(|value| value.is_finite() && *value > 0.0);
        let mut lines = Vec::<String>::new();

        for paragraph in text.split('\n') {
            if paragraph.is_empty() {
                lines.push(String::new());
                continue;
            }
            let mut current = String::new();
            let mut current_width = 0.0;
            for word in paragraph.split_whitespace() {
                let word_width = word.chars().count() as f64 * advance;
                let separator = if current.is_empty() { 0.0 } else { advance };
                if let Some(limit) = max_width
                    && !current.is_empty()
                    && current_width + separator + word_width > limit
                {
                    lines.push(std::mem::take(&mut current));
                    current_width = 0.0;
                }
                if !current.is_empty() {
                    current.push(' ');
                    current_width += advance;
                }
                if word_width <= max_width.unwrap_or(f64::INFINITY) {
                    current.push_str(word);
                    current_width += word_width;
                } else {
                    // Break an overlong word at character boundaries.
                    for character in word.chars() {
                        if let Some(limit) = max_width
                            && !current.is_empty()
                            && current_width + advance > limit
                        {
                            lines.push(std::mem::take(&mut current));
                            current_width = 0.0;
                        }
                        current.push(character);
                        current_width += advance;
                    }
                }
            }
            lines.push(current);
        }
        if lines.is_empty() {
            lines.push(String::new());
        }

        let measured_width = lines
            .iter()
            .map(|line| line.chars().count() as f64 * advance)
            .fold(0.0, f64::max);
        let width = max_width.unwrap_or(measured_width).max(advance);
        let mut glyphs = Vec::new();
        let mut output_lines = Vec::with_capacity(lines.len());
        let mut unsupported = 0;
        for (line_index, line) in lines.iter().enumerate() {
            let line_width = line.chars().count() as f64 * advance;
            let x = match style.alignment {
                TextAlignment::Center => (width - line_width).max(0.0) * 0.5,
                TextAlignment::Right => (width - line_width).max(0.0),
                TextAlignment::Left | TextAlignment::Justify => 0.0,
            };
            let y = line_index as f64 * line_height;
            output_lines.push(TextLine {
                text: line.clone(),
                y,
                width: line_width,
            });
            let mut cursor = x;
            for character in line.chars() {
                let supported = glyph_pattern(character).is_some();
                if !supported {
                    unsupported += 1;
                }
                glyphs.push(TextGlyph {
                    character,
                    x: cursor,
                    y,
                    width: advance,
                    height: size,
                    advance,
                    supported,
                });
                cursor += advance;
            }
        }

        Self {
            glyphs,
            lines: output_lines,
            width,
            height: (lines.len() as f64 * line_height).max(size),
            line_height,
            font_size: size,
            unsupported_characters: unsupported,
        }
    }

    /// Return the 5x7 coverage of a glyph at a normalized glyph-local point.
    pub fn coverage(&self, character: char, x: f64, y: f64) -> f64 {
        let Some(pattern) = glyph_pattern(character) else {
            return 0.0;
        };
        if !(0.0..1.0).contains(&x) || !(0.0..1.0).contains(&y) {
            return 0.0;
        }
        let column = (x * 5.0).floor() as usize;
        let row = (y * 7.0).floor() as usize;
        if pattern[row] & (1 << (4 - column)) != 0 {
            1.0
        } else {
            0.0
        }
    }
}

fn glyph_pattern(character: char) -> Option<[u8; 7]> {
    let character = if character.is_ascii_lowercase() {
        character.to_ascii_uppercase()
    } else {
        character
    };
    let pattern = match character {
        'A' => [0x0e, 0x11, 0x11, 0x1f, 0x11, 0x11, 0x11],
        'B' => [0x1e, 0x11, 0x11, 0x1e, 0x11, 0x11, 0x1e],
        'C' => [0x0f, 0x10, 0x10, 0x10, 0x10, 0x10, 0x0f],
        'D' => [0x1e, 0x11, 0x11, 0x11, 0x11, 0x11, 0x1e],
        'E' => [0x1f, 0x10, 0x10, 0x1e, 0x10, 0x10, 0x1f],
        'F' => [0x1f, 0x10, 0x10, 0x1e, 0x10, 0x10, 0x10],
        'G' => [0x0f, 0x10, 0x10, 0x17, 0x11, 0x11, 0x0f],
        'H' => [0x11, 0x11, 0x11, 0x1f, 0x11, 0x11, 0x11],
        'I' => [0x1f, 0x04, 0x04, 0x04, 0x04, 0x04, 0x1f],
        'J' => [0x01, 0x01, 0x01, 0x01, 0x11, 0x11, 0x0e],
        'K' => [0x11, 0x12, 0x14, 0x18, 0x14, 0x12, 0x11],
        'L' => [0x10, 0x10, 0x10, 0x10, 0x10, 0x10, 0x1f],
        'M' => [0x11, 0x1b, 0x15, 0x15, 0x11, 0x11, 0x11],
        'N' => [0x11, 0x19, 0x15, 0x13, 0x11, 0x11, 0x11],
        'O' => [0x0e, 0x11, 0x11, 0x11, 0x11, 0x11, 0x0e],
        'P' => [0x1e, 0x11, 0x11, 0x1e, 0x10, 0x10, 0x10],
        'Q' => [0x0e, 0x11, 0x11, 0x11, 0x15, 0x12, 0x0d],
        'R' => [0x1e, 0x11, 0x11, 0x1e, 0x14, 0x12, 0x11],
        'S' => [0x0f, 0x10, 0x10, 0x0e, 0x01, 0x01, 0x1e],
        'T' => [0x1f, 0x04, 0x04, 0x04, 0x04, 0x04, 0x04],
        'U' => [0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x0e],
        'V' => [0x11, 0x11, 0x11, 0x11, 0x11, 0x0a, 0x04],
        'W' => [0x11, 0x11, 0x11, 0x15, 0x15, 0x15, 0x0a],
        'X' => [0x11, 0x11, 0x0a, 0x04, 0x0a, 0x11, 0x11],
        'Y' => [0x11, 0x11, 0x0a, 0x04, 0x04, 0x04, 0x04],
        'Z' => [0x1f, 0x01, 0x02, 0x04, 0x08, 0x10, 0x1f],
        '0' => [0x0e, 0x11, 0x13, 0x15, 0x19, 0x11, 0x0e],
        '1' => [0x04, 0x0c, 0x04, 0x04, 0x04, 0x04, 0x0e],
        '2' => [0x0e, 0x11, 0x01, 0x02, 0x04, 0x08, 0x1f],
        '3' => [0x1e, 0x01, 0x01, 0x0e, 0x01, 0x01, 0x1e],
        '4' => [0x02, 0x06, 0x0a, 0x12, 0x1f, 0x02, 0x02],
        '5' => [0x1f, 0x10, 0x10, 0x1e, 0x01, 0x01, 0x1e],
        '6' => [0x0e, 0x10, 0x10, 0x1e, 0x11, 0x11, 0x0e],
        '7' => [0x1f, 0x01, 0x02, 0x04, 0x08, 0x08, 0x08],
        '8' => [0x0e, 0x11, 0x11, 0x0e, 0x11, 0x11, 0x0e],
        '9' => [0x0e, 0x11, 0x11, 0x0f, 0x01, 0x01, 0x0e],
        ' ' => [0; 7],
        '.' => [0; 6]
            .into_iter()
            .chain([0x04])
            .collect::<Vec<_>>()
            .try_into()
            .ok()?,
        ',' => [0; 5]
            .into_iter()
            .chain([0x04, 0x08])
            .collect::<Vec<_>>()
            .try_into()
            .ok()?,
        ':' => [0x00, 0x04, 0x04, 0x00, 0x04, 0x04, 0x00],
        '!' => [0x04, 0x04, 0x04, 0x04, 0x04, 0x00, 0x04],
        '?' => [0x0e, 0x11, 0x01, 0x02, 0x04, 0x00, 0x04],
        '-' => [0x00, 0x00, 0x00, 0x1f, 0x00, 0x00, 0x00],
        '_' => [0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x1f],
        _ => return None,
    };
    Some(pattern)
}

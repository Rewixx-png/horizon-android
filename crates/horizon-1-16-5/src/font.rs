//! The 1.16.5 default font: the `ascii.png` glyph sheet plus vanilla's
//! per-glyph advances from `font/default.json` and `glyph_sizes.bin`.
//!
//! Only the ASCII sheet is used (code points 0..=255). Multi-megapixel unicode
//! pages would multiply the GUI atlas and the HUD rarely needs them; a glyph
//! the sheet lacks draws as a space, never as a wrong symbol.

use crate::atlas::{TextureImage, decode_png};

/// The sheet vanilla tiles 16x16 glyph cells into, in pixels. 1.16.5's
/// `ascii.png` is 128x128 (16 cells of 8 pixels), not the 256 of later
/// versions; only the 8-pixel cell grid matters here.
pub const SHEET: u32 = 128;
/// One glyph cell is 8x8 pixels of the sheet.
pub const CELL: u32 = 8;
/// The default advance of a wide glyph, in GUI pixels.
pub const DEFAULT_ADVANCE: u8 = 8;

/// The ASCII font: two sprite images (the white sheet and its shadow offset)
/// and the advance table.
pub struct Font {
    /// The glyph sheet as a texture image, keyed per glyph for the atlas.
    pub sheet: Vec<(String, TextureImage)>,
    /// Advance per code point 0..=255, in GUI pixels.
    pub advances: [u8; 256],
}

/// The sprite name for glyph `codepoint` and `shadow` (the black one-pixel
/// offset copy vanilla draws behind the white glyph).
pub fn sprite_name(codepoint: u8, shadow: bool) -> String {
    if shadow {
        format!("font_shadow_{codepoint}")
    } else {
        format!("font_{codepoint}")
    }
}

/// Splits `ascii.png` into one 8x8 sprite per non-blank glyph.
pub fn build(ascii_png: &[u8]) -> Result<Font, String> {
    let sheet = decode_png(ascii_png)?;
    if sheet.width < SHEET || sheet.height < SHEET {
        return Err(format!(
            "ascii.png is {}x{}, expected at least {SHEET}x{SHEET}",
            sheet.width, sheet.height
        ));
    }
    let mut images = Vec::new();
    for codepoint in 0u16..256 {
        let (cell_x, cell_y) = ((codepoint % 16) as u32, (codepoint / 16) as u32);
        let x = cell_x * CELL;
        let y = cell_y * CELL;
        let mut image = TextureImage {
            width: CELL,
            height: CELL,
            rgba: vec![0u8; (CELL * CELL * 4) as usize],
        };
        let mut blank = true;
        for row in 0..CELL {
            for col in 0..CELL {
                let src = (((y + row) * sheet.width + x + col) * 4) as usize;
                let rgba = &sheet.rgba[src..src + 4];
                if rgba[3] != 0 {
                    blank = false;
                }
                let dst = ((row * CELL + col) * 4) as usize;
                image.rgba[dst..dst + 4].copy_from_slice(rgba);
            }
        }
        if blank {
            continue;
        }
        let name = sprite_name(codepoint as u8, false);
        images.push((name, image));
    }
    // Vanilla's advances: ASCII glyphs are mostly 6, with wider letters at 9.
    let mut advances = [DEFAULT_ADVANCE; 256];
    for (codepoint, advance) in advances.iter_mut().enumerate() {
        let narrow = matches!(
            codepoint as u8,
            b' ' | b'!' | b',' | b'.' | b':' | b';' | b'\'' | b'|' | b'i' | b'l' | b'I'
        );
        *advance = if narrow { 4 } else { 6 };
    }
    advances[b' ' as usize] = 4;
    Ok(Font {
        sheet: images,
        advances,
    })
}

/// The width of `text` in GUI pixels, in the default font.
pub fn text_width(font: &Font, text: &str) -> u32 {
    text.bytes()
        .map(|byte| u32::from(font.advances[byte as usize]))
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sprite_names_are_distinct_per_glyph_and_shadow() {
        assert_eq!(sprite_name(b'A', false), "font_65");
        assert_eq!(sprite_name(b'A', true), "font_shadow_65");
        assert_ne!(sprite_name(b'a', false), sprite_name(b'b', false));
    }

    #[test]
    fn text_width_sums_the_advances() {
        let mut advances = [6u8; 256];
        advances[b' ' as usize] = 4;
        let font = Font {
            sheet: Vec::new(),
            advances,
        };
        // "Grass Block" is eleven glyphs; the single space advances 4.
        assert_eq!(text_width(&font, "Grass Block"), 10 * 6 + 4);
        assert_eq!(text_width(&font, ""), 0);
    }

    #[test]
    fn a_short_sheet_is_an_error() {
        // Eight bytes are not a PNG at all; the decoder rejects it before
        // the size check can run.
        assert!(build(&[0u8; 8]).is_err());
    }
}

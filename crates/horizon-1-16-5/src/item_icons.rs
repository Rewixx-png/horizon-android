//! Small square RGBA icons for inventory items and blocks.
//!
//! The 1.16.5 HUD pipeline has no texture sampling, so a hotbar slot's icon is
//! drawn as a banded approximation of its sprite. This module turns the flat
//! `item/` or `block/` PNG behind a sprite name into a tiny square RGBA8 image
//! the HUD can read colour bands from, and caches the result per texture.
//!
//! Item sprites (`minecraft:item/<name>`) render exactly like block sprites
//! (`minecraft:block/<name>`): the sprite's first animation frame is cropped
//! (honouring its `.mcmeta` frame height) and nearest-resized into the square,
//! leaving one transparent pixel around the edge so a swatch drawn from the
//! icon reads as a silhouette rather than a filled tile.

use std::collections::HashMap;

use serde_json::Value;

use crate::atlas::{TextureImage, TextureSource, decode_png};

/// A per-texture cache of small square item icons.
#[derive(Debug, Default)]
pub struct ItemIcons {
    /// Rendered icons keyed by their pack-relative PNG path.
    cache: HashMap<String, Vec<u8>>,
}

impl ItemIcons {
    /// A cache with no rendered icons yet.
    pub fn new() -> Self {
        Self::default()
    }

    /// The icon last rendered for `texture`, RGBA8 and row after row.
    ///
    /// The image is `size * size * 4` bytes for whichever `size` the texture
    /// was rendered at. Namespaced (`minecraft:item/apple`) and bare
    /// (`item/apple`) spellings of the same texture share one cache entry.
    pub fn cached(&self, texture: &str) -> Option<&[u8]> {
        let path = texture_path(texture)?;
        self.cache.get(&path).map(Vec::as_slice)
    }

    /// Renders `texture` as a `size` by `size` RGBA8 icon.
    ///
    /// `texture` is a sprite name as item models use it, such as
    /// `minecraft:item/apple` or `minecraft:block/stone`; a bare path is read
    /// under the `minecraft` namespace. The sprite's first animation frame is
    /// cropped and nearest-resized into the icon, whose one-pixel border is
    /// fully transparent.
    ///
    /// Returns `None` when the name is empty, `size` is zero, or the source
    /// does not carry a decodable PNG for the name. Icons are cached by
    /// texture; asking for a different size re-renders and replaces the
    /// cached image so `cached` always matches the most recent request.
    pub fn icon(
        &mut self,
        source: &mut dyn TextureSource,
        texture: &str,
        size: u32,
    ) -> Option<Vec<u8>> {
        let path = texture_path(texture)?;
        if size == 0 {
            return None;
        }
        let expected = (size as usize).checked_mul(size as usize)?.checked_mul(4)?;
        // A cached image of another size is stale for this request and is
        // re-rendered, but a matching one is returned byte for byte.
        if let Some(bytes) = self.cache.get(&path) {
            if bytes.len() == expected {
                return Some(bytes.clone());
            }
        }
        let bytes = source.read(&path).ok()??;
        let image = decode_png(&bytes).ok()?;
        let image = first_frame(source, &path, image);
        if image.width == 0 || image.height == 0 {
            return None;
        }
        let icon = resize_with_border(&image, size);
        self.cache.insert(path, icon.clone());
        Some(icon)
    }
}

/// The pack-relative PNG path of a sprite name, e.g. `minecraft:textures/block/stone.png`.
///
/// `minecraft:block/stone`, `block/stone` and `minecraft:textures/block/stone.png`
/// all address the same file.
fn texture_path(texture: &str) -> Option<String> {
    let (namespace, path) = match texture.split_once(':') {
        Some((namespace, path)) => (namespace, path),
        None => ("minecraft", texture),
    };
    let path = path.strip_prefix("textures/").unwrap_or(path);
    let path = path.strip_suffix(".png").unwrap_or(path);
    if namespace.is_empty() || path.is_empty() {
        return None;
    }
    Some(format!("{namespace}/textures/{path}.png"))
}

/// Crops `image` to its first animation frame when a companion `.mcmeta`
/// declares a shorter frame height; otherwise the image is returned unchanged.
fn first_frame(source: &mut dyn TextureSource, path: &str, image: TextureImage) -> TextureImage {
    let frame_height = source
        .read(&format!("{path}.mcmeta"))
        .ok()
        .flatten()
        .and_then(|meta| first_frame_height(&meta, image.width, image.height));
    let Some(height) = frame_height else {
        return image;
    };
    let width = image.width;
    let rows = width as usize * 4 * height as usize;
    TextureImage {
        width,
        height,
        rgba: image.rgba[..rows].to_vec(),
    }
}

/// The frame height a sprite `.mcmeta` declares, when it is a whole number of
/// frames of a vertical strip.
fn first_frame_height(meta: &[u8], width: u32, height: u32) -> Option<u32> {
    let json: Value = serde_json::from_slice(meta).ok()?;
    let animation = json.get("animation")?;
    let frame = animation
        .get("frameHeight")
        .and_then(Value::as_u64)
        .unwrap_or(u64::from(width)) as u32;
    (frame > 0 && frame <= height && height % frame == 0 && frame != height).then_some(frame)
}

/// Nearest-resizes `image` into a `size` square, leaving a transparent border.
fn resize_with_border(image: &TextureImage, size: u32) -> Vec<u8> {
    let size = size as usize;
    let mut icon = vec![0u8; size * size * 4];
    if size < 3 {
        return icon;
    }
    let inner = size - 2;
    let width = image.width as usize;
    let height = image.height as usize;
    for y in 0..inner {
        let source_y = (y * height / inner).min(height - 1);
        for x in 0..inner {
            let source_x = (x * width / inner).min(width - 1);
            let source = (source_y * width + source_x) * 4;
            let target = ((y + 1) * size + (x + 1)) * 4;
            icon[target..target + 4].copy_from_slice(&image.rgba[source..source + 4]);
        }
    }
    icon
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An in-memory pack: asset path -> file bytes.
    #[derive(Default)]
    struct FakeSource {
        files: HashMap<String, Vec<u8>>,
    }

    impl FakeSource {
        fn new() -> Self {
            Self::default()
        }

        fn insert_png(&mut self, path: &str, width: u32, height: u32, rgba: &[u8]) {
            self.files
                .insert(path.to_owned(), encode_png(width, height, rgba));
        }

        fn insert_text(&mut self, path: &str, text: &str) {
            self.files.insert(path.to_owned(), text.as_bytes().to_vec());
        }
    }

    impl TextureSource for FakeSource {
        fn read(&mut self, path: &str) -> Result<Option<Vec<u8>>, String> {
            Ok(self.files.get(path).cloned())
        }
    }

    fn encode_png(width: u32, height: u32, rgba: &[u8]) -> Vec<u8> {
        let mut cursor = std::io::Cursor::new(Vec::new());
        {
            let mut encoder = png::Encoder::new(&mut cursor, width, height);
            encoder.set_color(png::ColorType::Rgba);
            encoder.set_depth(png::BitDepth::Eight);
            let mut writer = encoder.write_header().unwrap();
            writer.write_image_data(rgba).unwrap();
            writer.finish().unwrap();
        }
        cursor.into_inner()
    }

    /// A 4x4 sprite whose pixel `(x, y)` is `[x * 60, y * 60, 0, 255]`.
    fn gradient_4x4() -> Vec<u8> {
        let mut rgba = Vec::with_capacity(4 * 4 * 4);
        for y in 0..4u8 {
            for x in 0..4u8 {
                rgba.extend_from_slice(&[x * 60, y * 60, 0, 255]);
            }
        }
        rgba
    }

    fn pixel(icon: &[u8], size: u32, x: u32, y: u32) -> [u8; 4] {
        let index = ((y * size + x) * 4) as usize;
        icon[index..index + 4].try_into().unwrap()
    }

    #[test]
    fn icons_resize_deterministically_with_a_transparent_border() {
        let mut source = FakeSource::new();
        source.insert_png(
            "minecraft/textures/block/gradient.png",
            4,
            4,
            &gradient_4x4(),
        );

        let mut first_cache = ItemIcons::new();
        let first = first_cache
            .icon(&mut source, "minecraft:block/gradient", 4)
            .expect("sprite renders");
        let again = first_cache
            .icon(&mut source, "minecraft:block/gradient", 4)
            .expect("sprite renders");
        assert_eq!(first, again);

        // Nearest sampling at 4x4 keeps the (0,0), (2,0), (0,2), (2,2) texels.
        assert_eq!(pixel(&first, 4, 1, 1), [0, 0, 0, 255]);
        assert_eq!(pixel(&first, 4, 2, 1), [120, 0, 0, 255]);
        assert_eq!(pixel(&first, 4, 1, 2), [0, 120, 0, 255]);
        assert_eq!(pixel(&first, 4, 2, 2), [120, 120, 0, 255]);

        // Every border texel stays fully transparent, so nothing opaque
        // frames the icon.
        for x in 0..4 {
            for y in 0..4 {
                if x == 0 || y == 0 || x == 3 || y == 3 {
                    assert_eq!(pixel(&first, 4, x, y), [0, 0, 0, 0], "border {x},{y}");
                }
            }
        }

        let mut second_cache = ItemIcons::new();
        let second = second_cache
            .icon(&mut source, "minecraft:block/gradient", 4)
            .expect("sprite renders");
        assert_eq!(first, second);
    }

    #[test]
    fn cache_returns_the_same_bytes() {
        let mut source = FakeSource::new();
        source.insert_png(
            "minecraft/textures/item/apple.png",
            4,
            4,
            &[255u8, 0, 0, 255].repeat(16),
        );
        let mut icons = ItemIcons::new();
        assert_eq!(icons.cached("minecraft:item/apple"), None);
        let first = icons
            .icon(&mut source, "minecraft:item/apple", 8)
            .expect("sprite renders");
        assert_eq!(first.len(), 8 * 8 * 4);
        assert_eq!(icons.cached("minecraft:item/apple"), Some(first.as_slice()));
        // Bare names address the same sprite and share the cache entry.
        assert_eq!(icons.cached("item/apple"), Some(first.as_slice()));

        let second = icons
            .icon(&mut source, "item/apple", 8)
            .expect("sprite renders");
        assert_eq!(first, second);

        // A different size re-renders instead of returning a stale icon.
        let larger = icons
            .icon(&mut source, "minecraft:item/apple", 16)
            .expect("sprite renders");
        assert_eq!(larger.len(), 16 * 16 * 4);
        assert_eq!(
            icons.cached("minecraft:item/apple"),
            Some(larger.as_slice())
        );
    }

    #[test]
    fn unknown_textures_return_none() {
        let mut source = FakeSource::new();
        let mut icons = ItemIcons::new();
        assert_eq!(icons.icon(&mut source, "minecraft:item/missing", 8), None);
        assert_eq!(icons.cached("minecraft:item/missing"), None);
        assert_eq!(icons.icon(&mut source, "", 8), None);
        assert_eq!(icons.cached(""), None);
        assert_eq!(icons.icon(&mut source, "minecraft:block/stone", 0), None);
        assert_eq!(icons.cached("minecraft:block/stone"), None);
    }

    #[test]
    fn animated_sprites_use_their_first_frame() {
        let mut source = FakeSource::new();
        // A 2x4 sheet of two 2x2 frames: green on top, magenta below.
        let mut rgba = Vec::new();
        rgba.extend_from_slice(&[0u8, 255, 0, 255].repeat(4));
        rgba.extend_from_slice(&[255u8, 0, 255, 255].repeat(4));
        source.insert_png("minecraft/textures/item/animated.png", 2, 4, &rgba);
        source.insert_text(
            "minecraft/textures/item/animated.png.mcmeta",
            r#"{"animation": {"frameHeight": 2}}"#,
        );

        let mut icons = ItemIcons::new();
        let icon = icons
            .icon(&mut source, "minecraft:item/animated", 4)
            .expect("sprite renders");
        for x in 1..3 {
            for y in 1..3 {
                assert_eq!(pixel(&icon, 4, x, y), [0, 255, 0, 255], "pixel {x},{y}");
            }
        }
    }
}

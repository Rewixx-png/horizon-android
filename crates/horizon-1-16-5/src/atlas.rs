//! Format-6 texture sources and sprite atlases for the 1.16.5 target.
//!
//! A 1.16.5 resource pack addresses a texture by a logical asset name such as
//! `minecraft/textures/block/stone.png`. The vanilla 1.16 asset index only
//! contains sounds, fonts and a few GUI images; block and item textures ship
//! inside `client.jar`. [`VanillaAssets`] therefore reads the asset objects
//! first and falls back to the jar, matching the two sources the prepare tool
//! keeps under `.local/versions/1.16.5/inputs`.
//!
//! The atlas is a nearest-neighbour sprite sheet with a one-pixel extruded
//! border around every sprite. Model UVs are 0..16, so a renderer maps them
//! into the sprite rectangle returned by [`TextureAtlas::uv`]. Animated
//! textures use their first frame; the frame height comes from the texture's
//! `.png.mcmeta`, and a malformed or unreadable texture falls back to the
//! generated magenta/black `missingno` checkerboard like vanilla.

use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};

/// The vanilla missing-texture name used by unresolved model variables.
pub const MISSING_TEXTURE: &str = "minecraft:missingno";

/// One decoded RGBA8 texture image, already cropped to its first animation
/// frame.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TextureImage {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// A byte source addressed by a pack-relative asset path such as
/// `minecraft/textures/block/stone.png`.
pub trait TextureSource {
    fn read(&mut self, path: &str) -> Result<Option<Vec<u8>>, String>;
}

/// A plain extracted resource pack: `root` is the folder holding the pack's
/// `assets/` directory (so `root/minecraft/textures/...` is a texture).
pub struct DirectorySource {
    root: PathBuf,
}

impl DirectorySource {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }
}

impl TextureSource for DirectorySource {
    fn read(&mut self, path: &str) -> Result<Option<Vec<u8>>, String> {
        let full = self.root.join(path);
        match std::fs::read(&full) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(format!("{}: {error}", full.display())),
        }
    }
}

/// The prepared `assets/objects/<xx>/<sha1>` store plus its index. Logical
/// names such as `minecraft/textures/gui/title/background/panorama_0.png`
/// map to a content-addressed object.
pub struct AssetObjects {
    root: PathBuf,
    objects: HashMap<String, String>,
}

impl AssetObjects {
    /// Reads an asset index and points it at an objects folder.
    pub fn open(index: &Path, objects_root: &Path) -> Result<Self, String> {
        let bytes = std::fs::read(index).map_err(|e| format!("{}: {e}", index.display()))?;
        let json: Value =
            serde_json::from_slice(&bytes).map_err(|e| format!("{}: {e}", index.display()))?;
        let objects = json
            .get("objects")
            .and_then(Value::as_object)
            .ok_or_else(|| format!("{}: no objects map", index.display()))?;
        let mut hashes = HashMap::new();
        for (name, entry) in objects {
            let hash = entry
                .get("hash")
                .and_then(Value::as_str)
                .ok_or_else(|| format!("{}: {name} has no hash", index.display()))?;
            if hash.len() != 40 || !hash.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                return Err(format!("{}: {name} has an invalid hash", index.display()));
            }
            hashes.insert(name.clone(), hash.to_owned());
        }
        Ok(Self {
            root: objects_root.to_path_buf(),
            objects: hashes,
        })
    }

    pub fn contains(&self, path: &str) -> bool {
        self.objects.contains_key(path)
    }
}

impl TextureSource for AssetObjects {
    fn read(&mut self, path: &str) -> Result<Option<Vec<u8>>, String> {
        let Some(hash) = self.objects.get(path) else {
            return Ok(None);
        };
        let full = self.root.join(&hash[..2]).join(hash);
        match std::fs::read(&full) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(format!("{}: {error}", full.display())),
        }
    }
}

/// The vanilla client jar, read through ZIP. Asset index names are prefixed
/// with `assets/` here, e.g. `assets/minecraft/textures/block/stone.png`.
pub struct ClientJar {
    path: PathBuf,
    archive: zip::ZipArchive<std::fs::File>,
}

impl ClientJar {
    pub fn open(path: &Path) -> Result<Self, String> {
        let file = std::fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let archive = zip::ZipArchive::new(file)
            .map_err(|e| format!("{}: not a client jar: {e}", path.display()))?;
        Ok(Self {
            path: path.to_path_buf(),
            archive,
        })
    }
}

impl TextureSource for ClientJar {
    fn read(&mut self, path: &str) -> Result<Option<Vec<u8>>, String> {
        use std::io::Read;
        let name = format!("assets/{path}");
        let mut entry = match self.archive.by_name(&name) {
            Ok(entry) => entry,
            Err(zip::result::ZipError::FileNotFound) => return Ok(None),
            Err(error) => return Err(format!("{}: {name}: {error}", self.path.display())),
        };
        let mut bytes = Vec::new();
        entry
            .read_to_end(&mut bytes)
            .map_err(|e| format!("{}: {name}: {e}", self.path.display()))?;
        Ok(Some(bytes))
    }
}

/// The two vanilla sources together: asset-index objects first, then the
/// client jar. Opening the jar is required; the object store is optional so a
/// bare client jar still works for block textures.
pub struct VanillaAssets {
    jar: ClientJar,
    objects: Option<AssetObjects>,
}

impl VanillaAssets {
    pub fn open(
        client_jar: &Path,
        index: Option<&Path>,
        objects_root: Option<&Path>,
    ) -> Result<Self, String> {
        let objects = match (index, objects_root) {
            (Some(index), Some(root)) => Some(AssetObjects::open(index, root)?),
            _ => None,
        };
        Ok(Self {
            jar: ClientJar::open(client_jar)?,
            objects,
        })
    }

    pub fn from_jar(client_jar: &Path) -> Result<Self, String> {
        Self::open(client_jar, None, None)
    }
}

impl TextureSource for VanillaAssets {
    fn read(&mut self, path: &str) -> Result<Option<Vec<u8>>, String> {
        if let Some(objects) = &mut self.objects {
            if let Some(bytes) = objects.read(path)? {
                return Ok(Some(bytes));
            }
        }
        self.jar.read(path)
    }
}

/// One sprite's rectangle in atlas pixels. `uv` converts it to the 0..1
/// coordinates a GPU samples.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Sprite {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

impl Sprite {
    /// The sprite rectangle as `[u0, v0, u1, v1]`, top-left to bottom-right.
    pub fn uv(&self, atlas: &TextureAtlas) -> [f32; 4] {
        [
            self.x as f32 / atlas.width as f32,
            self.y as f32 / atlas.height as f32,
            (self.x + self.width) as f32 / atlas.width as f32,
            (self.y + self.height) as f32 / atlas.height as f32,
        ]
    }
}

/// A packed RGBA8 sprite atlas.
pub struct TextureAtlas {
    width: u32,
    height: u32,
    pixels: Vec<u8>,
    sprites: BTreeMap<String, Sprite>,
}

impl TextureAtlas {
    /// Builds an atlas for `textures` from `source`. Every model texture name
    /// (`minecraft:block/stone`, `minecraft:missingno`, ...) is resolved to a
    /// sprite; an unresolved name shares the generated missing sprite.
    ///
    /// The packing is deterministic: the missing sprite comes first, then the
    /// requested names in sorted order, shelf-packed with a two-pixel gap.
    pub fn load(
        source: &mut dyn TextureSource,
        textures: impl IntoIterator<Item = String>,
    ) -> Result<Self, String> {
        let mut requested = BTreeSet::new();
        for texture in textures {
            requested.insert(normalized(texture));
        }
        requested.insert(MISSING_TEXTURE.to_owned());

        let missing = TextureImage {
            width: 16,
            height: 16,
            rgba: missing_checkerboard(),
        };
        let mut images = vec![(MISSING_TEXTURE.to_owned(), missing)];
        let mut resolved: HashMap<String, usize> = HashMap::new();
        resolved.insert(MISSING_TEXTURE.to_owned(), 0);

        for name in &requested {
            if resolved.contains_key(name) {
                continue;
            }
            let image = match texture_path(name) {
                Some(path) => match source.read(&path)? {
                    Some(bytes) => decode_first_frame(source, &path, &bytes).ok(),
                    None => None,
                },
                None => None,
            };
            match image {
                Some(image) => {
                    let index = images.len();
                    images.push((name.clone(), image));
                    resolved.insert(name.clone(), index);
                }
                None => {
                    resolved.insert(name.clone(), 0);
                }
            }
        }

        let (width, height, placed) = pack(&images);
        let mut pixels = vec![0u8; (width as usize) * (height as usize) * 4];
        for ((_, image), &(x, y)) in images.iter().zip(&placed) {
            blit(&mut pixels, width, image, x, y);
        }
        let mut sprites = BTreeMap::new();
        for (name, index) in &resolved {
            let (x, y) = placed[*index];
            let image = &images[*index].1;
            sprites.insert(
                name.clone(),
                Sprite {
                    x,
                    y,
                    width: image.width,
                    height: image.height,
                },
            );
        }
        Ok(Self {
            width,
            height,
            pixels,
            sprites,
        })
    }

    /// Builds an atlas from an extracted pack whose `assets/` folder is
    /// `root`.
    pub fn from_directory(
        root: &Path,
        textures: impl IntoIterator<Item = String>,
    ) -> Result<Self, String> {
        let mut source = DirectorySource::new(root);
        Self::load(&mut source, textures)
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    /// The atlas pixels, RGBA8, row after row.
    pub fn rgba(&self) -> &[u8] {
        &self.pixels
    }

    pub fn sprite(&self, texture: &str) -> Option<&Sprite> {
        self.sprites.get(&normalized(texture.to_owned()))
    }

    /// The sprite in normalized UV coordinates, or `None` for a name that
    /// was not requested at build time.
    pub fn uv(&self, texture: &str) -> Option<[f32; 4]> {
        self.sprite(texture).map(|sprite| sprite.uv(self))
    }

    /// Nearest-neighbour CPU sample of `texture` at sprite-local (u, v) in
    /// 0..1. Used by tests and tools that inspect the atlas without a GPU.
    pub fn sample(&self, texture: &str, u: f32, v: f32) -> Option<[u8; 4]> {
        let sprite = self.sprite(texture)?;
        let x = sprite.x + (u.clamp(0.0, 0.999_999) * sprite.width as f32).floor() as u32;
        let y = sprite.y + (v.clamp(0.0, 0.999_999) * sprite.height as f32).floor() as u32;
        let offset = ((y as usize) * (self.width as usize) + x as usize) * 4;
        self.pixels
            .get(offset..offset + 4)
            .map(|pixel| [pixel[0], pixel[1], pixel[2], pixel[3]])
    }
}

/// `minecraft:block/stone` and `block/stone` both become
/// `minecraft:block/stone`. A trailing `.png`, an explicit `textures/`
/// prefix and surrounding whitespace are tolerated.
fn normalized(name: String) -> String {
    let name = name.trim();
    let name = if name.contains(':') {
        name.to_owned()
    } else {
        format!("minecraft:{name}")
    };
    let Some((namespace, path)) = name.split_once(':') else {
        return name;
    };
    let path = path.strip_prefix("textures/").unwrap_or(path);
    let path = path.strip_suffix(".png").unwrap_or(path);
    format!("{namespace}:{path}")
}

/// The asset-index path of a model texture name, e.g.
/// `minecraft/textures/block/stone.png`.
fn texture_path(name: &str) -> Option<String> {
    let (namespace, path) = name.split_once(':')?;
    if namespace.is_empty() || path.is_empty() {
        return None;
    }
    Some(format!("{namespace}/textures/{path}.png"))
}

/// Decompresses a PNG and keeps the first animation frame when the companion
/// `.png.mcmeta` says the image is a vertical frame strip.
fn decode_first_frame(
    source: &mut dyn TextureSource,
    path: &str,
    bytes: &[u8],
) -> Result<TextureImage, String> {
    let image = decode_png(bytes)?;
    let frame_height = source
        .read(&format!("{path}.mcmeta"))?
        .and_then(|meta| first_frame_height(&meta, image.width, image.height))
        .unwrap_or(image.height);
    let height = frame_height.min(image.height);
    if height == image.height {
        return Ok(image);
    }
    let row = image.width as usize * 4;
    Ok(TextureImage {
        width: image.width,
        height,
        rgba: image.rgba[..row * height as usize].to_vec(),
    })
}

fn first_frame_height(meta: &[u8], width: u32, height: u32) -> Option<u32> {
    let json: Value = serde_json::from_slice(meta).ok()?;
    let animation = json.get("animation")?;
    let frame = animation
        .get("frameHeight")
        .and_then(Value::as_u64)
        .unwrap_or(u64::from(width)) as u32;
    (frame > 0 && frame <= height && height % frame == 0).then_some(frame)
}

/// Decodes a PNG to RGBA8 with palette and 16-bit data expanded to 8 bits.
pub fn decode_png(bytes: &[u8]) -> Result<TextureImage, String> {
    let mut decoder = png::Decoder::new(std::io::Cursor::new(bytes));
    decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut reader = decoder.read_info().map_err(|e| e.to_string())?;
    let mut buffer = vec![
        0;
        reader
            .output_buffer_size()
            .ok_or("texture exceeds decoder limits")?
    ];
    let info = reader.next_frame(&mut buffer).map_err(|e| e.to_string())?;
    let channels = info.color_type.samples();
    let mut rgba = Vec::with_capacity((info.width * info.height * 4) as usize);
    for pixel in buffer[..info.buffer_size()].chunks_exact(channels) {
        match info.color_type {
            png::ColorType::Rgba => rgba.extend_from_slice(&pixel[..4]),
            png::ColorType::Rgb => {
                rgba.extend_from_slice(&pixel[..3]);
                rgba.push(255);
            }
            png::ColorType::Grayscale => {
                rgba.extend_from_slice(&[pixel[0], pixel[0], pixel[0], 255]);
            }
            png::ColorType::GrayscaleAlpha => {
                rgba.extend_from_slice(&[pixel[0], pixel[0], pixel[0], pixel[1]]);
            }
            _ => return Err("unsupported expanded PNG format".into()),
        }
    }
    Ok(TextureImage {
        width: info.width,
        height: info.height,
        rgba,
    })
}

/// The magenta/black checkerboard vanilla generates for a missing sprite.
fn missing_checkerboard() -> Vec<u8> {
    let mut rgba = Vec::with_capacity(16 * 16 * 4);
    for y in 0..16 {
        for x in 0..16 {
            if (x / 8 + y / 8) % 2 == 0 {
                rgba.extend_from_slice(&[255, 0, 255, 255]);
            } else {
                rgba.extend_from_slice(&[0, 0, 0, 255]);
            }
        }
    }
    rgba
}

/// Shelf-packs the images, returning the atlas size and each image's
/// top-left position.
fn pack(images: &[(String, TextureImage)]) -> (u32, u32, Vec<(u32, u32)>) {
    const GAP: u32 = 2;
    let max_width = images
        .iter()
        .map(|(_, image)| image.width)
        .max()
        .unwrap_or(1)
        + GAP;
    let area: u64 = images
        .iter()
        .map(|(_, image)| u64::from(image.width + GAP) * u64::from(image.height + GAP))
        .sum();
    let mut width = (area as f64).sqrt().ceil() as u32;
    width = width.max(max_width).max(64).next_power_of_two();
    let mut placed = Vec::with_capacity(images.len());
    let mut x = GAP;
    let mut y = GAP;
    let mut row_height = 0;
    for (_, image) in images {
        if x + image.width + GAP > width {
            x = GAP;
            y += row_height + GAP;
            row_height = 0;
        }
        placed.push((x, y));
        x += image.width + GAP;
        row_height = row_height.max(image.height);
    }
    let used_height = y + row_height + GAP;
    let height = used_height.max(1).next_power_of_two();
    (width, height, placed)
}

/// Copies an image into the atlas and extrudes its edge pixels one texel out,
/// so sampling at a sprite boundary cannot pick a transparent atlas texel.
fn blit(atlas: &mut [u8], atlas_width: u32, image: &TextureImage, x: u32, y: u32) {
    for py in -1..=image.height as i32 {
        for px in -1..=image.width as i32 {
            let sx = px.clamp(0, image.width as i32 - 1) as u32;
            let sy = py.clamp(0, image.height as i32 - 1) as u32;
            let dx = x as i32 + px;
            let dy = y as i32 + py;
            if dx < 0 || dy < 0 || dx >= atlas_width as i32 {
                continue;
            }
            let source = ((sy * image.width + sx) * 4) as usize;
            let target = ((dy as u32 * atlas_width + dx as u32) * 4) as usize;
            if let (Some(from), Some(to)) = (
                image.rgba.get(source..source + 4),
                atlas.get_mut(target..target + 4),
            ) {
                to.copy_from_slice(from);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static NEXT_DIR: AtomicUsize = AtomicUsize::new(0);

    fn temp_root() -> PathBuf {
        let id = NEXT_DIR.fetch_add(1, Ordering::Relaxed);
        let root =
            std::env::temp_dir().join(format!("horizon-1165-atlas-{}-{id}", std::process::id()));
        std::fs::create_dir_all(root.join("minecraft/textures/block")).unwrap();
        root
    }

    fn write_png(path: &Path, width: u32, height: u32, rgba: &[u8]) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let file = std::fs::File::create(path).unwrap();
        let mut encoder = png::Encoder::new(file, width, height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().unwrap();
        writer.write_image_data(rgba).unwrap();
        writer.finish().unwrap();
    }

    fn solid(width: u32, height: u32, colour: [u8; 4]) -> Vec<u8> {
        colour.repeat((width * height) as usize)
    }

    #[test]
    fn distinct_textures_get_distinct_sprites_and_uvs() {
        let root = temp_root();
        write_png(
            &root.join("minecraft/textures/block/red.png"),
            2,
            2,
            &solid(2, 2, [255, 0, 0, 255]),
        );
        write_png(
            &root.join("minecraft/textures/block/blue.png"),
            4,
            4,
            &solid(4, 4, [0, 0, 255, 255]),
        );
        let atlas = TextureAtlas::from_directory(
            &root,
            ["minecraft:block/red".to_owned(), "block/blue".to_owned()],
        )
        .unwrap();
        let red = *atlas.sprite("minecraft:block/red").unwrap();
        let blue = *atlas.sprite("minecraft:block/blue").unwrap();
        assert_eq!((red.width, red.height), (2, 2));
        assert_eq!((blue.width, blue.height), (4, 4));
        assert_eq!(
            atlas.sample("minecraft:block/red", 0.5, 0.5),
            Some([255, 0, 0, 255])
        );
        assert_eq!(
            atlas.sample("minecraft:block/blue", 0.5, 0.5),
            Some([0, 0, 255, 255])
        );
        let red_uv = atlas.uv("minecraft:block/red").unwrap();
        let blue_uv = atlas.uv("block/blue").unwrap();
        assert!(
            red_uv[2] <= blue_uv[0]
                || blue_uv[2] <= red_uv[0]
                || red_uv[3] <= blue_uv[1]
                || blue_uv[3] <= red_uv[1]
        );
        assert!(red_uv[0] >= 0.0 && red_uv[2] <= 1.0);
        assert!(atlas.sample("minecraft:block/missing", 0.0, 0.0).is_none());
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn unresolved_and_unknown_textures_share_the_missing_sprite() {
        let root = temp_root();
        let atlas =
            TextureAtlas::from_directory(&root, ["minecraft:block/does_not_exist".to_owned()])
                .unwrap();
        let missing = atlas.uv(MISSING_TEXTURE).unwrap();
        assert_eq!(atlas.uv("minecraft:block/does_not_exist"), Some(missing));
        // The generated checkerboard starts magenta in its top-left texel.
        assert_eq!(
            atlas.sample(MISSING_TEXTURE, 0.0, 0.0),
            Some([255, 0, 255, 255])
        );
        assert_eq!(
            atlas.sample(MISSING_TEXTURE, 0.9, 0.1),
            Some([0, 0, 0, 255])
        );
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn animated_textures_use_the_first_frame() {
        let root = temp_root();
        let path = root.join("minecraft/textures/block/water.png");
        let mut strip = Vec::new();
        strip.extend(solid(4, 4, [0, 0, 255, 255]));
        strip.extend(solid(4, 4, [255, 0, 0, 255]));
        write_png(&path, 4, 8, &strip);
        std::fs::write(
            root.join("minecraft/textures/block/water.png.mcmeta"),
            r#"{"animation": {"frametime": 2}}"#,
        )
        .unwrap();
        let atlas =
            TextureAtlas::from_directory(&root, ["minecraft:block/water".to_owned()]).unwrap();
        let sprite = *atlas.sprite("minecraft:block/water").unwrap();
        assert_eq!((sprite.width, sprite.height), (4, 4));
        assert_eq!(
            atlas.sample("minecraft:block/water", 0.5, 0.5),
            Some([0, 0, 255, 255])
        );
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn asset_objects_are_read_through_their_index() {
        let root = temp_root();
        let hash = "ab".repeat(20);
        let objects = root.join("objects");
        std::fs::create_dir_all(objects.join("ab")).unwrap();
        let png_path = objects.join("ab").join(&hash);
        write_png(&png_path, 1, 1, &[7, 8, 9, 255]);
        let index = root.join("index.json");
        std::fs::write(
            &index,
            format!(
                r#"{{"objects": {{"minecraft/textures/gui/test.png": {{"hash": "{hash}", "size": 1}}}}}}"#
            ),
        )
        .unwrap();
        let mut assets = AssetObjects::open(&index, &objects).unwrap();
        let bytes = assets
            .read("minecraft/textures/gui/test.png")
            .unwrap()
            .expect("object exists");
        let image = decode_png(&bytes).unwrap();
        assert_eq!((image.width, image.height), (1, 1));
        assert!(
            assets
                .read("minecraft/textures/block/stone.png")
                .unwrap()
                .is_none()
        );
        std::fs::remove_dir_all(&root).unwrap();
    }
}

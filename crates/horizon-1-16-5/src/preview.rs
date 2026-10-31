//! A tiny software preview of a CPU chunk mesh. This is not the game
//! renderer; it exists so the world/model/mesh pipeline can produce a visible,
//! deterministic image on a build machine before the wgpu/Android integration.

use crate::Mesh;
use std::f32::consts::PI;

pub struct Preview {
    pub width: usize,
    pub height: usize,
    pub rgb: Vec<u8>,
}

/// Orthographic isometric projection. `yaw`/`pitch` are radians, `scale` is
/// pixels per block. The returned buffer is RGB8, top row first.
pub fn render(
    mesh: &Mesh,
    width: usize,
    height: usize,
    yaw: f32,
    pitch: f32,
    scale: f32,
) -> Preview {
    let width = width.max(1);
    let height = height.max(1);
    let mut rgb = vec![0u8; width * height * 3];
    // A quiet blue-grey sky; the geometry overdraws it.
    for pixel in rgb.chunks_exact_mut(3) {
        pixel.copy_from_slice(&[24, 28, 36]);
    }
    let mut depth = vec![f32::INFINITY; width * height];
    let center = bounds_center(mesh);
    let (sy, cy) = (yaw.sin(), yaw.cos());
    let (sp, cp) = (pitch.sin(), pitch.cos());
    let project = |p: [f32; 3]| {
        let p = [p[0] - center[0], p[1] - center[1], p[2] - center[2]];
        let x = p[0] * cy - p[2] * sy;
        let z = p[0] * sy + p[2] * cy;
        let y = p[1] * cp - z * sp;
        let z = p[1] * sp + z * cp;
        [
            width as f32 / 2.0 + x * scale,
            height as f32 / 2.0 - y * scale,
            z,
        ]
    };
    for (triangle_index, triangle) in mesh.indices.chunks_exact(3).enumerate() {
        let Some(vertices) = triangle
            .iter()
            .map(|index| mesh.vertices.get(*index as usize))
            .collect::<Option<Vec<_>>>()
        else {
            continue;
        };
        let points: Vec<[f32; 3]> = vertices.iter().map(|v| project(v.position)).collect();
        let normal = average_normal(&vertices);
        let light = (normal[1] * 0.65 + normal[0] * 0.25 + normal[2] * 0.35).abs();
        let base = texture_color(
            mesh.textures
                .get(triangle_index / 2)
                .map(String::as_str)
                .unwrap_or(""),
        );
        let color = base.map(|channel| (channel as f32 * (0.55 + 0.55 * light)).min(255.0) as u8);
        rasterize(&points, color, &mut rgb, &mut depth, width, height);
    }
    Preview { width, height, rgb }
}

fn bounds_center(mesh: &Mesh) -> [f32; 3] {
    if mesh.vertices.is_empty() {
        return [0.0; 3];
    }
    let mut min = [f32::INFINITY; 3];
    let mut max = [f32::NEG_INFINITY; 3];
    for vertex in &mesh.vertices {
        for axis in 0..3 {
            min[axis] = min[axis].min(vertex.position[axis]);
            max[axis] = max[axis].max(vertex.position[axis]);
        }
    }
    [
        (min[0] + max[0]) / 2.0,
        (min[1] + max[1]) / 2.0,
        (min[2] + max[2]) / 2.0,
    ]
}

/// A stable, readable fallback colour for a texture name. The preview has no
/// atlas; this keeps grass, dirt, stone and each wood type distinct enough to
/// inspect geometry and model selection.
fn texture_color(texture: &str) -> [u8; 3] {
    let mut hash = 2_166_136_261u32;
    for byte in texture.bytes() {
        hash ^= u32::from(byte);
        hash = hash.wrapping_mul(16_777_619);
    }
    let r = 55 + (hash & 0xff) as u8 % 150;
    let g = 55 + ((hash >> 8) & 0xff) as u8 % 150;
    let b = 55 + ((hash >> 16) & 0xff) as u8 % 150;
    [r, g, b]
}

fn average_normal(vertices: &[&crate::Vertex]) -> [f32; 3] {
    let mut normal = [0.0; 3];
    for vertex in vertices {
        for (sum, value) in normal.iter_mut().zip(vertex.normal) {
            *sum += value;
        }
    }
    let length = (normal[0] * normal[0] + normal[1] * normal[1] + normal[2] * normal[2]).sqrt();
    if length > 1e-6 {
        normal.map(|value| value / length)
    } else {
        [0.0, 1.0, 0.0]
    }
}

fn rasterize(
    points: &[[f32; 3]],
    color: [u8; 3],
    rgb: &mut [u8],
    depth: &mut [f32],
    width: usize,
    height: usize,
) {
    let [a, b, c] = [points[0], points[1], points[2]];
    let area = edge(a, b, c);
    if area.abs() < 1e-5 {
        return;
    }
    let min_x = a[0].min(b[0]).min(c[0]).floor().max(0.0) as i32;
    let max_x = a[0].max(b[0]).max(c[0]).ceil().min(width as f32 - 1.0) as i32;
    let min_y = a[1].min(b[1]).min(c[1]).floor().max(0.0) as i32;
    let max_y = a[1].max(b[1]).max(c[1]).ceil().min(height as f32 - 1.0) as i32;
    for y in min_y..=max_y {
        for x in min_x..=max_x {
            let p = [x as f32 + 0.5, y as f32 + 0.5, 0.0];
            let w0 = edge(b, c, p) / area;
            let w1 = edge(c, a, p) / area;
            let w2 = edge(a, b, p) / area;
            if w0 < 0.0 || w1 < 0.0 || w2 < 0.0 {
                continue;
            }
            let z = w0 * a[2] + w1 * b[2] + w2 * c[2];
            let index = y as usize * width + x as usize;
            if z < depth[index] {
                depth[index] = z;
                rgb[index * 3..index * 3 + 3].copy_from_slice(&color);
            }
        }
    }
}

fn edge(a: [f32; 3], b: [f32; 3], p: [f32; 3]) -> f32 {
    (p[0] - a[0]) * (b[1] - a[1]) - (p[1] - a[1]) * (b[0] - a[0])
}

/// Write a binary PPM (P6), which needs no external image encoder.
pub fn write_ppm(path: &std::path::Path, preview: &Preview) -> Result<(), String> {
    use std::io::Write;
    let mut file = std::fs::File::create(path).map_err(|e| format!("{}: {e}", path.display()))?;
    write!(file, "P6\n{} {}\n255\n", preview.width, preview.height).map_err(|e| e.to_string())?;
    file.write_all(&preview.rgb).map_err(|e| e.to_string())
}

/// The default preview camera.
pub fn default_camera() -> (f32, f32, f32) {
    (PI / 4.0, 0.55, 7.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Mesh, Vertex};

    #[test]
    fn a_triangle_changes_pixels_and_is_deterministic() {
        let mesh = Mesh {
            vertices: vec![
                Vertex {
                    position: [0.0, 0.0, 0.0],
                    uv: [0.0, 0.0],
                    normal: [0.0, 1.0, 0.0],
                },
                Vertex {
                    position: [1.0, 0.0, 0.0],
                    uv: [1.0, 0.0],
                    normal: [0.0, 1.0, 0.0],
                },
                Vertex {
                    position: [0.0, 1.0, 0.0],
                    uv: [0.0, 1.0],
                    normal: [0.0, 1.0, 0.0],
                },
            ],
            indices: vec![0, 1, 2],
            textures: vec![],
        };
        let (yaw, pitch, scale) = default_camera();
        let first = render(&mesh, 64, 64, yaw, pitch, scale);
        let second = render(&mesh, 64, 64, yaw, pitch, scale);
        assert_eq!(first.rgb, second.rgb);
        assert!(first.rgb.chunks_exact(3).any(|pixel| pixel != [24, 28, 36]));
    }
}

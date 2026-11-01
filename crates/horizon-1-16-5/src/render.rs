//! wgpu rendering of a 1.16.5 CPU mesh.
//!
//! This is deliberately independent of the 26.3 client: it creates its own
//! instance, device and pipeline and draws one [`Mesh`] with an
//! [`TextureAtlas`]. [`OffscreenRenderer`] rasterizes into an RGBA8 texture,
//! reads it back and writes a PNG, so it runs on a build machine and in
//! tests; [`SurfaceRenderer`] presents the same mesh straight into a winit
//! window.
//!
//! The frame is a diagnostic image, not the game renderer: lighting is a
//! fixed directional term, there are no shadows, sky or entities. Opaque
//! and cutout quads are drawn first; quads whose texture is translucent
//! ([`is_translucent_texture`]) are drawn afterwards with alpha blending and
//! no depth writes, so glass and water show what is behind them. Grass and
//! foliage textures get the fixed plains biome tint while the CPU mesh is
//! expanded into GPU vertices.
//! [`SurfaceRenderer`] can additionally draw a clip-space HUD overlay with
//! alpha blending on top of the world, and atlas-sampled [`HudSprite`] icons
//! for the hotbar and inventory items.

use crate::Mesh;
use crate::atlas::{MISSING_TEXTURE, TextureAtlas};
use bytemuck::{Pod, Zeroable};
use std::fmt;
use wgpu::util::DeviceExt;

/// A rendered RGBA8 frame, top row first.
pub struct Frame {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

impl Frame {
    pub fn pixel(&self, x: u32, y: u32) -> Option<[u8; 4]> {
        if x >= self.width || y >= self.height {
            return None;
        }
        let offset = ((y as usize) * (self.width as usize) + x as usize) * 4;
        self.rgba
            .get(offset..offset + 4)
            .map(|pixel| [pixel[0], pixel[1], pixel[2], pixel[3]])
    }

    /// Writes the frame as an 8-bit RGBA PNG.
    pub fn write_png(&self, path: &std::path::Path) -> Result<(), String> {
        let file = std::fs::File::create(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let mut encoder = png::Encoder::new(std::io::BufWriter::new(file), self.width, self.height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().map_err(|e| e.to_string())?;
        writer
            .write_image_data(&self.rgba)
            .map_err(|e| e.to_string())?;
        writer.finish().map_err(|e| e.to_string())
    }
}

/// A perspective camera in block coordinates.
#[derive(Clone, Copy, Debug)]
pub struct Camera {
    pub eye: [f32; 3],
    pub target: [f32; 3],
    pub up: [f32; 3],
    pub fov_y_degrees: f32,
}

impl Camera {
    /// Frames the mesh bounds from `yaw`/`pitch` (radians), `margin` times the
    /// bounding radius away, looking at the bounds centre.
    pub fn framing(mesh: &Mesh, yaw: f32, pitch: f32, margin: f32) -> Self {
        let (center, radius) = bounds(mesh);
        let direction = [
            pitch.cos() * yaw.sin(),
            pitch.sin(),
            pitch.cos() * yaw.cos(),
        ];
        let distance = radius.max(0.5) * margin.max(1.0);
        Self {
            eye: [
                center[0] + direction[0] * distance,
                center[1] + direction[1] * distance,
                center[2] + direction[2] * distance,
            ],
            target: center,
            up: [0.0, 1.0, 0.0],
            fov_y_degrees: 60.0,
        }
    }

    pub fn looking_at(eye: [f32; 3], target: [f32; 3]) -> Self {
        Self {
            eye,
            target,
            up: [0.0, 1.0, 0.0],
            fov_y_degrees: 60.0,
        }
    }
}

fn bounds(mesh: &Mesh) -> ([f32; 3], f32) {
    if mesh.vertices.is_empty() {
        return ([0.0; 3], 1.0);
    }
    let mut min = [f32::INFINITY; 3];
    let mut max = [f32::NEG_INFINITY; 3];
    for vertex in &mesh.vertices {
        for axis in 0..3 {
            min[axis] = min[axis].min(vertex.position[axis]);
            max[axis] = max[axis].max(vertex.position[axis]);
        }
    }
    let center = [
        (min[0] + max[0]) * 0.5,
        (min[1] + max[1]) * 0.5,
        (min[2] + max[2]) * 0.5,
    ];
    let diagonal = [max[0] - min[0], max[1] - min[1], max[2] - min[2]];
    let radius = 0.5
        * (diagonal[0] * diagonal[0] + diagonal[1] * diagonal[1] + diagonal[2] * diagonal[2])
            .sqrt();
    (center, radius.max(0.5))
}

#[derive(Debug)]
pub enum RenderError {
    /// No graphics adapter is available in this environment.
    NoAdapter(String),
    /// A device, pipeline or readback step failed.
    Failed(String),
}

impl fmt::Display for RenderError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoAdapter(error) => write!(formatter, "no wgpu adapter: {error}"),
            Self::Failed(error) => formatter.write_str(error),
        }
    }
}

impl std::error::Error for RenderError {}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct GpuVertex {
    position: [f32; 3],
    uv: [f32; 2],
    normal: [f32; 3],
    /// Linear RGB biome tint applied to the sampled texel.
    tint: [f32; 3],
    /// Per-vertex 1.16.5 light (sky/block plus ambient occlusion), 0.05..1.
    level: f32,
}

/// A screen-space HUD vertex: a clip-space position and a straight
/// (unpremultiplied) RGBA colour. [`hud_rect`] builds quads from these.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Pod, Zeroable)]
pub struct HudVertex {
    /// Clip space, `-1..=1` on both axes, with `y` pointing up.
    pub position: [f32; 2],
    /// Straight RGBA in `0..=1`; the alpha channel is used for blending.
    pub color: [f32; 4],
}

/// Expands a rectangle given in normalized `0..=1` coordinates with a
/// top-left origin into six clip-space vertices: two triangles ordered
/// top-left, top-right, bottom-right and top-left, bottom-right, bottom-left.
pub fn hud_rect(x: f32, y: f32, w: f32, h: f32, color: [f32; 4]) -> [HudVertex; 6] {
    let corner = |x: f32, y: f32| HudVertex {
        position: [x * 2.0 - 1.0, 1.0 - y * 2.0],
        color,
    };
    [
        corner(x, y),
        corner(x + w, y),
        corner(x + w, y + h),
        corner(x, y),
        corner(x + w, y + h),
        corner(x, y + h),
    ]
}

/// One textured screen-space HUD sprite: where it goes on screen, the atlas
/// rectangle it samples and a straight RGBA multiplier.
///
/// [`hud_sprite`] expands it into six [`SpriteVertex`]es, and
/// [`SurfaceRenderer::render_with_hud`] draws them with the atlas-sampling
/// pipeline after the flat HUD, so the hotbar icons show the real block and
/// item sprites instead of averaged colour bands.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HudSprite {
    /// Normalized `0..=1` screen rectangle with a top-left origin:
    /// `x, y, width, height`.
    pub rect: [f32; 4],
    /// The atlas rectangle to sample as `u0, v0, u1, v1`. Sprite-local
    /// coordinates are `[0.0, 0.0, 1.0, 1.0]`; a packed sheet uses the
    /// sprite's own normalized atlas rectangle instead.
    pub uv: [f32; 4],
    /// Straight RGBA in `0..=1`, multiplied with the sampled texel.
    pub color: [f32; 4],
}

impl HudSprite {
    /// A sprite over `rect` sampling `uv`, drawn untinted.
    pub fn new(rect: [f32; 4], uv: [f32; 4]) -> Self {
        Self {
            rect,
            uv,
            color: [1.0, 1.0, 1.0, 1.0],
        }
    }
}

/// A textured screen-space HUD vertex: clip-space position, sprite UV and a
/// straight (unpremultiplied) RGBA colour. [`hud_sprite`] builds quads from
/// these and the sprite pipeline samples the atlas with their UVs.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Pod, Zeroable)]
pub struct SpriteVertex {
    /// Clip space, `-1..=1` on both axes, with `y` pointing up.
    pub position: [f32; 2],
    /// The atlas `u, v` this corner samples.
    pub uv: [f32; 2],
    /// Straight RGBA in `0..=1`; the alpha channel is used for blending.
    pub color: [f32; 4],
}

/// Expands a sprite into six clip-space vertices in the same order as
/// [`hud_rect`]: top-left, top-right, bottom-right, then top-left,
/// bottom-right, bottom-left, each carrying its corner of the `uv` rectangle.
pub fn hud_sprite(rect: [f32; 4], uv: [f32; 4], color: [f32; 4]) -> [SpriteVertex; 6] {
    let [x, y, w, h] = rect;
    let [u0, v0, u1, v1] = uv;
    let corner = |x: f32, y: f32, u: f32, v: f32| SpriteVertex {
        position: [x * 2.0 - 1.0, 1.0 - y * 2.0],
        uv: [u, v],
        color,
    };
    [
        corner(x, y, u0, v0),
        corner(x + w, y, u1, v0),
        corner(x + w, y + h, u1, v1),
        corner(x, y, u0, v0),
        corner(x + w, y + h, u1, v1),
        corner(x, y + h, u0, v1),
    ]
}

/// Flattens `sprites` into the vertex stream the sprite pipeline draws. An
/// empty slice yields no vertices at all, so a frame without sprites draws
/// exactly the same pixels as the flat HUD alone.
pub fn sprite_vertices(sprites: &[HudSprite]) -> Vec<SpriteVertex> {
    let mut vertices = Vec::with_capacity(sprites.len() * 6);
    for sprite in sprites {
        vertices.extend_from_slice(&hud_sprite(sprite.rect, sprite.uv, sprite.color));
    }
    vertices
}

/// The GPU vertices for a mixed list: axis-aligned [`HudSprite`]s plus raw
/// [`SpriteVertex`] triangles (a rotated quad such as the held block). Raw
/// vertices come first so a caller can append them in any order.
pub fn sprite_vertex_list(sprites: &[HudSprite], extra: &[SpriteVertex]) -> Vec<SpriteVertex> {
    let mut vertices = Vec::with_capacity(extra.len() + sprites.len() * 6);
    vertices.extend_from_slice(extra);
    vertices.extend_from_slice(&sprite_vertices(sprites));
    vertices
}

/// One tilted sprite quad, e.g. the block in the player's hand: the rect is
/// the sprite's centre and half-size in normalized screen units, `angle` is a
/// clockwise rotation in radians.
pub fn hud_sprite_rotated(
    center: [f32; 2],
    half: [f32; 2],
    angle: f32,
    uv: [f32; 4],
    color: [f32; 4],
) -> [SpriteVertex; 6] {
    let (sin, cos) = angle.sin_cos();
    let corners: [[f32; 2]; 4] = [
        [center[0] - half[0], center[1] - half[1]],
        [center[0] + half[0], center[1] - half[1]],
        [center[0] + half[0], center[1] + half[1]],
        [center[0] - half[0], center[1] + half[1]],
    ];
    let rotate = |point: [f32; 2]| {
        let dx = point[0] - center[0];
        let dy = point[1] - center[1];
        [
            center[0] + dx * cos - dy * sin,
            center[1] + dx * sin + dy * cos,
        ]
    };
    let uv_corners = [
        [uv[0], uv[1]],
        [uv[2], uv[1]],
        [uv[2], uv[3]],
        [uv[0], uv[3]],
    ];
    let mut vertices = [SpriteVertex::default(); 6];
    let order = [0usize, 1, 2, 0, 2, 3];
    for (slot, corner) in order.iter().enumerate() {
        let point = rotate(corners[*corner]);
        vertices[slot] = SpriteVertex {
            position: [point[0] * 2.0 - 1.0, 1.0 - point[1] * 2.0],
            uv: uv_corners[*corner],
            color,
        };
    }
    vertices
}

/// Expands a filled ellipse into `segments` triangle wedges (a fan around the
/// centre), so the touch joystick reads as a circle instead of a square.
///
/// `center` and `radius` are normalized `0..=1` screen units with a top-left
/// origin, matching [`hud_rect`]; passing `radius[0] / radius[1] == width /
/// height` keeps the disc circular in pixels on a non-square screen. Every
/// triangle is counter-clockwise in clip space, with at least three segments.
pub fn hud_disc(
    center: [f32; 2],
    radius: [f32; 2],
    color: [f32; 4],
    segments: usize,
) -> Vec<HudVertex> {
    let segments = segments.max(3);
    let clip = |x: f32, y: f32| HudVertex {
        position: [x * 2.0 - 1.0, 1.0 - y * 2.0],
        color,
    };
    let rim = |step: usize| {
        let angle = std::f32::consts::TAU * step as f32 / segments as f32;
        clip(
            center[0] + angle.cos() * radius[0],
            center[1] + angle.sin() * radius[1],
        )
    };
    let hub = clip(center[0], center[1]);
    let mut vertices = Vec::with_capacity(segments * 3);
    for step in 0..segments {
        // Hub, next rim point, this rim point: a positive (counter-clockwise)
        // signed area in clip space, unlike the top-left-first `hud_rect`
        // quads, which the HUD pipeline draws with culling disabled.
        vertices.push(hub);
        vertices.push(rim(step + 1));
        vertices.push(rim(step));
    }
    vertices
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct CameraUniform {
    view_proj: [f32; 16],
    /// Camera eye in block coordinates, for distance fog.
    eye: [f32; 4],
}

const SHADER: &str = r#"
struct Camera {
    view_proj: mat4x4<f32>,
    eye: vec4<f32>,
};
@group(0) @binding(0) var<uniform> camera: Camera;

const SKY: vec3<f32> = vec3<f32>(0.47, 0.65, 1.0);
// The sky reads lighter at the horizon and deeper overhead.
const SKY_HORIZON: vec3<f32> = vec3<f32>(0.62, 0.74, 1.0);
const SKY_ZENITH: vec3<f32> = vec3<f32>(0.30, 0.48, 0.92);
const FOG_NEAR: f32 = 48.0;
const FOG_FAR: f32 = 96.0;

// Distance fog toward the sky colour, as 1.16.5 blends the horizon.
fn fogged(color: vec3<f32>, world: vec3<f32>) -> vec3<f32> {
    let distance = length(world - camera.eye.xyz);
    let amount = clamp((distance - FOG_NEAR) / (FOG_FAR - FOG_NEAR), 0.0, 1.0);
    let horizon = mix(SKY_HORIZON, SKY_ZENITH, clamp((world.y - camera.eye.y + 48.0) / 160.0, 0.0, 1.0));
    return mix(color, horizon, amount);
}
@group(0) @binding(1) var atlas_texture: texture_2d<f32>;
@group(0) @binding(2) var atlas_sampler: sampler;

struct VertexInput {
    @location(0) position: vec3<f32>,
    @location(1) uv: vec2<f32>,
    @location(2) normal: vec3<f32>,
    @location(3) tint: vec3<f32>,
    @location(4) level: f32,
};
struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) light: f32,
    @location(2) tint: vec3<f32>,
    @location(3) world: vec3<f32>,
};

@vertex
fn vs_main(input: VertexInput) -> VertexOutput {
    var output: VertexOutput;
    output.clip_position = camera.view_proj * vec4<f32>(input.position, 1.0);
    output.uv = input.uv;
    output.world = input.position;
    output.tint = input.tint;
    // The CPU already multiplied in vanilla's per-face shade (1.0 / 0.8 /
    // 0.6 / 0.5), so the vertex value is the final brightness.
    output.light = input.level;
    return output;
}

@fragment
fn fs_main(input: VertexOutput) -> @location(0) vec4<f32> {
    let texel = textureSample(atlas_texture, atlas_sampler, input.uv);
    if (texel.a < 0.5) {
        discard;
    }
    return vec4<f32>(fogged(texel.rgb * input.tint * input.light, input.world), 1.0);
}

// The blended pass keeps the sampled alpha so glass, water and ice mix with
// whatever the opaque pass already wrote. Only fully transparent texels are
// dropped; the 0.5 cutout above would erase low-alpha glass entirely.
@fragment
fn fs_blend(input: VertexOutput) -> @location(0) vec4<f32> {
    let texel = textureSample(atlas_texture, atlas_sampler, input.uv);
    if (texel.a <= 0.0) {
        discard;
    }
    return vec4<f32>(fogged(texel.rgb * input.tint * input.light, input.world), texel.a);
}
"#;

const HUD_SHADER: &str = r#"
struct VertexInput {
    @location(0) position: vec2<f32>,
    @location(1) color: vec4<f32>,
};
struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) color: vec4<f32>,
};

@vertex
fn vs_main(input: VertexInput) -> VertexOutput {
    var output: VertexOutput;
    output.clip_position = vec4<f32>(input.position, 0.0, 1.0);
    output.color = input.color;
    return output;
}

@fragment
fn fs_main(input: VertexOutput) -> @location(0) vec4<f32> {
    return input.color;
}
"#;

// The textured HUD pass samples the same packed atlas as the world, with the
// nearest/clamp sampler bound by the renderer, so a hotbar icon keeps its
// crisp pixels at any screen size. The vertex colour tints the texel and its
// alpha (straight, alpha blending) lets glass items stay see-through.
const HUD_SPRITE_SHADER: &str = r#"
@group(0) @binding(0) var atlas_texture: texture_2d<f32>;
@group(0) @binding(1) var atlas_sampler: sampler;

struct VertexInput {
    @location(0) position: vec2<f32>,
    @location(1) uv: vec2<f32>,
    @location(2) color: vec4<f32>,
};
struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) color: vec4<f32>,
};

@vertex
fn vs_main(input: VertexInput) -> VertexOutput {
    var output: VertexOutput;
    output.clip_position = vec4<f32>(input.position, 0.0, 1.0);
    output.uv = input.uv;
    output.color = input.color;
    return output;
}

@fragment
fn fs_main(input: VertexOutput) -> @location(0) vec4<f32> {
    let texel = textureSample(atlas_texture, atlas_sampler, input.uv);
    return vec4<f32>(texel.rgb * input.color.rgb, texel.a * input.color.a);
}
"#;

/// A reusable offscreen wgpu device and pipeline. Creating one asks the
/// system for a graphics adapter, which can fail (`RenderError::NoAdapter`)
/// on a machine without a driver.
pub struct OffscreenRenderer {
    device: wgpu::Device,
    queue: wgpu::Queue,
    pipeline: wgpu::RenderPipeline,
    translucent_pipeline: wgpu::RenderPipeline,
}

impl OffscreenRenderer {
    pub fn new() -> Result<Self, RenderError> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        let adapter = match pollster::block_on(
            instance.request_adapter(&wgpu::RequestAdapterOptions::default()),
        ) {
            Ok(adapter) => adapter,
            Err(first) => {
                pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
                    force_fallback_adapter: true,
                    ..Default::default()
                }))
                .map_err(|second| RenderError::NoAdapter(format!("{first}; fallback: {second}")))?
            }
        };
        let (device, queue) =
            pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default()))
                .map_err(|error| RenderError::Failed(format!("requesting a device: {error}")))?;

        let pipeline = create_pipeline(
            &device,
            "horizon-1-16-5 offscreen pipeline",
            wgpu::TextureFormat::Rgba8UnormSrgb,
            WorldPass::Opaque,
        );
        let translucent_pipeline = create_pipeline(
            &device,
            "horizon-1-16-5 offscreen translucent pipeline",
            wgpu::TextureFormat::Rgba8UnormSrgb,
            WorldPass::Translucent,
        );
        Ok(Self {
            device,
            queue,
            pipeline,
            translucent_pipeline,
        })
    }

    /// Rasterizes `mesh`, sampling `atlas` by each quad's texture name.
    /// Opaque and cutout quads are drawn in the first pass; quads whose
    /// texture is translucent are then alpha blended over them without
    /// writing depth.
    pub fn render(
        &self,
        mesh: &Mesh,
        atlas: &TextureAtlas,
        camera: Camera,
        width: u32,
        height: u32,
    ) -> Result<Frame, RenderError> {
        self.render_lit(mesh, atlas, camera, width, height, &[])
    }

    /// [`Self::render`] with one 1.16.5 light value per mesh vertex.
    pub fn render_lit(
        &self,
        mesh: &Mesh,
        atlas: &TextureAtlas,
        camera: Camera,
        width: u32,
        height: u32,
        light: &[f32],
    ) -> Result<Frame, RenderError> {
        if width == 0 || height == 0 {
            return Err(RenderError::Failed("frame size must be non-zero".into()));
        }
        let prepared = prepare_mesh(mesh, atlas, light)?;

        let view_proj = view_projection(camera, width as f32 / height as f32, mesh);
        let uniform = CameraUniform {
            view_proj,
            eye: [camera.eye[0], camera.eye[1], camera.eye[2], 1.0],
        };
        let uniform_buffer = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("horizon-1-16-5 camera"),
                contents: bytemuck::bytes_of(&uniform),
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            });
        let vertex_buffer = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("horizon-1-16-5 vertices"),
                contents: bytemuck::cast_slice(&prepared.vertices),
                usage: wgpu::BufferUsages::VERTEX,
            });
        let opaque_indices = index_buffer(
            &self.device,
            "horizon-1-16-5 opaque indices",
            &prepared.opaque_indices,
        );
        let translucent_indices = index_buffer(
            &self.device,
            "horizon-1-16-5 translucent indices",
            &prepared.translucent_indices,
        );

        let atlas_texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("horizon-1-16-5 atlas"),
            size: wgpu::Extent3d {
                width: atlas.width(),
                height: atlas.height(),
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        self.queue.write_texture(
            atlas_texture.as_image_copy(),
            atlas.rgba(),
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(atlas.width().max(1) * 4),
                rows_per_image: Some(atlas.height().max(1)),
            },
            wgpu::Extent3d {
                width: atlas.width(),
                height: atlas.height(),
                depth_or_array_layers: 1,
            },
        );
        let atlas_view = atlas_texture.create_view(&wgpu::TextureViewDescriptor::default());
        let sampler = self.device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("horizon-1-16-5 atlas sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            mipmap_filter: wgpu::MipmapFilterMode::Nearest,
            ..Default::default()
        });
        // The two world pipelines get their own auto-generated bind group
        // layouts, so the same camera/texture/sampler resources are bound
        // once per pipeline.
        let create_bind_group = |label: &str, layout: &wgpu::BindGroupLayout| {
            self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some(label),
                layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: uniform_buffer.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(&atlas_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: wgpu::BindingResource::Sampler(&sampler),
                    },
                ],
            })
        };
        let opaque_bind_group = create_bind_group(
            "horizon-1-16-5 opaque bind group",
            &self.pipeline.get_bind_group_layout(0),
        );
        let translucent_bind_group = create_bind_group(
            "horizon-1-16-5 translucent bind group",
            &self.translucent_pipeline.get_bind_group_layout(0),
        );

        let target = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("horizon-1-16-5 frame"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let target_view = target.create_view(&wgpu::TextureViewDescriptor::default());
        let depth = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("horizon-1-16-5 depth"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Depth32Float,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });
        let depth_view = depth.create_view(&wgpu::TextureViewDescriptor::default());

        let row_bytes = padded_row(width);
        let readback = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("horizon-1-16-5 readback"),
            size: u64::from(row_bytes) * u64::from(height),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("horizon-1-16-5 offscreen"),
            });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("horizon-1-16-5 offscreen pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &target_view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(sky_clear()),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &depth_view,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(1.0),
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            if let Some(indices) = &opaque_indices {
                pass.set_pipeline(&self.pipeline);
                pass.set_bind_group(0, &opaque_bind_group, &[]);
                pass.set_vertex_buffer(0, vertex_buffer.slice(..));
                pass.set_index_buffer(indices.slice(..), wgpu::IndexFormat::Uint32);
                pass.draw_indexed(0..prepared.opaque_indices.len() as u32, 0, 0..1);
            }
        }
        if let Some(indices) = &translucent_indices {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("horizon-1-16-5 offscreen translucent pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &target_view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &depth_view,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&self.translucent_pipeline);
            pass.set_bind_group(0, &translucent_bind_group, &[]);
            pass.set_vertex_buffer(0, vertex_buffer.slice(..));
            pass.set_index_buffer(indices.slice(..), wgpu::IndexFormat::Uint32);
            pass.draw_indexed(0..prepared.translucent_indices.len() as u32, 0, 0..1);
        }
        encoder.copy_texture_to_buffer(
            target.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &readback,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(row_bytes),
                    rows_per_image: Some(height),
                },
            },
            wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
        );
        self.queue.submit([encoder.finish()]);

        let (sender, receiver) = std::sync::mpsc::channel();
        readback.map_async(wgpu::MapMode::Read, .., move |result| {
            let _ = sender.send(result);
        });
        self.device
            .poll(wgpu::PollType::wait_indefinitely())
            .map_err(|error| RenderError::Failed(format!("waiting for the frame: {error}")))?;
        receiver
            .recv()
            .map_err(|error| RenderError::Failed(format!("mapping the frame: {error}")))?
            .map_err(|error| RenderError::Failed(format!("mapping the frame: {error}")))?;

        let mut rgba = Vec::with_capacity((width * height * 4) as usize);
        {
            let mapped = readback
                .get_mapped_range(..)
                .map_err(|error| RenderError::Failed(format!("reading the frame: {error}")))?;
            for row in 0..height as usize {
                let start = row * row_bytes as usize;
                rgba.extend_from_slice(&mapped[start..start + width as usize * 4]);
            }
        }
        readback.unmap();
        Ok(Frame {
            width,
            height,
            rgba,
        })
    }
}

/// Which of the two world passes a pipeline serves. Both passes share the
/// shader module and vertex layout; they differ in fragment entry point,
/// blending and depth writes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WorldPass {
    /// Cutout geometry drawn first: alpha-tested, depth written, opaque.
    Opaque,
    /// Transparent geometry drawn second: alpha blended, depth tested but
    /// not written, so overlapping glass does not hide what is behind it.
    Translucent,
}

/// Builds a world pipeline for `pass`: one triangle-list pass with position,
/// UV, normal and tint vertex attributes, back-face culling, depth testing
/// against the 1.16.5 conventions and a single `format` colour target.
/// Offscreen and windowed rendering use the same shaders; only the colour
/// format differs.
fn create_pipeline(
    device: &wgpu::Device,
    label: &str,
    format: wgpu::TextureFormat,
    pass: WorldPass,
) -> wgpu::RenderPipeline {
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("horizon-1-16-5 mesh shader"),
        source: wgpu::ShaderSource::Wgsl(SHADER.into()),
    });
    let attributes = wgpu::vertex_attr_array![
        0 => Float32x3,
        1 => Float32x2,
        2 => Float32x3,
        3 => Float32x3,
        4 => Float32,
    ];
    let (entry_point, blend, depth_write) = match pass {
        WorldPass::Opaque => ("fs_main", None, true),
        WorldPass::Translucent => ("fs_blend", Some(wgpu::BlendState::ALPHA_BLENDING), false),
    };
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some(label),
        layout: None,
        vertex: wgpu::VertexState {
            module: &shader,
            entry_point: Some("vs_main"),
            compilation_options: Default::default(),
            buffers: &[Some(wgpu::VertexBufferLayout {
                array_stride: std::mem::size_of::<GpuVertex>() as u64,
                step_mode: wgpu::VertexStepMode::Vertex,
                attributes: &attributes,
            })],
        },
        fragment: Some(wgpu::FragmentState {
            module: &shader,
            entry_point: Some(entry_point),
            compilation_options: Default::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format,
                blend,
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        primitive: wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::TriangleList,
            strip_index_format: None,
            front_face: wgpu::FrontFace::Ccw,
            cull_mode: Some(wgpu::Face::Back),
            polygon_mode: wgpu::PolygonMode::Fill,
            unclipped_depth: false,
            conservative: false,
        },
        depth_stencil: Some(wgpu::DepthStencilState {
            format: wgpu::TextureFormat::Depth32Float,
            depth_write_enabled: Some(depth_write),
            depth_compare: Some(wgpu::CompareFunction::Less),
            stencil: wgpu::StencilState::default(),
            bias: wgpu::DepthBiasState::default(),
        }),
        multisample: wgpu::MultisampleState::default(),
        multiview_mask: None,
        cache: None,
    })
}

/// Builds the HUD pipeline: clip-space position/colour triangles with alpha
/// blending and no depth attachment, drawn after the world pass.
fn create_hud_pipeline(
    device: &wgpu::Device,
    label: &str,
    format: wgpu::TextureFormat,
) -> wgpu::RenderPipeline {
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("horizon-1-16-5 hud shader"),
        source: wgpu::ShaderSource::Wgsl(HUD_SHADER.into()),
    });
    let attributes = wgpu::vertex_attr_array![
        0 => Float32x2,
        1 => Float32x4,
    ];
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some(label),
        layout: None,
        vertex: wgpu::VertexState {
            module: &shader,
            entry_point: Some("vs_main"),
            compilation_options: Default::default(),
            buffers: &[Some(wgpu::VertexBufferLayout {
                array_stride: std::mem::size_of::<HudVertex>() as u64,
                step_mode: wgpu::VertexStepMode::Vertex,
                attributes: &attributes,
            })],
        },
        fragment: Some(wgpu::FragmentState {
            module: &shader,
            entry_point: Some("fs_main"),
            compilation_options: Default::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format,
                blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        primitive: wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::TriangleList,
            strip_index_format: None,
            front_face: wgpu::FrontFace::Ccw,
            cull_mode: None,
            polygon_mode: wgpu::PolygonMode::Fill,
            unclipped_depth: false,
            conservative: false,
        },
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        multiview_mask: None,
        cache: None,
    })
}

/// Builds the textured HUD pipeline: clip-space position/UV/colour triangles
/// that sample the atlas through the renderer's nearest/clamp sampler, with
/// alpha blending and no depth attachment, drawn after the flat HUD.
fn create_hud_sprite_pipeline(
    device: &wgpu::Device,
    label: &str,
    format: wgpu::TextureFormat,
) -> wgpu::RenderPipeline {
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("horizon-1-16-5 hud sprite shader"),
        source: wgpu::ShaderSource::Wgsl(HUD_SPRITE_SHADER.into()),
    });
    // An explicit layout, so the sprite pass binds the same atlas texture and
    // sampler resources the world pass uses instead of the shader inferred
    // without knowing the filter mode.
    let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("horizon-1-16-5 hud sprite bind group layout"),
        entries: &[
            wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 1,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                count: None,
            },
        ],
    });
    let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("horizon-1-16-5 hud sprite pipeline layout"),
        bind_group_layouts: &[Some(&bind_group_layout)],
        ..Default::default()
    });
    let attributes = wgpu::vertex_attr_array![
        0 => Float32x2,
        1 => Float32x2,
        2 => Float32x4,
    ];
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some(label),
        layout: Some(&layout),
        vertex: wgpu::VertexState {
            module: &shader,
            entry_point: Some("vs_main"),
            compilation_options: Default::default(),
            buffers: &[Some(wgpu::VertexBufferLayout {
                array_stride: std::mem::size_of::<SpriteVertex>() as u64,
                step_mode: wgpu::VertexStepMode::Vertex,
                attributes: &attributes,
            })],
        },
        fragment: Some(wgpu::FragmentState {
            module: &shader,
            entry_point: Some("fs_main"),
            compilation_options: Default::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format,
                blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        primitive: wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::TriangleList,
            strip_index_format: None,
            front_face: wgpu::FrontFace::Ccw,
            cull_mode: None,
            polygon_mode: wgpu::PolygonMode::Fill,
            unclipped_depth: false,
            conservative: false,
        },
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        multiview_mask: None,
        cache: None,
    })
}

/// Decodes a packed sRGB colour (`0xRRGGBB`) into linear RGB, as the atlas
/// samples are linear after the sRGB texture decode.
fn srgb_to_linear(color: u32) -> [f32; 3] {
    let channel = |shift: u32| {
        let srgb = ((color >> shift) & 0xff) as f32 / 255.0;
        if srgb <= 0.04045 {
            srgb / 12.92
        } else {
            ((srgb + 0.055) / 1.055).powf(2.4)
        }
    };
    [channel(16), channel(8), channel(0)]
}

/// The plains biome grass colour (`0x91BD59`) in linear RGB.
static GRASS_TINT: std::sync::LazyLock<[f32; 3]> =
    std::sync::LazyLock::new(|| srgb_to_linear(0x91BD59));

/// The plains biome foliage colour (`0x59AE30`) in linear RGB.
static FOLIAGE_TINT: std::sync::LazyLock<[f32; 3]> =
    std::sync::LazyLock::new(|| srgb_to_linear(0x59AE30));

/// Whether a quad using `name` belongs to the alpha-blended pass.
///
/// The translucent block sprites in 1.16.5 are glass (plain, panes, stained),
/// water, every ice variant, slime and honey. Names are matched by whole
/// sprite-name token after the namespace and `block/` folder, so
/// `minecraft:block/glass`, `white_stained_glass`, `glass_pane_top`,
/// `water_still`, `blue_ice`, `slime_block` and `honey_block_top` all blend,
/// while `stone`, `oak_planks` and `diorite` stay opaque.
/// The list mirrors the vanilla render types; `assets` may call it when it
/// starts modelling render layers.
pub fn is_translucent_texture(name: &str) -> bool {
    let path = name.split_once(':').map_or(name, |(_, path)| path);
    let sprite = path.strip_prefix("block/").unwrap_or(path);
    sprite
        .split('_')
        .any(|token| matches!(token, "glass" | "water" | "ice" | "slime" | "honey"))
}

/// The 1.16.5 daylight sky colour (RGB 0..1), used for the clear and, mixed
/// with the far fog, for the horizon haze.
pub const SKY_COLOR: [f32; 3] = [0.47, 0.65, 1.0];
/// Fog ramp: fully clear until this many blocks, fully sky-coloured beyond
/// `FOG_FAR`, matching the vanilla render distance feel at short radii.
pub const FOG_NEAR: f32 = 48.0;
/// Distance at which geometry is fully fogged.
pub const FOG_FAR: f32 = 96.0;

/// Sky clear colour for a frame.
pub fn sky_clear() -> wgpu::Color {
    wgpu::Color {
        r: f64::from(SKY_COLOR[0]),
        g: f64::from(SKY_COLOR[1]),
        b: f64::from(SKY_COLOR[2]),
        a: 1.0,
    }
}

/// Vanilla's face shading: top faces brightest, bottom darkest, sides between.
fn face_shade(normal: [f32; 3]) -> f32 {
    // Vanilla's per-face brightness (`Block.getShade`/`Direction` constants):
    // top 1.0, north/south 0.8, east/west 0.6, bottom 0.5.
    let x = normal[0].abs();
    let y = normal[1].abs();
    let z = normal[2].abs();
    if y >= x && y >= z {
        if normal[1] >= 0.0 { 1.0 } else { 0.5 }
    } else if z >= x {
        0.8
    } else {
        0.6
    }
}

/// The fixed plains biome tint for a quad's texture name.
///
/// Vanilla leaves grass and foliage sprites untinted and multiplies them by
/// the biome colour while chunk meshing; this renderer applies the same
/// colour when it expands a quad into GPU vertices. Names are matched after
/// the namespace and `block/` folder, so `minecraft:block/oak_leaves`,
/// `oak_leaves` and `block/oak_leaves` all tint alike. Every unrelated
/// texture keeps `[1, 1, 1]`.
pub fn tint_for_texture(name: &str) -> [f32; 3] {
    let path = name.split_once(':').map_or(name, |(_, path)| path);
    let sprite = path.strip_prefix("block/").unwrap_or(path);
    if matches!(
        sprite,
        "grass_block_top"
            | "grass_block_side_overlay"
            | "grass"
            | "fern"
            | "tall_grass"
            | "large_fern"
            | "sugar_cane"
    ) {
        return *GRASS_TINT;
    }
    if sprite.ends_with("_leaves") || matches!(sprite, "vine" | "lily_pad") {
        return *FOLIAGE_TINT;
    }
    [1.0, 1.0, 1.0]
}

/// Validates a mesh and expands it into GPU vertices, mapping each quad's
/// per-face UVs (sixteenths of a tile) into its atlas region.
fn build_vertices(
    mesh: &Mesh,
    atlas: &TextureAtlas,
    light: &[f32],
) -> Result<Vec<GpuVertex>, RenderError> {
    if mesh.vertices.len() % 4 != 0 {
        return Err(RenderError::Failed(
            "mesh vertex count is not a multiple of four".into(),
        ));
    }
    let quads = mesh.vertices.len() / 4;
    if mesh.textures.len() != quads {
        return Err(RenderError::Failed(format!(
            "mesh has {quads} quads but {} texture names",
            mesh.textures.len()
        )));
    }
    // An empty mesh is a valid frame: a network world starts with no chunks
    // loaded, so the first frames show only the sky.
    if mesh.indices.is_empty() {
        return Ok(Vec::new());
    }
    let missing_uv = atlas
        .uv(MISSING_TEXTURE)
        .ok_or_else(|| RenderError::Failed("atlas has no missing sprite".into()))?;

    if !light.is_empty() && light.len() != mesh.vertices.len() {
        return Err(RenderError::Failed(format!(
            "{} light values for {} vertices",
            light.len(),
            mesh.vertices.len()
        )));
    }
    let mut vertices = Vec::with_capacity(mesh.vertices.len());
    for (quad_index, quad) in mesh.vertices.chunks_exact(4).enumerate() {
        let uv_rect = atlas.uv(&mesh.textures[quad_index]).unwrap_or(missing_uv);
        let tint = tint_for_texture(&mesh.textures[quad_index]);
        for (corner, vertex) in quad.iter().enumerate() {
            let local = [vertex.uv[0] / 16.0, vertex.uv[1] / 16.0];
            let vertex_index = quad_index * 4 + corner;
            // No captured light (tests, synthetic meshes): full daylight on
            // the face, so shading still comes from the face normal.
            let level = light.get(vertex_index).copied().unwrap_or(1.0);
            vertices.push(GpuVertex {
                position: vertex.position,
                uv: [
                    uv_rect[0] + local[0] * (uv_rect[2] - uv_rect[0]),
                    uv_rect[1] + local[1] * (uv_rect[3] - uv_rect[1]),
                ],
                normal: vertex.normal,
                tint,
                level: level * face_shade(vertex.normal),
            });
        }
    }
    let vertex_count = vertices.len() as u32;
    if mesh.indices.iter().any(|index| *index >= vertex_count) {
        return Err(RenderError::Failed(
            "mesh index is outside the vertex buffer".into(),
        ));
    }
    Ok(vertices)
}

/// A mesh ready for upload: one shared vertex buffer and the triangle indices
/// of each world pass. The passes share the vertex buffer because splitting
/// by index keeps the per-quad texture and UV expansion unchanged.
struct PreparedMesh {
    vertices: Vec<GpuVertex>,
    opaque_indices: Vec<u32>,
    translucent_indices: Vec<u32>,
}

/// Partitions a mesh's triangle indices into the opaque and translucent
/// passes. A quad emits two consecutive triangles, so the material of the
/// triangle starting at `indices` is `mesh.textures[triangle_index / 2]`;
/// this matches the [`Mesh::textures`] layout and needs no extra per-quad
/// flag in the CPU mesh. Index and vertex order is preserved inside each
/// pass.
fn split_indices(mesh: &Mesh) -> Result<(Vec<u32>, Vec<u32>), RenderError> {
    let mut opaque = Vec::with_capacity(mesh.indices.len());
    let mut translucent = Vec::new();
    for (triangle, indices) in mesh.indices.chunks_exact(3).enumerate() {
        let texture = mesh.textures.get(triangle / 2).ok_or_else(|| {
            RenderError::Failed(format!(
                "mesh triangle {triangle} has no texture (mesh has {} quads)",
                mesh.textures.len()
            ))
        })?;
        if is_translucent_texture(texture) {
            translucent.extend_from_slice(indices);
        } else {
            opaque.extend_from_slice(indices);
        }
    }
    Ok((opaque, translucent))
}

/// Validates `mesh`, expands it into GPU vertices and splits its triangles
/// into the two world passes.
fn prepare_mesh(
    mesh: &Mesh,
    atlas: &TextureAtlas,
    light: &[f32],
) -> Result<PreparedMesh, RenderError> {
    let vertices = build_vertices(mesh, atlas, light)?;
    let (opaque_indices, translucent_indices) = split_indices(mesh)?;
    Ok(PreparedMesh {
        vertices,
        opaque_indices,
        translucent_indices,
    })
}

/// Uploads `indices` as an index buffer, or returns `None` for an empty
/// pass; wgpu does not need a zero-sized buffer and the pass is skipped.
fn index_buffer(device: &wgpu::Device, label: &str, indices: &[u32]) -> Option<wgpu::Buffer> {
    if indices.is_empty() {
        return None;
    }
    Some(
        device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some(label),
            contents: bytemuck::cast_slice(indices),
            usage: wgpu::BufferUsages::INDEX,
        }),
    )
}

fn create_depth(device: &wgpu::Device, width: u32, height: u32) -> wgpu::TextureView {
    device
        .create_texture(&wgpu::TextureDescriptor {
            label: Some("horizon-1-16-5 depth"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Depth32Float,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        })
        .create_view(&wgpu::TextureViewDescriptor::default())
}

/// The extent a frame is drawn into: the acquired swapchain texture always
/// wins over the configured size.
///
/// `wgpu` can hand back a frame whose extent differs from the configuration:
/// Android's compositor resizes the native window under the surface, and a
/// missed `Resized` leaves the configuration stale. Drawing into the frame's
/// own extent covers every pixel the compositor shows instead of leaving the
/// uncovered part black. A zero-sized frame is impossible in practice and
/// falls back to the configured size.
fn fit_extent(configured: [u32; 2], frame: [u32; 2]) -> [u32; 2] {
    if frame[0] == 0 || frame[1] == 0 {
        configured
    } else {
        frame
    }
}

/// The swapchain extent a renderer starts from.
///
/// `Window::inner_size` is already in physical pixels (the surface extent) on
/// every backend, Android included, so it is never scaled by the window's
/// factor. A window that reports zero (it has not been laid out yet) becomes
/// 1, and anything past the device's texture limit is clamped, because `wgpu`
/// cannot configure either; the frame read back on the first draw corrects a
/// window that was still reporting a placeholder size here.
fn initial_extent(inner: [u32; 2], max: u32) -> [u32; 2] {
    let max = max.max(1);
    [inner[0].clamp(1, max), inner[1].clamp(1, max)]
}

/// The pixel aspect ratio a frame is projected with.
///
/// The camera must use the extent actually drawn into rather than the
/// configured one, or a stale swapchain stretches the world.
fn aspect_of(extent: [u32; 2]) -> f32 {
    extent[0].max(1) as f32 / extent[1].max(1) as f32
}

/// A wgpu device, world and HUD pipelines, and swapchain that presents one
/// [`Mesh`] into a winit window. Creating one asks the system for a graphics
/// adapter that can present to that window, which can fail
/// (`RenderError::NoAdapter`) on a machine without a usable display.
pub struct SurfaceRenderer {
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    pipeline: wgpu::RenderPipeline,
    translucent_pipeline: wgpu::RenderPipeline,
    hud_pipeline: wgpu::RenderPipeline,
    hud_sprite_pipeline: wgpu::RenderPipeline,
    format: wgpu::TextureFormat,
    size: [u32; 2],
    depth: wgpu::TextureView,
}

impl SurfaceRenderer {
    pub fn new(window: std::sync::Arc<winit::window::Window>) -> Result<Self, RenderError> {
        // `inner_size` is the physical surface extent on every backend,
        // Android included, so it is taken as-is: converting it through the
        // window's scale factor would configure a swapchain narrower than
        // the surface and leave the uncovered part black. The frame read
        // back on the first draw corrects a window that still reports a
        // placeholder size here.
        let inner = window.inner_size();
        let requested = [inner.width, inner.height];
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_with_display_handle(
            Box::new(window.clone()),
        ));
        let surface = instance
            .create_surface(window)
            .map_err(|error| RenderError::Failed(format!("creating a window surface: {error}")))?;
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            compatible_surface: Some(&surface),
            ..Default::default()
        }))
        .map_err(|error| {
            RenderError::NoAdapter(format!("no adapter can present to the window: {error}"))
        })?;
        let capabilities = surface.get_capabilities(&adapter);
        let format = [
            wgpu::TextureFormat::Rgba8UnormSrgb,
            wgpu::TextureFormat::Bgra8UnormSrgb,
        ]
        .into_iter()
        .find(|format| capabilities.formats.contains(format))
        .ok_or_else(|| {
            RenderError::Failed(format!(
                "the surface offers no sRGB format (offered: {:?})",
                capabilities.formats
            ))
        })?;
        let (device, queue) =
            pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default()))
                .map_err(|error| RenderError::Failed(format!("requesting a device: {error}")))?;
        let max = device.limits().max_texture_dimension_2d;
        let size = initial_extent(requested, max);
        let pipeline = create_pipeline(
            &device,
            "horizon-1-16-5 surface pipeline",
            format,
            WorldPass::Opaque,
        );
        let translucent_pipeline = create_pipeline(
            &device,
            "horizon-1-16-5 surface translucent pipeline",
            format,
            WorldPass::Translucent,
        );
        let hud_pipeline =
            create_hud_pipeline(&device, "horizon-1-16-5 surface hud pipeline", format);
        let hud_sprite_pipeline = create_hud_sprite_pipeline(
            &device,
            "horizon-1-16-5 surface hud sprite pipeline",
            format,
        );
        let depth = create_depth(&device, size[0], size[1]);
        let renderer = Self {
            surface,
            device,
            queue,
            pipeline,
            translucent_pipeline,
            hud_pipeline,
            hud_sprite_pipeline,
            format,
            size,
            depth,
        };
        renderer.configure();
        Ok(renderer)
    }

    /// Follows the window to `width` x `height`, both in physical pixels.
    ///
    /// On Android these come from the compositor's surface and are already
    /// physical, so they are applied without a scale-factor conversion. A
    /// zero dimension (a minimized window) or a dimension past the device's
    /// texture limit keeps the last swapchain, which `wgpu` cannot configure.
    pub fn resize(&mut self, width: u32, height: u32) {
        if width == 0 || height == 0 {
            return;
        }
        let max = self.device.limits().max_texture_dimension_2d;
        if width > max || height > max {
            return;
        }
        self.size = [width, height];
        self.depth = create_depth(&self.device, width, height);
        self.configure();
    }

    /// The extent the swapchain was last configured with.
    ///
    /// `app` compares this against `Window::inner_size` on every redraw:
    /// Android can miss the `Resized` that would otherwise report a new
    /// surface size, and a stale swapchain leaves part of the window black.
    pub fn native_size(&self) -> [u32; 2] {
        self.size
    }

    /// Rasterizes `mesh`, sampling `atlas` by each quad's texture name, and
    /// presents it to the window. Opaque and cutout quads are drawn first;
    /// quads whose texture is translucent are then alpha blended over them
    /// without writing depth. The atlas is uploaded on every call.
    ///
    /// `Ok(())` means the frame was submitted for presentation, except that
    /// transient conditions where the compositor cannot take a frame right
    /// now (a timeout, occlusion, or a swapchain that must be reconfigured)
    /// also return `Ok(())` without drawing; the next call draws normally.
    /// A lost surface or a misconfigured swapchain is a hard `Err`.
    pub fn render(
        &mut self,
        mesh: &Mesh,
        atlas: &TextureAtlas,
        camera: Camera,
    ) -> Result<(), RenderError> {
        self.render_with_hud(mesh, atlas, camera, &[], &[])
    }

    /// Rasterizes `mesh` exactly like [`Self::render`], then draws `hud`
    /// clip-space triangles and, after them, the `sprites` sampled from
    /// `atlas` with alpha blending and no depth writes before presenting the
    /// combined frame. Empty slices draw nothing extra, so `render` keeps its
    /// original output.
    pub fn render_with_hud(
        &mut self,
        mesh: &Mesh,
        atlas: &TextureAtlas,
        camera: Camera,
        hud: &[HudVertex],
        sprites: &[HudSprite],
    ) -> Result<(), RenderError> {
        self.render_with_hud_lit(mesh, atlas, camera, hud, sprites, &[])
    }

    /// [`Self::render_with_hud`] with one 1.16.5 light value per mesh vertex.
    pub fn render_with_hud_lit(
        &mut self,
        mesh: &Mesh,
        atlas: &TextureAtlas,
        camera: Camera,
        hud: &[HudVertex],
        sprites: &[HudSprite],
        light: &[f32],
    ) -> Result<(), RenderError> {
        self.render_with_hud_gui(mesh, atlas, camera, hud, sprites, light, None, &[])
    }

    /// [`Self::render_with_hud_lit`] plus a third sprite list sampled from the
    /// vanilla GUI atlas (`gui::GuiSprites`): hotbar, selection frame,
    /// crosshair and the Bedrock touch buttons.
    #[allow(clippy::too_many_arguments)]
    pub fn render_with_hud_gui(
        &mut self,
        mesh: &Mesh,
        atlas: &TextureAtlas,
        camera: Camera,
        hud: &[HudVertex],
        sprites: &[HudSprite],
        light: &[f32],
        gui: Option<(&crate::gui::GuiSprites, &[HudSprite])>,
        hand: &[SpriteVertex],
    ) -> Result<(), RenderError> {
        let prepared = prepare_mesh(mesh, atlas, light)?;
        // The swapchain is not authoritative: Android can deliver the first
        // `Resized` late or not at all, and may hand back a frame a different
        // size than the surface was configured with. The acquired frame is
        // exactly what the compositor will show, so its own extent drives the
        // depth buffer, the camera aspect and the render passes.
        let frame = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(frame)
            | wgpu::CurrentSurfaceTexture::Suboptimal(frame) => frame,
            wgpu::CurrentSurfaceTexture::Timeout | wgpu::CurrentSurfaceTexture::Occluded => {
                return Ok(());
            }
            wgpu::CurrentSurfaceTexture::Outdated => {
                self.configure();
                return Ok(());
            }
            wgpu::CurrentSurfaceTexture::Lost => {
                return Err(RenderError::Failed(
                    "the window surface was lost and needs a new renderer".into(),
                ));
            }
            wgpu::CurrentSurfaceTexture::Validation => {
                return Err(RenderError::Failed(
                    "the surface rejected the swapchain configuration".into(),
                ));
            }
        };
        let extent = fit_extent(self.size, [frame.texture.width(), frame.texture.height()]);
        // The depth attachment must match the colour attachment, so it
        // follows the frame whenever the two disagree with the swapchain.
        let stale_swapchain = extent != self.size;
        if stale_swapchain {
            self.size = extent;
            self.depth = create_depth(&self.device, extent[0], extent[1]);
        }
        let view_proj = view_projection(camera, aspect_of(extent), mesh);
        let uniform = CameraUniform {
            view_proj,
            eye: [camera.eye[0], camera.eye[1], camera.eye[2], 1.0],
        };
        let uniform_buffer = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("horizon-1-16-5 camera"),
                contents: bytemuck::bytes_of(&uniform),
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            });
        let vertex_buffer = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("horizon-1-16-5 vertices"),
                contents: bytemuck::cast_slice(&prepared.vertices),
                usage: wgpu::BufferUsages::VERTEX,
            });
        let opaque_indices = index_buffer(
            &self.device,
            "horizon-1-16-5 opaque indices",
            &prepared.opaque_indices,
        );
        let translucent_indices = index_buffer(
            &self.device,
            "horizon-1-16-5 translucent indices",
            &prepared.translucent_indices,
        );

        let atlas_texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("horizon-1-16-5 atlas"),
            size: wgpu::Extent3d {
                width: atlas.width(),
                height: atlas.height(),
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        self.queue.write_texture(
            atlas_texture.as_image_copy(),
            atlas.rgba(),
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(atlas.width().max(1) * 4),
                rows_per_image: Some(atlas.height().max(1)),
            },
            wgpu::Extent3d {
                width: atlas.width(),
                height: atlas.height(),
                depth_or_array_layers: 1,
            },
        );
        let atlas_view = atlas_texture.create_view(&wgpu::TextureViewDescriptor::default());
        let sampler = self.device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("horizon-1-16-5 atlas sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            mipmap_filter: wgpu::MipmapFilterMode::Nearest,
            ..Default::default()
        });
        // The two world pipelines get their own auto-generated bind group
        // layouts, so the same camera/texture/sampler resources are bound
        // once per pipeline.
        let create_bind_group = |label: &str, layout: &wgpu::BindGroupLayout| {
            self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some(label),
                layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: uniform_buffer.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(&atlas_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: wgpu::BindingResource::Sampler(&sampler),
                    },
                ],
            })
        };
        let opaque_bind_group = create_bind_group(
            "horizon-1-16-5 opaque bind group",
            &self.pipeline.get_bind_group_layout(0),
        );
        let translucent_bind_group = create_bind_group(
            "horizon-1-16-5 translucent bind group",
            &self.translucent_pipeline.get_bind_group_layout(0),
        );

        let view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let view_of_frame = view.clone();
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("horizon-1-16-5 surface"),
            });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("horizon-1-16-5 surface pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(sky_clear()),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &self.depth,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(1.0),
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            if let Some(indices) = &opaque_indices {
                pass.set_pipeline(&self.pipeline);
                pass.set_bind_group(0, &opaque_bind_group, &[]);
                pass.set_vertex_buffer(0, vertex_buffer.slice(..));
                pass.set_index_buffer(indices.slice(..), wgpu::IndexFormat::Uint32);
                pass.draw_indexed(0..prepared.opaque_indices.len() as u32, 0, 0..1);
            }
        }
        if let Some(indices) = &translucent_indices {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("horizon-1-16-5 surface translucent pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &self.depth,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&self.translucent_pipeline);
            pass.set_bind_group(0, &translucent_bind_group, &[]);
            pass.set_vertex_buffer(0, vertex_buffer.slice(..));
            pass.set_index_buffer(indices.slice(..), wgpu::IndexFormat::Uint32);
            pass.draw_indexed(0..prepared.translucent_indices.len() as u32, 0, 0..1);
        }
        if !hud.is_empty() {
            let hud_buffer = self
                .device
                .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("horizon-1-16-5 hud"),
                    contents: bytemuck::cast_slice(hud),
                    usage: wgpu::BufferUsages::VERTEX,
                });
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("horizon-1-16-5 hud pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&self.hud_pipeline);
            pass.set_vertex_buffer(0, hud_buffer.slice(..));
            pass.draw(0..hud.len() as u32, 0..1);
        }
        if !sprites.is_empty() {
            let vertices = sprite_vertices(sprites);
            let sprite_buffer = self
                .device
                .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("horizon-1-16-5 hud sprites"),
                    contents: bytemuck::cast_slice(&vertices),
                    usage: wgpu::BufferUsages::VERTEX,
                });
            // The sprite pass binds the world atlas and its nearest/clamp
            // sampler, so hotbar icons sample the real block and item sprites
            // with the same filtering as the world.
            let sprite_bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("horizon-1-16-5 hud sprite bind group"),
                layout: &self.hud_sprite_pipeline.get_bind_group_layout(0),
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(&atlas_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::Sampler(&sampler),
                    },
                ],
            });
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("horizon-1-16-5 hud sprite pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&self.hud_sprite_pipeline);
            pass.set_bind_group(0, &sprite_bind_group, &[]);
            pass.set_vertex_buffer(0, sprite_buffer.slice(..));
            pass.draw(0..vertices.len() as u32, 0..1);
        }
        if let Some((gui, gui_sprites)) = gui.filter(|(_, sprites)| !sprites.is_empty()) {
            let texture = self.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("horizon-1-16-5 gui atlas"),
                size: wgpu::Extent3d {
                    width: gui.width(),
                    height: gui.height(),
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8UnormSrgb,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            });
            self.queue.write_texture(
                texture.as_image_copy(),
                gui.rgba(),
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(gui.width() * 4),
                    rows_per_image: Some(gui.height()),
                },
                wgpu::Extent3d {
                    width: gui.width(),
                    height: gui.height(),
                    depth_or_array_layers: 1,
                },
            );
            let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
            let sampler = self.device.create_sampler(&wgpu::SamplerDescriptor {
                label: Some("horizon-1-16-5 gui sampler"),
                address_mode_u: wgpu::AddressMode::ClampToEdge,
                address_mode_v: wgpu::AddressMode::ClampToEdge,
                address_mode_w: wgpu::AddressMode::ClampToEdge,
                mag_filter: wgpu::FilterMode::Nearest,
                min_filter: wgpu::FilterMode::Nearest,
                mipmap_filter: wgpu::MipmapFilterMode::Nearest,
                ..Default::default()
            });
            let vertices = sprite_vertex_list(gui_sprites, hand);
            let buffer = self
                .device
                .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("horizon-1-16-5 gui sprites"),
                    contents: bytemuck::cast_slice(&vertices),
                    usage: wgpu::BufferUsages::VERTEX,
                });
            let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("horizon-1-16-5 gui bind group"),
                layout: &self.hud_sprite_pipeline.get_bind_group_layout(0),
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(&view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::Sampler(&sampler),
                    },
                ],
            });
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("horizon-1-16-5 gui pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view_of_frame,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&self.hud_sprite_pipeline);
            pass.set_bind_group(0, &bind_group, &[]);
            pass.set_vertex_buffer(0, buffer.slice(..));
            pass.draw(0..vertices.len() as u32, 0..1);
        }
        self.queue.submit([encoder.finish()]);
        self.queue.present(frame);
        // `Surface::configure` panics while a `SurfaceTexture` is still
        // alive, so the swapchain only follows the frame after it is shown;
        // the next acquire then returns the frame's own extent.
        if stale_swapchain {
            self.configure();
        }
        Ok(())
    }

    /// Applies the current size and format to the swapchain.
    fn configure(&self) {
        self.surface.configure(
            &self.device,
            &wgpu::SurfaceConfiguration {
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                format: self.format,
                color_space: wgpu::SurfaceColorSpace::Auto,
                width: self.size[0],
                height: self.size[1],
                present_mode: wgpu::PresentMode::Fifo,
                desired_maximum_frame_latency: 2,
                alpha_mode: wgpu::CompositeAlphaMode::Auto,
                view_formats: vec![],
            },
        );
    }
}

fn padded_row(width: u32) -> u32 {
    (width * 4).next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT)
}

fn view_projection(camera: Camera, aspect: f32, mesh: &Mesh) -> [f32; 16] {
    let (_, radius) = bounds(mesh);
    let direction = [
        camera.target[0] - camera.eye[0],
        camera.target[1] - camera.eye[1],
        camera.target[2] - camera.eye[2],
    ];
    let distance =
        (direction[0] * direction[0] + direction[1] * direction[1] + direction[2] * direction[2])
            .sqrt()
            .max(0.01);
    let near = (distance * 0.01).max(0.01);
    let far = distance * 10.0 + radius * 8.0 + 64.0;
    let projection = perspective(camera.fov_y_degrees.to_radians(), aspect, near, far);
    let view = look_at(camera.eye, camera.target, camera.up);
    multiply(projection, view)
}

/// A right-handed perspective matrix for wgpu's 0..1 depth range, in
/// column-major order.
fn perspective(fov_y: f32, aspect: f32, near: f32, far: f32) -> [f32; 16] {
    let f = 1.0 / (fov_y * 0.5).tan();
    let range = near - far;
    [
        f / aspect,
        0.0,
        0.0,
        0.0,
        0.0,
        f,
        0.0,
        0.0,
        0.0,
        0.0,
        far / range,
        -1.0,
        0.0,
        0.0,
        near * far / range,
        0.0,
    ]
}

/// A right-handed look-at view matrix, column-major.
fn look_at(eye: [f32; 3], target: [f32; 3], up: [f32; 3]) -> [f32; 16] {
    let forward = normalize(sub(target, eye));
    let side = normalize(cross(forward, up));
    let up = cross(side, forward);
    [
        side[0],
        up[0],
        -forward[0],
        0.0,
        side[1],
        up[1],
        -forward[1],
        0.0,
        side[2],
        up[2],
        -forward[2],
        0.0,
        -dot(side, eye),
        -dot(up, eye),
        dot(forward, eye),
        1.0,
    ]
}

/// `a * b` for column-major 4x4 matrices.
fn multiply(a: [f32; 16], b: [f32; 16]) -> [f32; 16] {
    let mut out = [0.0; 16];
    for column in 0..4 {
        for row in 0..4 {
            let mut sum = 0.0;
            for k in 0..4 {
                sum += a[k * 4 + row] * b[column * 4 + k];
            }
            out[column * 4 + row] = sum;
        }
    }
    out
}

fn sub(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

fn normalize(vector: [f32; 3]) -> [f32; 3] {
    let length = dot(vector, vector).sqrt();
    if length > 1e-6 {
        [vector[0] / length, vector[1] / length, vector[2] / length]
    } else {
        [0.0, 1.0, 0.0]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::atlas::TextureAtlas;
    use crate::{BakedModel, BlockSource, BlockState, Face, ModelSource, Quad, Vertex};
    use std::sync::atomic::{AtomicUsize, Ordering};

    static NEXT_DIR: AtomicUsize = AtomicUsize::new(0);

    fn temp_root() -> std::path::PathBuf {
        let id = NEXT_DIR.fetch_add(1, Ordering::Relaxed);
        let root =
            std::env::temp_dir().join(format!("horizon-1165-render-{}-{id}", std::process::id()));
        std::fs::create_dir_all(root.join("minecraft/textures/block")).unwrap();
        root
    }

    /// Writes an 8x8 solid RGBA sprite into the pack at `root`.
    fn write_sprite(root: &std::path::Path, name: &str, colour: [u8; 4]) {
        let file = std::fs::File::create(root.join("minecraft/textures/block").join(name)).unwrap();
        let mut encoder = png::Encoder::new(file, 8, 8);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().unwrap();
        writer.write_image_data(&colour.repeat(64)).unwrap();
        writer.finish().unwrap();
    }

    fn temp_pack(red: [u8; 4], blue: [u8; 4]) -> std::path::PathBuf {
        let root = temp_root();
        write_sprite(&root, "red.png", red);
        write_sprite(&root, "blue.png", blue);
        root
    }

    /// A cube whose top face is red and every other face blue.
    struct Cube;
    impl ModelSource for Cube {
        fn model(&self, _state: &BlockState) -> Option<BakedModel> {
            let faces: [(Face, [[f32; 3]; 4]); 6] = [
                (
                    Face::Down,
                    [
                        [0.0, 0.0, 16.0],
                        [0.0, 0.0, 0.0],
                        [16.0, 0.0, 0.0],
                        [16.0, 0.0, 16.0],
                    ],
                ),
                (
                    Face::Up,
                    [
                        [16.0, 16.0, 0.0],
                        [0.0, 16.0, 0.0],
                        [0.0, 16.0, 16.0],
                        [16.0, 16.0, 16.0],
                    ],
                ),
                (
                    Face::North,
                    [
                        [16.0, 0.0, 0.0],
                        [0.0, 0.0, 0.0],
                        [0.0, 16.0, 0.0],
                        [16.0, 16.0, 0.0],
                    ],
                ),
                (
                    Face::South,
                    [
                        [0.0, 0.0, 16.0],
                        [16.0, 0.0, 16.0],
                        [16.0, 16.0, 16.0],
                        [0.0, 16.0, 16.0],
                    ],
                ),
                (
                    Face::West,
                    [
                        [0.0, 0.0, 0.0],
                        [0.0, 0.0, 16.0],
                        [0.0, 16.0, 16.0],
                        [0.0, 16.0, 0.0],
                    ],
                ),
                (
                    Face::East,
                    [
                        [16.0, 0.0, 16.0],
                        [16.0, 0.0, 0.0],
                        [16.0, 16.0, 0.0],
                        [16.0, 16.0, 16.0],
                    ],
                ),
            ];
            Some(BakedModel {
                quads: faces
                    .into_iter()
                    .map(|(face, corners)| Quad {
                        vertices: corners.map(|position| Vertex {
                            position,
                            uv: [position[0], position[1]],
                            normal: face.normal(),
                        }),
                        texture: if face == Face::Up {
                            "minecraft:block/red".to_owned()
                        } else {
                            "minecraft:block/blue".to_owned()
                        },
                        cullface: None,
                    })
                    .collect(),
            })
        }
    }

    struct SingleCube;
    impl BlockSource for SingleCube {
        fn state_at(&self, x: i32, y: i32, z: i32) -> Option<BlockState> {
            ([x, y, z] == [0, 0, 0]).then(|| BlockState::new("minecraft:stone"))
        }
    }

    fn cube_mesh() -> Mesh {
        crate::mesh::mesh_box(&SingleCube, &Cube, [0, 0, 0], [1, 1, 1])
    }

    #[test]
    fn offscreen_render_writes_a_png_with_texture_colours() {
        let root = temp_pack([255, 0, 0, 255], [0, 0, 255, 255]);
        let mesh = cube_mesh();
        let atlas = TextureAtlas::from_directory(
            &root,
            [
                "minecraft:block/red".to_owned(),
                "minecraft:block/blue".to_owned(),
            ],
        )
        .unwrap();
        let renderer = match OffscreenRenderer::new() {
            Ok(renderer) => renderer,
            Err(RenderError::NoAdapter(error)) => {
                eprintln!("skipping the offscreen render test: {error}");
                std::fs::remove_dir_all(&root).unwrap();
                return;
            }
            Err(error) => panic!("offscreen renderer: {error}"),
        };
        let camera = Camera::framing(&mesh, std::f32::consts::FRAC_PI_4, 0.55, 2.5);
        let frame = renderer.render(&mesh, &atlas, camera, 128, 128).unwrap();
        assert_eq!((frame.width, frame.height), (128, 128));
        assert_eq!(frame.rgba.len(), 128 * 128 * 4);

        // The top face is red, the sides blue; the clear colour is neither.
        let red = frame
            .rgba
            .chunks_exact(4)
            .filter(|p| p[0] > 160 && p[1] < 80 && p[2] < 80)
            .count();
        let blue = frame
            .rgba
            .chunks_exact(4)
            .filter(|p| p[2] > 160 && p[0] < 80 && p[1] < 80)
            .count();
        assert!(red > 200, "expected red top pixels, got {red}");
        assert!(blue > 200, "expected blue side pixels, got {blue}");
        // The corner keeps the sky clear colour when the cube does not cover it.
        assert!(
            frame
                .pixel(0, 0)
                .is_some_and(|p| p[2] > p[0] && p[3] == 255),
            "expected sky at the corner: {:?}",
            frame.pixel(0, 0)
        );

        let out = root.join("frame.png");
        frame.write_png(&out).unwrap();
        let bytes = std::fs::read(&out).unwrap();
        assert_eq!(&bytes[..8], b"\x89PNG\r\n\x1a\n");
        let decoder = png::Decoder::new(std::io::Cursor::new(bytes));
        let mut reader = decoder.read_info().unwrap();
        let mut pixels = vec![0; reader.output_buffer_size().unwrap()];
        let info = reader.next_frame(&mut pixels).unwrap();
        assert_eq!((info.width, info.height), (128, 128));
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn camera_changes_the_frame() {
        let root = temp_pack([255, 0, 0, 255], [0, 0, 255, 255]);
        let mesh = cube_mesh();
        let atlas = TextureAtlas::from_directory(
            &root,
            [
                "minecraft:block/red".to_owned(),
                "minecraft:block/blue".to_owned(),
            ],
        )
        .unwrap();
        let renderer = match OffscreenRenderer::new() {
            Ok(renderer) => renderer,
            Err(RenderError::NoAdapter(_)) => {
                std::fs::remove_dir_all(&root).unwrap();
                return;
            }
            Err(error) => panic!("offscreen renderer: {error}"),
        };
        let front = renderer
            .render(&mesh, &atlas, Camera::framing(&mesh, 0.0, 0.0, 2.5), 64, 64)
            .unwrap();
        let top = renderer
            .render(&mesh, &atlas, Camera::framing(&mesh, 0.0, 1.4, 2.5), 64, 64)
            .unwrap();
        assert_ne!(front.rgba, top.rgba);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn texture_tints_match_the_plains_biome() {
        fn close(actual: [f32; 3], expected: [f32; 3]) {
            for (actual, expected) in actual.into_iter().zip(expected) {
                assert!(
                    (actual - expected).abs() < 5e-4,
                    "expected {expected}, got {actual} (whole value {actual:?} vs {expected:?})"
                );
            }
        }

        // Plains grass colour 0x91BD59 and foliage colour 0x59AE30, decoded
        // from sRGB to linear RGB.
        let grass = [0.2831, 0.5088, 0.0999];
        let foliage = [0.0999, 0.4232, 0.0296];
        for name in [
            "minecraft:block/grass_block_top",
            "minecraft:block/grass_block_side_overlay",
            "minecraft:block/grass",
            "minecraft:block/fern",
            "minecraft:block/tall_grass",
            "minecraft:block/large_fern",
            "minecraft:block/sugar_cane",
            "grass",
            "block/tall_grass",
        ] {
            close(tint_for_texture(name), grass);
        }
        for name in [
            "minecraft:block/oak_leaves",
            "minecraft:block/dark_oak_leaves",
            "minecraft:block/jungle_leaves",
            "minecraft:block/vine",
            "minecraft:block/lily_pad",
            "oak_leaves",
        ] {
            close(tint_for_texture(name), foliage);
        }
        for name in [
            "minecraft:block/stone",
            "minecraft:block/oak_planks",
            "minecraft:block/red",
            "minecraft:entity/zombie",
            "minecraft:item/stick",
        ] {
            assert_eq!(tint_for_texture(name), [1.0, 1.0, 1.0], "{name}");
        }
        assert_ne!(grass, foliage);
    }

    #[test]
    fn gpu_vertices_carry_a_tint() {
        let root = temp_pack([255, 0, 0, 255], [0, 0, 255, 255]);
        let mesh = Mesh {
            vertices: vec![
                Vertex {
                    position: [0.0, 0.0, 0.0],
                    uv: [0.0, 0.0],
                    normal: [0.0, 1.0, 0.0],
                };
                4
            ],
            indices: vec![0, 1, 2, 0, 2, 3],
            textures: vec!["minecraft:block/grass_block_top".to_owned()],
        };
        let atlas = TextureAtlas::from_directory(
            &root,
            [
                "minecraft:block/red".to_owned(),
                "minecraft:block/blue".to_owned(),
            ],
        )
        .unwrap();
        let vertices = build_vertices(&mesh, &atlas, &[]).unwrap();
        assert_eq!(vertices.len(), 4);
        for vertex in &vertices {
            assert_eq!(
                vertex.tint,
                tint_for_texture("minecraft:block/grass_block_top")
            );
        }
        // position 12 + uv 8 + normal 12 + tint 12 + light 4.
        assert_eq!(std::mem::size_of::<GpuVertex>(), 48);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn rejects_malformed_meshes() {
        let root = temp_pack([255, 0, 0, 255], [0, 0, 255, 255]);
        let atlas =
            TextureAtlas::from_directory(&root, ["minecraft:block/red".to_owned()]).unwrap();
        let renderer = match OffscreenRenderer::new() {
            Ok(renderer) => renderer,
            Err(RenderError::NoAdapter(_)) => {
                std::fs::remove_dir_all(&root).unwrap();
                return;
            }
            Err(error) => panic!("offscreen renderer: {error}"),
        };
        // An empty mesh is a valid frame (a network world starts empty); a
        // mesh whose textures and quads disagree is not.
        let empty = Mesh::default();
        let camera = Camera::looking_at([3.0, 3.0, 3.0], [0.0, 0.0, 0.0]);
        assert!(renderer.render(&empty, &atlas, camera, 64, 64).is_ok());
        let malformed = Mesh {
            vertices: vec![crate::Vertex::default(); 4],
            indices: vec![0, 1, 2, 0, 2, 3],
            textures: Vec::new(),
        };
        assert!(matches!(
            renderer.render(&malformed, &atlas, camera, 64, 64),
            Err(RenderError::Failed(_))
        ));
        std::fs::remove_dir_all(&root).unwrap();
    }

    /// Builds `textures.len()` unit quads side by side, two triangles each, so
    /// quad `q` owns vertices `4q..4q+4` and indices `6q..6q+6`.
    fn quad_mesh(textures: &[&str]) -> Mesh {
        let mut vertices = Vec::new();
        for quad in 0..textures.len() {
            let x = quad as f32;
            for corner in [[x, 0.0], [x + 1.0, 0.0], [x + 1.0, 1.0], [x, 1.0]] {
                vertices.push(Vertex {
                    position: [corner[0], corner[1], 0.0],
                    uv: [0.0, 0.0],
                    normal: [0.0, 0.0, 1.0],
                });
            }
        }
        let mut indices = Vec::new();
        for quad in 0..textures.len() as u32 {
            let base = quad * 4;
            indices.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
        }
        Mesh {
            vertices,
            indices,
            textures: textures.iter().map(|name| (*name).to_owned()).collect(),
        }
    }

    #[test]
    fn translucent_textures_are_classified_by_sprite() {
        for name in [
            "minecraft:block/glass",
            "minecraft:block/white_stained_glass",
            "minecraft:block/glass_pane_top",
            "glass",
            "block/glass_pane_top",
            "minecraft:block/water_still",
            "minecraft:block/water_flow",
            "minecraft:block/ice",
            "minecraft:block/blue_ice",
            "minecraft:block/packed_ice",
            "minecraft:block/frosted_ice_0",
            "minecraft:block/slime_block",
            "minecraft:block/honey_block_top",
        ] {
            assert!(is_translucent_texture(name), "{name}");
        }
        for name in [
            "minecraft:block/stone",
            "minecraft:block/oak_planks",
            "minecraft:block/diorite",
            "minecraft:block/sandstone",
            "minecraft:block/grass_block_top",
            "minecraft:block/red",
            "minecraft:missingno",
            "minecraft:item/stick",
        ] {
            assert!(!is_translucent_texture(name), "{name}");
        }
    }

    #[test]
    fn mixed_mesh_splits_into_opaque_and_translucent_quads() {
        let mesh = quad_mesh(&[
            "minecraft:block/stone",
            "minecraft:block/glass",
            "minecraft:block/oak_planks",
            "minecraft:block/water_still",
            "minecraft:block/blue_ice",
            "minecraft:block/slime_block",
        ]);
        let (opaque, translucent) = split_indices(&mesh).unwrap();
        assert_eq!(opaque.len() / 6, 2, "opaque quads");
        assert_eq!(translucent.len() / 6, 4, "translucent quads");
        assert_eq!(opaque.len() + translucent.len(), mesh.indices.len());
        assert_eq!(opaque, vec![0, 1, 2, 0, 2, 3, 8, 9, 10, 8, 10, 11]);
        assert_eq!(
            translucent,
            vec![
                4, 5, 6, 4, 6, 7, // glass
                12, 13, 14, 12, 14, 15, // water
                16, 17, 18, 16, 18, 19, // blue ice
                20, 21, 22, 20, 22, 23, // slime
            ]
        );
    }

    #[test]
    fn all_glass_mesh_draws_only_the_second_pass() {
        let mesh = quad_mesh(&["minecraft:block/glass", "minecraft:block/glass_pane_top"]);
        let (opaque, translucent) = split_indices(&mesh).unwrap();
        assert!(opaque.is_empty(), "glass must not enter the opaque pass");
        assert_eq!(translucent, mesh.indices);
    }

    #[test]
    fn triangles_without_a_texture_are_rejected() {
        // Two triangles belong to one quad, so a third triangle would name a
        // second texture the mesh does not have.
        let mut mesh = quad_mesh(&["minecraft:block/stone"]);
        mesh.indices.extend_from_slice(&[4, 5, 6]);
        assert!(split_indices(&mesh).is_err());
    }

    #[test]
    fn translucent_quads_blend_over_opaque_geometry() {
        let root = temp_pack([255, 0, 0, 255], [0, 0, 255, 128]);
        let mut vertices = Vec::new();
        let mut indices = Vec::new();
        let mut textures = Vec::new();
        // An opaque red quad at z = 0 with a half-transparent blue quad in
        // front of it. Every quad is emitted in both windings so back-face
        // culling keeps exactly one copy, whichever winding this viewport
        // treats as front-facing.
        for (texture, z) in [
            ("minecraft:block/red", 0.0f32),
            ("minecraft:block/blue", 1.0),
        ] {
            for reversed in [false, true] {
                let base = vertices.len() as u32;
                let mut corners = [
                    ([0.0, 0.0], [0.0, 0.0]),
                    ([1.0, 0.0], [16.0, 0.0]),
                    ([1.0, 1.0], [16.0, 16.0]),
                    ([0.0, 1.0], [0.0, 16.0]),
                ];
                if reversed {
                    corners.swap(1, 3);
                }
                for (corner, uv) in corners {
                    vertices.push(Vertex {
                        position: [corner[0], corner[1], z],
                        uv,
                        normal: [0.0, 0.0, 1.0],
                    });
                }
                indices.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
                textures.push(texture.to_owned());
            }
        }
        let mesh = Mesh {
            vertices,
            indices,
            textures,
        };
        let atlas = TextureAtlas::from_directory(
            &root,
            [
                "minecraft:block/red".to_owned(),
                "minecraft:block/blue".to_owned(),
            ],
        )
        .unwrap();
        let renderer = match OffscreenRenderer::new() {
            Ok(renderer) => renderer,
            Err(RenderError::NoAdapter(_)) => {
                std::fs::remove_dir_all(&root).unwrap();
                return;
            }
            Err(error) => panic!("offscreen renderer: {error}"),
        };
        let camera = Camera::looking_at([0.5, 0.5, 3.0], [0.5, 0.5, 0.5]);
        let frame = renderer.render(&mesh, &atlas, camera, 64, 64).unwrap();
        let centre = frame.pixel(32, 32).expect("centre pixel");
        // The test sprites are fully opaque, so the blended pass must produce
        // the front (blue) quad unchanged here; the blend-over behavior comes
        // from glass textures in the real atlas, whose alpha the shader now
        // keeps instead of cutting out.
        assert!(
            centre[0] < 60 && centre[2] > 90,
            "expected the front quad, got {centre:?}"
        );
        assert!(centre[1] < 60, "expected no green, got {centre:?}");
        assert_eq!(centre[3], 255);
        std::fs::remove_dir_all(&root).unwrap();
    }

    fn clip(x: f32, y: f32) -> [f32; 2] {
        [x * 2.0 - 1.0, 1.0 - y * 2.0]
    }

    /// Twice the signed area of a triangle in clip space.
    fn signed_area(a: [f32; 2], b: [f32; 2], c: [f32; 2]) -> f32 {
        (b[0] - a[0]) * (c[1] - a[1]) - (b[1] - a[1]) * (c[0] - a[0])
    }

    #[test]
    fn hud_rect_converts_normalized_coordinates_to_clip_space() {
        let color = [0.25, 0.5, 0.75, 0.5];
        let [
            top_left,
            top_right,
            bottom_right,
            top_left_2,
            bottom_right_2,
            bottom_left,
        ] = hud_rect(0.25, 0.5, 0.5, 0.25, color);

        assert_eq!(top_left.position, clip(0.25, 0.5));
        assert_eq!(top_right.position, clip(0.75, 0.5));
        assert_eq!(bottom_right.position, clip(0.75, 0.75));
        assert_eq!(top_left_2.position, clip(0.25, 0.5));
        assert_eq!(bottom_right_2.position, clip(0.75, 0.75));
        assert_eq!(bottom_left.position, clip(0.25, 0.75));

        assert_eq!(top_left.position, [-0.5, 0.0]);
        assert_eq!(top_right.position, [0.5, 0.0]);
        assert_eq!(bottom_right.position, [0.5, -0.5]);
        assert_eq!(bottom_left.position, [-0.5, -0.5]);

        for vertex in [
            top_left,
            top_right,
            bottom_right,
            top_left_2,
            bottom_right_2,
            bottom_left,
        ] {
            assert_eq!(vertex.color, color);
        }
    }

    #[test]
    fn hud_rect_emits_two_ordered_triangles_in_the_same_winding() {
        let [a, b, c, d, e, f] = hud_rect(0.0, 0.0, 1.0, 1.0, [1.0, 0.0, 0.0, 1.0]);

        // First triangle: top-left, top-right, bottom-right.
        assert_eq!(a.position, [-1.0, 1.0]);
        assert_eq!(b.position, [1.0, 1.0]);
        assert_eq!(c.position, [1.0, -1.0]);
        // Second triangle reuses the diagonal: top-left, bottom-right, bottom-left.
        assert_eq!(d.position, a.position);
        assert_eq!(e.position, c.position);
        assert_eq!(f.position, [-1.0, -1.0]);

        let first = signed_area(a.position, b.position, c.position);
        let second = signed_area(d.position, e.position, f.position);
        assert_ne!(first, 0.0);
        assert_ne!(second, 0.0);
        assert_eq!(first.signum(), second.signum());
    }

    #[test]
    fn hud_vertices_are_packed_for_the_gpu_layout() {
        assert_eq!(std::mem::size_of::<HudVertex>(), 24);
        assert_eq!(std::mem::align_of::<HudVertex>(), 4);
    }

    #[test]
    fn fit_extent_prefers_the_acquired_frame() {
        // A stale 1280x720 swapchain inside a 2400x1080 surface must draw
        // into the frame's own extent, never the smaller configured one.
        assert_eq!(fit_extent([1280, 720], [2400, 1080]), [2400, 1080]);
        assert_eq!(fit_extent([2400, 1080], [2400, 1080]), [2400, 1080]);
        // A frame larger than the configured surface is still what the
        // compositor shows, so it wins too.
        assert_eq!(fit_extent([1920, 1080], [2400, 1080]), [2400, 1080]);
        // Only an impossible zero-sized frame falls back to the config.
        assert_eq!(fit_extent([1280, 720], [0, 0]), [1280, 720]);
        assert_eq!(fit_extent([1280, 720], [2400, 0]), [1280, 720]);
    }

    #[test]
    fn the_camera_aspect_follows_the_frame_extent() {
        // 2400x1080 is 20:9; projecting with a stale 1280x720 (16:9) would
        // stretch the world.
        assert_eq!(aspect_of([2400, 1080]), 2400.0 / 1080.0);
        assert_eq!(aspect_of([1280, 720]), 1280.0 / 720.0);
        assert_eq!(aspect_of([0, 0]), 1.0);

        // `perspective` stores `f / aspect` first, so the projection widens
        // exactly as much as the frame does.
        let fov = 70.0_f32.to_radians();
        let f = 1.0 / (fov * 0.5).tan();
        let narrow = perspective(fov, aspect_of([1280, 720]), 0.1, 100.0);
        let wide = perspective(fov, aspect_of([2400, 1080]), 0.1, 100.0);
        assert_eq!(narrow[0], f / (1280.0 / 720.0));
        assert_eq!(wide[0], f / (2400.0 / 1080.0));
        assert!(wide[0] < narrow[0]);
    }

    #[test]
    fn initial_extent_takes_physical_pixels_and_clamps() {
        // `inner_size` is already physical on every backend, Android
        // included, so it is used unchanged.
        assert_eq!(initial_extent([2400, 1080], 8192), [2400, 1080]);
        // A window that has not been laid out yet becomes 1x1, the smallest
        // extent wgpu can configure.
        assert_eq!(initial_extent([0, 0], 8192), [1, 1]);
        // Past the device limit is clamped to what the device can create.
        assert_eq!(initial_extent([9000, 1080], 8192), [8192, 1080]);
    }

    #[test]
    fn hud_sprite_maps_rect_and_uv_corners_to_clip_space() {
        let color = [0.25, 0.5, 0.75, 1.0];
        let uv = [0.25, 0.5, 0.75, 1.0];
        let [
            top_left,
            top_right,
            bottom_right,
            top_left_2,
            bottom_right_2,
            bottom_left,
        ] = hud_sprite([0.1, 0.2, 0.4, 0.5], uv, color);

        assert_eq!(top_left.position, clip(0.1, 0.2));
        assert_eq!(top_right.position, clip(0.5, 0.2));
        assert_eq!(bottom_right.position, clip(0.5, 0.7));
        assert_eq!(top_left_2.position, top_left.position);
        assert_eq!(bottom_right_2.position, bottom_right.position);
        assert_eq!(bottom_left.position, clip(0.1, 0.7));

        // The sprite's own corners, in the same order as `hud_rect`.
        assert_eq!(top_left.uv, [0.25, 0.5]);
        assert_eq!(top_right.uv, [0.75, 0.5]);
        assert_eq!(bottom_right.uv, [0.75, 1.0]);
        assert_eq!(bottom_left.uv, [0.25, 1.0]);

        for vertex in [
            top_left,
            top_right,
            bottom_right,
            top_left_2,
            bottom_right_2,
            bottom_left,
        ] {
            assert_eq!(vertex.color, color);
        }
    }

    #[test]
    fn sprite_vertices_flatten_every_sprite_in_order() {
        // A frame without sprites has no sprite geometry at all.
        assert!(sprite_vertices(&[]).is_empty());

        let first = HudSprite::new([0.0, 0.0, 0.5, 0.5], [0.0, 0.0, 0.5, 0.5]);
        let second = HudSprite {
            rect: [0.5, 0.5, 0.5, 0.5],
            uv: [0.5, 0.5, 1.0, 1.0],
            color: [1.0, 0.5, 0.25, 0.75],
        };
        let vertices = sprite_vertices(&[first, second]);
        assert_eq!(vertices.len(), 12);
        assert_eq!(vertices[..6], hud_sprite(first.rect, first.uv, first.color));
        assert_eq!(
            vertices[6..],
            hud_sprite(second.rect, second.uv, second.color)
        );
    }

    #[test]
    fn a_frame_without_sprites_keeps_the_flat_hud_stream() {
        // `render` forwards an empty sprite list to `render_with_hud`; with no
        // sprites the sprite pass is skipped, so the frame is exactly the flat
        // HUD geometry the pre-sprite renderer drew.
        let hud = hud_rect(0.25, 0.5, 0.5, 0.25, [1.0, 0.0, 0.0, 1.0]);
        assert_eq!(std::mem::size_of_val(&hud), 6 * 24);
        assert!(sprite_vertices(&[]).is_empty());
        assert_eq!(
            hud.iter().map(|vertex| vertex.position).collect::<Vec<_>>(),
            vec![
                clip(0.25, 0.5),
                clip(0.75, 0.5),
                clip(0.75, 0.75),
                clip(0.25, 0.5),
                clip(0.75, 0.75),
                clip(0.25, 0.75),
            ]
        );
    }

    #[test]
    fn sprite_vertex_is_packed_for_the_gpu_layout() {
        assert_eq!(std::mem::size_of::<SpriteVertex>(), 32);
        assert_eq!(std::mem::align_of::<SpriteVertex>(), 4);
    }

    #[test]
    fn hud_disc_emits_three_vertices_per_wedge_counter_clockwise() {
        let color = [1.0, 1.0, 1.0, 0.5];
        let center = [0.5, 0.5];
        let radius = [0.25, 0.25];
        let disc = hud_disc(center, radius, color, 16);

        assert_eq!(disc.len(), 16 * 3);
        // At least a triangle comes out, however small the request.
        assert_eq!(hud_disc(center, radius, color, 0).len(), 3 * 3);

        for triangle in disc.chunks_exact(3) {
            assert_eq!(triangle[0].position, clip(0.5, 0.5));
            // Counter-clockwise in clip space: a positive signed area for
            // every wedge of the fan.
            let area = signed_area(
                triangle[0].position,
                triangle[1].position,
                triangle[2].position,
            );
            assert!(area > 0.0, "wedge must be counter-clockwise, got {area}");
            for vertex in triangle {
                assert_eq!(vertex.color, color);
            }
        }
        // Every rim point sits on the circle, so the base reads round rather
        // than as the rectangle `hud_rect` would emit.
        for vertex in disc.chunks_exact(3).flat_map(|t| &t[1..]) {
            let dx = vertex.position[0];
            let dy = vertex.position[1];
            let distance = (dx * dx + dy * dy).sqrt();
            assert!((distance - 0.5).abs() < 1e-5, "rim at {distance}");
        }
    }
}

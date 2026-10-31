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
//! alpha blending on top of the world.

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

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct CameraUniform {
    view_proj: [f32; 16],
}

const SHADER: &str = r#"
struct Camera {
    view_proj: mat4x4<f32>,
};
@group(0) @binding(0) var<uniform> camera: Camera;
@group(0) @binding(1) var atlas_texture: texture_2d<f32>;
@group(0) @binding(2) var atlas_sampler: sampler;

struct VertexInput {
    @location(0) position: vec3<f32>,
    @location(1) uv: vec2<f32>,
    @location(2) normal: vec3<f32>,
    @location(3) tint: vec3<f32>,
};
struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) light: f32,
    @location(2) tint: vec3<f32>,
};

@vertex
fn vs_main(input: VertexInput) -> VertexOutput {
    var output: VertexOutput;
    output.clip_position = camera.view_proj * vec4<f32>(input.position, 1.0);
    output.uv = input.uv;
    output.tint = input.tint;
    let normal = normalize(input.normal);
    let sun = normalize(vec3<f32>(0.35, 1.0, 0.25));
    let diffuse = max(dot(normal, sun), 0.0);
    let sky = 0.5 + 0.5 * normal.y;
    output.light = 0.45 + 0.40 * diffuse + 0.15 * sky;
    return output;
}

@fragment
fn fs_main(input: VertexOutput) -> @location(0) vec4<f32> {
    let texel = textureSample(atlas_texture, atlas_sampler, input.uv);
    if (texel.a < 0.5) {
        discard;
    }
    return vec4<f32>(texel.rgb * input.tint * input.light, 1.0);
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
    return vec4<f32>(texel.rgb * input.tint * input.light, texel.a);
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
        if width == 0 || height == 0 {
            return Err(RenderError::Failed("frame size must be non-zero".into()));
        }
        let prepared = prepare_mesh(mesh, atlas)?;

        let view_proj = view_projection(camera, width as f32 / height as f32, mesh);
        let uniform = CameraUniform { view_proj };
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
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: 0.07,
                            g: 0.09,
                            b: 0.12,
                            a: 1.0,
                        }),
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
fn build_vertices(mesh: &Mesh, atlas: &TextureAtlas) -> Result<Vec<GpuVertex>, RenderError> {
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
    if mesh.indices.is_empty() {
        return Err(RenderError::Failed("mesh has no triangles".into()));
    }
    let missing_uv = atlas
        .uv(MISSING_TEXTURE)
        .ok_or_else(|| RenderError::Failed("atlas has no missing sprite".into()))?;

    let mut vertices = Vec::with_capacity(mesh.vertices.len());
    for (quad_index, quad) in mesh.vertices.chunks_exact(4).enumerate() {
        let uv_rect = atlas.uv(&mesh.textures[quad_index]).unwrap_or(missing_uv);
        let tint = tint_for_texture(&mesh.textures[quad_index]);
        for vertex in quad {
            let local = [vertex.uv[0] / 16.0, vertex.uv[1] / 16.0];
            vertices.push(GpuVertex {
                position: vertex.position,
                uv: [
                    uv_rect[0] + local[0] * (uv_rect[2] - uv_rect[0]),
                    uv_rect[1] + local[1] * (uv_rect[3] - uv_rect[1]),
                ],
                normal: vertex.normal,
                tint,
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
fn prepare_mesh(mesh: &Mesh, atlas: &TextureAtlas) -> Result<PreparedMesh, RenderError> {
    let vertices = build_vertices(mesh, atlas)?;
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
    format: wgpu::TextureFormat,
    size: [u32; 2],
    depth: wgpu::TextureView,
}

impl SurfaceRenderer {
    pub fn new(window: std::sync::Arc<winit::window::Window>) -> Result<Self, RenderError> {
        let inner = window.inner_size();
        let size = [inner.width.max(1), inner.height.max(1)];
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
        let size = [size[0].min(max), size[1].min(max)];
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
        let depth = create_depth(&device, size[0], size[1]);
        let renderer = Self {
            surface,
            device,
            queue,
            pipeline,
            translucent_pipeline,
            hud_pipeline,
            format,
            size,
            depth,
        };
        renderer.configure();
        Ok(renderer)
    }

    /// Follows the window to `width` x `height`. A zero dimension (a
    /// minimized window) or a dimension past the device's texture limit
    /// keeps the last swapchain, which `wgpu` cannot configure.
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
        self.render_with_hud(mesh, atlas, camera, &[])
    }

    /// Rasterizes `mesh` exactly like [`Self::render`], then draws `hud`
    /// clip-space triangles over it with alpha blending and no depth writes
    /// before presenting the combined frame. An empty slice draws nothing
    /// extra, so `render` keeps its original output.
    pub fn render_with_hud(
        &mut self,
        mesh: &Mesh,
        atlas: &TextureAtlas,
        camera: Camera,
        hud: &[HudVertex],
    ) -> Result<(), RenderError> {
        let prepared = prepare_mesh(mesh, atlas)?;
        let [width, height] = self.size;
        let view_proj = view_projection(camera, width as f32 / height as f32, mesh);
        let uniform = CameraUniform { view_proj };
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

        let view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
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
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: 0.07,
                            g: 0.09,
                            b: 0.12,
                            a: 1.0,
                        }),
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
        self.queue.submit([encoder.finish()]);
        self.queue.present(frame);
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
        assert!(
            frame
                .pixel(0, 0)
                .is_some_and(|p| p[0] < 100 && p[1] < 100 && p[2] < 100 && p[3] == 255)
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
        let vertices = build_vertices(&mesh, &atlas).unwrap();
        assert_eq!(vertices.len(), 4);
        for vertex in &vertices {
            assert_eq!(
                vertex.tint,
                tint_for_texture("minecraft:block/grass_block_top")
            );
        }
        assert_eq!(std::mem::size_of::<GpuVertex>(), 44);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn rejects_malformed_meshes() {
        let mesh = Mesh::default();
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
        let camera = Camera::looking_at([3.0, 3.0, 3.0], [0.0, 0.0, 0.0]);
        assert!(matches!(
            renderer.render(&mesh, &atlas, camera, 64, 64),
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
}

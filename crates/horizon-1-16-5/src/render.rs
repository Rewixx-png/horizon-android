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
//! fixed directional term, there are no shadows, shaders, sky or entities,
//! and transparent pixels are simply discarded rather than blended.
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
};
struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) light: f32,
};

@vertex
fn vs_main(input: VertexInput) -> VertexOutput {
    var output: VertexOutput;
    output.clip_position = camera.view_proj * vec4<f32>(input.position, 1.0);
    output.uv = input.uv;
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
    return vec4<f32>(texel.rgb * input.light, 1.0);
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
        );
        Ok(Self {
            device,
            queue,
            pipeline,
        })
    }

    /// Rasterizes `mesh`, sampling `atlas` by each quad's texture name.
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
        let vertices = build_vertices(mesh, atlas)?;

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
                contents: bytemuck::cast_slice(&vertices),
                usage: wgpu::BufferUsages::VERTEX,
            });
        let index_buffer = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("horizon-1-16-5 indices"),
                contents: bytemuck::cast_slice(&mesh.indices),
                usage: wgpu::BufferUsages::INDEX,
            });

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
        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("horizon-1-16-5 atlas bind group"),
            layout: &self.pipeline.get_bind_group_layout(0),
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
        });

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
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &bind_group, &[]);
            pass.set_vertex_buffer(0, vertex_buffer.slice(..));
            pass.set_index_buffer(index_buffer.slice(..), wgpu::IndexFormat::Uint32);
            pass.draw_indexed(0..mesh.indices.len() as u32, 0, 0..1);
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

/// Builds the shared mesh pipeline: one triangle-list pass with position,
/// UV and normal vertex attributes, back-face culling, depth testing and a
/// single `format` colour target. Offscreen and windowed rendering use the
/// same shader and conventions; only the colour format differs.
fn create_pipeline(
    device: &wgpu::Device,
    label: &str,
    format: wgpu::TextureFormat,
) -> wgpu::RenderPipeline {
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("horizon-1-16-5 mesh shader"),
        source: wgpu::ShaderSource::Wgsl(SHADER.into()),
    });
    let attributes = wgpu::vertex_attr_array![
        0 => Float32x3,
        1 => Float32x2,
        2 => Float32x3,
    ];
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
            entry_point: Some("fs_main"),
            compilation_options: Default::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format,
                blend: None,
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
            depth_write_enabled: Some(true),
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
        for vertex in quad {
            let local = [vertex.uv[0] / 16.0, vertex.uv[1] / 16.0];
            vertices.push(GpuVertex {
                position: vertex.position,
                uv: [
                    uv_rect[0] + local[0] * (uv_rect[2] - uv_rect[0]),
                    uv_rect[1] + local[1] * (uv_rect[3] - uv_rect[1]),
                ],
                normal: vertex.normal,
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
        let pipeline = create_pipeline(&device, "horizon-1-16-5 surface pipeline", format);
        let hud_pipeline =
            create_hud_pipeline(&device, "horizon-1-16-5 surface hud pipeline", format);
        let depth = create_depth(&device, size[0], size[1]);
        let renderer = Self {
            surface,
            device,
            queue,
            pipeline,
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
    /// presents it to the window. The atlas is uploaded on every call.
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
        let vertices = build_vertices(mesh, atlas)?;
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
                contents: bytemuck::cast_slice(&vertices),
                usage: wgpu::BufferUsages::VERTEX,
            });
        let index_buffer = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("horizon-1-16-5 indices"),
                contents: bytemuck::cast_slice(&mesh.indices),
                usage: wgpu::BufferUsages::INDEX,
            });

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
        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("horizon-1-16-5 atlas bind group"),
            layout: &self.pipeline.get_bind_group_layout(0),
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
        });

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
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &bind_group, &[]);
            pass.set_vertex_buffer(0, vertex_buffer.slice(..));
            pass.set_index_buffer(index_buffer.slice(..), wgpu::IndexFormat::Uint32);
            pass.draw_indexed(0..mesh.indices.len() as u32, 0, 0..1);
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

    fn temp_pack(red: [u8; 4], blue: [u8; 4]) -> std::path::PathBuf {
        let id = NEXT_DIR.fetch_add(1, Ordering::Relaxed);
        let root =
            std::env::temp_dir().join(format!("horizon-1165-render-{}-{id}", std::process::id()));
        let textures = root.join("minecraft/textures/block");
        std::fs::create_dir_all(&textures).unwrap();
        for (name, colour) in [("red.png", red), ("blue.png", blue)] {
            let file = std::fs::File::create(textures.join(name)).unwrap();
            let mut encoder = png::Encoder::new(file, 8, 8);
            encoder.set_color(png::ColorType::Rgba);
            encoder.set_depth(png::BitDepth::Eight);
            let mut writer = encoder.write_header().unwrap();
            writer.write_image_data(&colour.repeat(64)).unwrap();
            writer.finish().unwrap();
        }
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

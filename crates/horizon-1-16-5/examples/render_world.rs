//! Renders the first real 1.16.5 frame: an Anvil chunk, the client jar's
//! format-6 models and textures, the CPU mesher and the offscreen wgpu
//! renderer, written as a PNG. Usage:
//!
//! cargo run -p horizon-1-16-5 --example render_world -- \
//!   --world .local/versions/1.16.5/world \
//!   --tables .local/versions/1.16.5/tables.json \
//!   --client-jar .local/versions/1.16.5/inputs/client.jar \
//!   --assets-index .local/versions/1.16.5/inputs/assets/indexes/1.16.json \
//!   --assets-objects .local/versions/1.16.5/inputs/assets/objects \
//!   --out first-frame.png
use horizon_1_16_5::BlockTable;
use horizon_1_16_5::assets::ModelPack;
use horizon_1_16_5::atlas::{TextureAtlas, VanillaAssets};
use horizon_1_16_5::mesh::mesh_box;
use horizon_1_16_5::render::{Camera, OffscreenRenderer};
use horizon_1_16_5::world::World;
use std::path::PathBuf;

fn main() -> Result<(), String> {
    let mut world = None;
    let mut tables = None;
    let mut client_jar = None;
    let mut assets_index = None;
    let mut assets_objects = None;
    let mut out = PathBuf::from("first-frame.png");
    let mut width = 1280;
    let mut height = 720;
    let mut chunk_x = 0;
    let mut chunk_z = 0;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let value = args.next().ok_or_else(|| format!("{arg} needs a value"))?;
        match arg.as_str() {
            "--world" => world = Some(PathBuf::from(value)),
            "--tables" => tables = Some(PathBuf::from(value)),
            "--client-jar" => client_jar = Some(PathBuf::from(value)),
            "--assets-index" => assets_index = Some(PathBuf::from(value)),
            "--assets-objects" => assets_objects = Some(PathBuf::from(value)),
            "--out" => out = PathBuf::from(value),
            "--width" => width = value.parse().map_err(|e| format!("--width: {e}"))?,
            "--height" => height = value.parse().map_err(|e| format!("--height: {e}"))?,
            "--chunk-x" => chunk_x = value.parse().map_err(|e| format!("--chunk-x: {e}"))?,
            "--chunk-z" => chunk_z = value.parse().map_err(|e| format!("--chunk-z: {e}"))?,
            other => return Err(format!("unknown argument {other}")),
        }
    }
    let world_path = world.ok_or("--world is required")?;
    let tables_path = tables.ok_or("--tables is required")?;
    let client_jar = client_jar.ok_or("--client-jar is required")?;
    let assets_index = assets_index.ok_or("--assets-index is required")?;
    let assets_objects = assets_objects.ok_or("--assets-objects is required")?;

    let table = BlockTable::load(&tables_path)?;
    let world = World::open(&world_path, table)?;
    let models = ModelPack::load_client_jar(&client_jar)?;
    let mut assets = VanillaAssets::open(&client_jar, Some(&assets_index), Some(&assets_objects))?;
    let chunk = world
        .chunk(chunk_x, chunk_z)
        .ok_or_else(|| format!("world has no chunk ({chunk_x}, {chunk_z})"))?;
    let min = [chunk.x * 16, 0, chunk.z * 16];
    let mesh = mesh_box(&world, &models, min, [16, 256, 16]);
    if mesh.vertices.is_empty() {
        return Err("the model pack produced no geometry for the chunk".into());
    }

    let atlas = TextureAtlas::load(&mut assets, mesh.textures.iter().cloned())?;
    let renderer = OffscreenRenderer::new().map_err(|error| error.to_string())?;
    let camera = Camera::framing(&mesh, std::f32::consts::FRAC_PI_4, 0.55, 2.2);
    let frame = renderer
        .render(&mesh, &atlas, camera, width, height)
        .map_err(|error| error.to_string())?;
    frame.write_png(&out)?;
    println!(
        "chunk ({}, {}) -> {} vertices, {} indices, {} textures, atlas {}x{} -> {}x{} {}",
        chunk.x,
        chunk.z,
        mesh.vertices.len(),
        mesh.indices.len(),
        mesh.textures.len(),
        atlas.width(),
        atlas.height(),
        frame.width,
        frame.height,
        out.display()
    );
    Ok(())
}

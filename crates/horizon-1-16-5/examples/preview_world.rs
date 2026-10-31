//! Software preview of one loaded 1.16.5 chunk using the real format-6
//! model pack. Usage:
//!
//! cargo run -p horizon-1-16-5 --example preview_world -- \
//!   --world DIR --tables tables.json --pack assets/minecraft --out preview.ppm
use horizon_1_16_5::assets::ModelPack;
use horizon_1_16_5::mesh::mesh_box;
use horizon_1_16_5::preview::{default_camera, render, write_ppm};
use horizon_1_16_5::world::World;
use horizon_1_16_5::{BlockSource, BlockTable};
use std::path::PathBuf;

fn main() -> Result<(), String> {
    let mut world = None;
    let mut tables = None;
    let mut pack = None;
    let mut out = PathBuf::from("preview.ppm");
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let value = args.next().ok_or_else(|| format!("{arg} needs a value"))?;
        match arg.as_str() {
            "--world" => world = Some(PathBuf::from(value)),
            "--tables" => tables = Some(PathBuf::from(value)),
            "--pack" => pack = Some(PathBuf::from(value)),
            "--out" => out = PathBuf::from(value),
            other => return Err(format!("unknown argument {other}")),
        }
    }
    let world_path = world.ok_or("--world is required")?;
    let tables_path = tables.ok_or("--tables is required")?;
    let pack_path = pack.ok_or("--pack is required (assets/minecraft)")?;
    let table = BlockTable::load(&tables_path)?;
    let world = World::open(&world_path, table)?;
    let models = ModelPack::load_lenient(&pack_path)?;
    let chunk = world
        .chunks()
        .find(|chunk| {
            world
                .state_at(chunk.x * 16 + 8, 4, chunk.z * 16 + 8)
                .is_some()
        })
        .ok_or("the world has no loaded chunk")?;
    let min = [chunk.x * 16, 0, chunk.z * 16];
    let mesh = mesh_box(&world, &models, min, [16, 256, 16]);
    if mesh.vertices.is_empty() {
        return Err("the model pack produced no geometry for the first chunk".into());
    }
    let (yaw, pitch, scale) = default_camera();
    let image = render(&mesh, 800, 600, yaw, pitch, scale * 3.0);
    write_ppm(&out, &image)?;
    println!(
        "chunk ({}, {}) -> {} vertices, {} indices, {} -> {}",
        chunk.x,
        chunk.z,
        mesh.vertices.len(),
        mesh.indices.len(),
        out.display(),
        image.width * image.height * 3
    );
    Ok(())
}

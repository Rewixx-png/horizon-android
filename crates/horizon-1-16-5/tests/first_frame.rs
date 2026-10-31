//! End-to-end checks against the private 1.16.5 inputs prepared by
//! `tools/prepare_1165.py`. They are `#[ignore]`d because `.local/` is not in
//! Git; run them after preparing the version bundle:
//!
//! cargo test -p horizon-1-16-5 --test first_frame -- --ignored --nocapture

use horizon_1_16_5::assets::ModelPack;
use horizon_1_16_5::atlas::{MISSING_TEXTURE, TextureAtlas, VanillaAssets};
use horizon_1_16_5::mesh::mesh_box;
use horizon_1_16_5::render::{Camera, OffscreenRenderer};
use horizon_1_16_5::world::World;
use horizon_1_16_5::{BlockSource, BlockTable};
use std::path::{Path, PathBuf};

fn local() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(".local/versions/1.16.5")
}

struct Inputs {
    world: PathBuf,
    tables: PathBuf,
    client_jar: PathBuf,
    assets_index: PathBuf,
    assets_objects: PathBuf,
}

fn inputs() -> Inputs {
    let root = local();
    Inputs {
        world: std::env::var_os("HORIZON_1165_WORLD")
            .map(PathBuf::from)
            .unwrap_or_else(|| root.join("world")),
        tables: root.join("tables.json"),
        client_jar: root.join("inputs/client.jar"),
        assets_index: root.join("inputs/assets/indexes/1.16.json"),
        assets_objects: root.join("inputs/assets/objects"),
    }
}

fn require(path: &Path) {
    assert!(
        path.exists(),
        "missing {}: run `python3 tools/prepare_1165.py --assets` and prepare a test world",
        path.display()
    );
}

#[test]
#[ignore = "needs .local/versions/1.16.5 inputs prepared by tools/prepare_1165.py"]
fn client_jar_models_and_textures_bake() {
    let inputs = inputs();
    require(&inputs.client_jar);
    let models = ModelPack::load_client_jar(&inputs.client_jar).unwrap();
    let stone = horizon_1_16_5::BlockState::new("minecraft:stone");
    let baked = models.model(&stone).expect("stone has a baked model");
    assert_eq!(baked.quads.len(), 6, "a stone cube has six faces");
    assert!(
        baked
            .quads
            .iter()
            .all(|quad| quad.texture == "minecraft:block/stone")
    );

    let mut assets = VanillaAssets::open(
        &inputs.client_jar,
        Some(&inputs.assets_index),
        Some(&inputs.assets_objects),
    )
    .unwrap();
    let atlas = TextureAtlas::load(
        &mut assets,
        [
            "minecraft:block/stone".to_owned(),
            MISSING_TEXTURE.to_owned(),
        ],
    )
    .unwrap();
    let sprite = atlas.sprite("minecraft:block/stone").unwrap();
    assert_eq!((sprite.width, sprite.height), (16, 16));
    // The real stone sprite is not a single flat colour.
    let first = atlas.sample("minecraft:block/stone", 0.0, 0.0).unwrap();
    assert!(
        (0..16).any(|x| atlas.sample("minecraft:block/stone", x as f32 / 16.0, 0.5) != Some(first)),
        "stone texture should contain visible variation"
    );
}

#[test]
#[ignore = "needs .local/versions/1.16.5 inputs and a 1.16.5 world"]
fn first_frame_from_a_real_world() {
    let inputs = inputs();
    for path in [
        &inputs.world,
        &inputs.tables,
        &inputs.client_jar,
        &inputs.assets_index,
        &inputs.assets_objects,
    ] {
        require(path);
    }
    let table = BlockTable::load(&inputs.tables).unwrap();
    let world = World::open(&inputs.world, table).unwrap();
    assert!(!world.is_empty(), "the test world has no loaded chunks");
    let models = ModelPack::load_client_jar(&inputs.client_jar).unwrap();
    let mut assets = VanillaAssets::open(
        &inputs.client_jar,
        Some(&inputs.assets_index),
        Some(&inputs.assets_objects),
    )
    .unwrap();

    let non_empty = |chunk: &horizon_1_16_5::world::ChunkColumn| {
        (0..64).any(|y| {
            world
                .state_at(chunk.x * 16 + 8, y, chunk.z * 16 + 8)
                .is_some_and(|state| state.name != "minecraft:air")
        })
    };
    let chunk = world
        .chunk(0, 0)
        .filter(|chunk| non_empty(chunk))
        .or_else(|| world.chunks().find(|chunk| non_empty(chunk)))
        .expect("a non-empty chunk");
    let mesh = mesh_box(
        &world,
        &models,
        [chunk.x * 16, 0, chunk.z * 16],
        [16, 256, 16],
    );
    assert!(!mesh.vertices.is_empty(), "the chunk produced no geometry");
    assert_eq!(mesh.vertices.len() % 4, 0);
    assert_eq!(mesh.textures.len(), mesh.vertices.len() / 4);

    let atlas = TextureAtlas::load(&mut assets, mesh.textures.iter().cloned()).unwrap();
    let renderer = OffscreenRenderer::new().unwrap_or_else(|error| panic!("{error}"));
    let camera = Camera::framing(&mesh, std::f32::consts::FRAC_PI_4, 0.55, 2.2);
    let frame = renderer
        .render(&mesh, &atlas, camera, 320, 180)
        .unwrap_or_else(|error| panic!("{error}"));
    let out = std::env::var_os("HORIZON_1165_FRAME_OUT")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("horizon-1165-first-frame.png"));
    frame.write_png(&out).unwrap();
    eprintln!(
        "chunk ({}, {}) -> {} vertices, {} textures, atlas {}x{}, frame {}x{} -> {}",
        chunk.x,
        chunk.z,
        mesh.vertices.len(),
        mesh.textures.len(),
        atlas.width(),
        atlas.height(),
        frame.width,
        frame.height,
        out.display()
    );

    // The sky is dark blue-grey; textured geometry must cover a real part of
    // the frame with colours the clear colour cannot produce.
    let clear = frame.pixel(0, 0).unwrap();
    let covered = frame
        .rgba
        .chunks_exact(4)
        .filter(|pixel| *pixel != clear)
        .count();
    assert!(
        covered > (frame.width * frame.height) as usize / 20,
        "the mesh covered only {covered} pixels"
    );
    let bytes = std::fs::read(&out).unwrap();
    assert_eq!(&bytes[..8], b"\x89PNG\r\n\x1a\n");
}

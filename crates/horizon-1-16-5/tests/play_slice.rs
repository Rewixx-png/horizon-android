//! End-to-end play-slice checks against the private 1.16.5 bundle:
//! spawning, walking/falling, break/place and a rendered frame. They are
//! `#[ignore]`d because `.local/` is not in Git.

use horizon_1_16_5::assets::ModelPack;
use horizon_1_16_5::atlas::TextureAtlas;
use horizon_1_16_5::atlas::VanillaAssets;
use horizon_1_16_5::inventory::ItemStack;
use horizon_1_16_5::player::Input;
use horizon_1_16_5::render::{Camera, OffscreenRenderer, RenderError};
use horizon_1_16_5::server::{FrameInput, IntegratedServer};
use horizon_1_16_5::survival::GameMode;
use horizon_1_16_5::world::World;
use horizon_1_16_5::{BlockSource, BlockState, BlockTable};
use std::path::{Path, PathBuf};

fn root() -> PathBuf {
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
    let root = root();
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

fn load_world(inputs: &Inputs) -> World {
    for path in [&inputs.world, &inputs.tables] {
        assert!(
            path.exists(),
            "missing {}: prepare a 1.16.5 world and run tools/prepare_1165.py",
            path.display()
        );
    }
    World::open(
        &inputs.world,
        BlockTable::load(&inputs.tables).expect("tables load"),
    )
    .expect("world loads")
}

fn state_name(server: &IntegratedServer, x: i32, y: i32, z: i32) -> String {
    server.world.state_at(x, y, z).expect("loaded chunk").name
}

#[test]
#[ignore = "needs .local/versions/1.16.5/world and prepared inputs"]
fn spawns_falls_walks_and_stays_on_solid_ground() {
    let inputs = inputs();
    let world = load_world(&inputs);
    let mut server = IntegratedServer::open(world);

    // The fixture puts an oak log at (0, 9, 0); the spawn scan lands on it.
    assert!((server.player.position[0] - 0.5).abs() < 1e-9);
    assert!((server.player.position[2] - 0.5).abs() < 1e-9);
    assert!(server.player.position[1] > 9.9);

    for _ in 0..80 {
        server.tick(&FrameInput::default());
    }
    assert!(server.player.on_ground, "player does not settle");
    let start = server.player.position;

    let walk = FrameInput {
        movement: Input {
            forward: true,
            ..Input::default()
        },
        ..FrameInput::default()
    };
    for _ in 0..30 {
        server.tick(&walk);
    }
    let end = server.player.position;
    assert!(
        (end[2] - start[2]).abs() > 1.0,
        "walking moved only {:.3} blocks",
        (end[2] - start[2]).abs()
    );
    assert!(server.player.on_ground, "walking ended airborne");
    assert!(
        end[1] == 7.0 || end[1] == 6.0,
        "unexpected surface height {} (planks top 7, grass top 6)",
        end[1]
    );
}

#[test]
#[ignore = "needs .local/versions/1.16.5/world and prepared inputs"]
fn breaks_and_replaces_a_plank_in_one_tick() {
    let inputs = inputs();
    let world = load_world(&inputs);
    // The plank platform is y=6, so feet at y=7 stand on it. Put the player
    // outside the glass box (x/z 1..5) so the down ray starts in air and
    // hits the plank at (3,6,-4).
    let mut server = IntegratedServer::new(world, [3.5, 7.0, -3.5]);
    server.player.set_look(0.0, 90.0);
    server.tick(&FrameInput::default());
    assert_eq!(state_name(&server, 3, 6, -4), "minecraft:oak_planks");

    let action = FrameInput {
        break_block: true,
        place_block: true,
        ..FrameInput::default()
    };
    server.tick(&action);
    // Break removes the plank; place casts again, hits the grass block two
    // blocks below and fills the air cell directly above it with stone.
    assert_eq!(state_name(&server, 3, 6, -4), "minecraft:air");
    assert_eq!(state_name(&server, 3, 4, -4), "minecraft:stone");
    assert!(server.dirty);

    // The grass hotbar state has no `snowy` property; placing it resolves
    // through the table's first grass_block state instead of failing.
    server.inventory.select(2);
    server.tick(&FrameInput {
        place_block: true,
        ..FrameInput::default()
    });
    let grass = server.world.state_at(3, 5, -4).expect("loaded");
    assert_eq!(grass.name, "minecraft:grass_block");
    // The default state is the lowest id; some 1.16.5 tables list
    // `snowy=true` first, so only the name is asserted here.
}

#[test]
#[ignore = "needs .local/versions/1.16.5/world and prepared inputs"]
fn play_mesh_renders_a_first_person_frame() {
    let inputs = inputs();
    for path in [
        &inputs.client_jar,
        &inputs.assets_index,
        &inputs.assets_objects,
    ] {
        assert!(path.exists(), "missing {}", path.display());
    }
    let world = load_world(&inputs);
    let models = ModelPack::load_client_jar(&inputs.client_jar).expect("models");
    let mut assets = VanillaAssets::open(
        &inputs.client_jar,
        Some(&inputs.assets_index),
        Some(&inputs.assets_objects),
    )
    .expect("textures");
    let server = IntegratedServer::new(world, [0.5, 10.0, 0.5]);
    let mesh = server.render_mesh(&models, 1);
    assert!(!mesh.vertices.is_empty(), "play mesh is empty");
    assert_eq!(mesh.vertices.len() % 4, 0);
    assert_eq!(mesh.textures.len(), mesh.vertices.len() / 4);

    let atlas =
        TextureAtlas::load(&mut assets, mesh.textures.iter().cloned()).expect("atlas loads");
    let renderer = match OffscreenRenderer::new() {
        Ok(renderer) => renderer,
        Err(RenderError::NoAdapter(error)) => {
            eprintln!("skipping the play frame render: {error}");
            return;
        }
        Err(error) => panic!("{error}"),
    };
    // A straight-down view makes the [0, 1, 0] up vector parallel to the
    // view direction and the look-at matrix degenerate; aim slightly ahead.
    let eye = server.player.eye();
    let camera = Camera::looking_at(
        [eye[0] as f32, eye[1] as f32, eye[2] as f32],
        [
            eye[0] as f32 + 0.5,
            eye[1] as f32 - 1.0,
            eye[2] as f32 + 0.5,
        ],
    );
    let frame = renderer
        .render(&mesh, &atlas, camera, 320, 180)
        .expect("frame");
    let out = std::env::var_os("HORIZON_1165_PLAY_FRAME")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("horizon-1165-play-frame.png"));
    frame.write_png(&out).expect("png");
    let clear = frame.pixel(0, 0).expect("pixel");
    let covered = frame
        .rgba
        .chunks_exact(4)
        .filter(|pixel| *pixel != clear)
        .count();
    assert!(
        covered > (frame.width * frame.height) as usize / 10,
        "the play mesh covered only {covered} pixels"
    );
    assert_eq!(
        &std::fs::read(&out).expect("png bytes")[..8],
        b"\x89PNG\r\n\x1a\n"
    );
}

#[test]
#[ignore = "needs .local/versions/1.16.5/world and prepared inputs"]
fn edits_survive_save_and_reopen() {
    let inputs = inputs();
    let source = inputs.world;
    assert!(source.is_dir(), "missing {}", source.display());
    let target = std::env::temp_dir().join(format!(
        "horizon-1165-save-{}-{}",
        std::process::id(),
        std::thread::current().name().unwrap_or("test")
    ));
    let _ = std::fs::remove_dir_all(&target);
    copy_dir(&source, &target);
    let world_dir = target.canonicalize().unwrap();

    let table = BlockTable::load(&inputs.tables).expect("tables");
    let mut world = World::open(&world_dir, table).expect("world");
    let before = world.state_id_at(0, 9, 0).expect("fixture log");
    let stone = BlockState::new("minecraft:stone");
    world.set_state(0, 9, 0, &stone).expect("set block");
    assert!(world.has_unsaved_changes());
    assert!(world.save(&world_dir).expect("save"));
    assert!(!world.has_unsaved_changes());
    drop(world);

    let reopened = World::open(
        &world_dir,
        BlockTable::load(&inputs.tables).expect("tables"),
    )
    .expect("reopen");
    let after = reopened.state_id_at(0, 9, 0).expect("saved block");
    assert_ne!(after, before);
    let state = reopened.state_at(0, 9, 0).expect("saved state");
    assert_eq!(state.name, "minecraft:stone");
    std::fs::remove_dir_all(&target).unwrap();
}

fn copy_dir(source: &Path, target: &Path) {
    std::fs::create_dir_all(target).unwrap();
    for entry in std::fs::read_dir(source).unwrap() {
        let entry = entry.unwrap();
        let destination = target.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &destination);
        } else {
            std::fs::copy(entry.path(), destination).unwrap();
        }
    }
}

#[test]
#[ignore = "needs .local/versions/1.16.5 inputs (server.jar for loot, client.jar for models)"]
fn survival_break_uses_the_real_loot_tables() {
    let inputs = inputs();
    let server_jar = inputs.client_jar.with_file_name("server.jar");
    assert!(
        server_jar.is_file(),
        "missing {}: run tools/prepare_1165.py",
        server_jar.display()
    );
    let mut world = load_world(&inputs);
    world.load_loot(&server_jar).expect("loot tables");
    let mut server = IntegratedServer::new(world, [3.5, 7.0, -4.5]);
    server.survival.toggle_mode();
    assert_eq!(server.survival.mode, GameMode::Survival);
    for slot in 0..9 {
        server.survival.inventory.set_slot(slot, None);
    }
    // Look straight down at the plank platform at y = 6.
    server.player.set_look(0.0, 90.0);
    server.tick(&FrameInput::default());
    server.tick(&FrameInput {
        break_block: true,
        ..FrameInput::default()
    });
    assert_eq!(state_name(&server, 3, 6, -5), "minecraft:air");
    let plank = server
        .survival
        .inventory
        .slots()
        .iter()
        .flatten()
        .find(|stack| stack.item == "minecraft:oak_planks")
        .expect("the plank drop is in the inventory");
    assert!(plank.count >= 1);

    // Glass drops nothing in 1.16.5 (`survives_explosion: false`).
    let mut glass_server = IntegratedServer::new(load_world(&inputs), [3.5, 7.0, 3.0]);
    glass_server.world.load_loot(&server_jar).expect("loot");
    glass_server.survival.toggle_mode();
    for slot in 0..9 {
        glass_server.survival.inventory.set_slot(slot, None);
    }
    glass_server.player.set_look(0.0, 90.0);
    glass_server.tick(&FrameInput::default());
    glass_server.tick(&FrameInput {
        break_block: true,
        ..FrameInput::default()
    });
    assert!(
        glass_server
            .survival
            .inventory
            .slots()
            .iter()
            .all(Option::is_none),
        "glass must not add an item: {:?}",
        glass_server.survival.inventory.slots()
    );
}

#[test]
#[ignore = "needs .local/versions/1.16.5 inputs (server.jar for recipes)"]
fn survival_crafts_oak_planks_from_logs() {
    let inputs = inputs();
    let server_jar = inputs.client_jar.with_file_name("server.jar");
    assert!(server_jar.is_file(), "missing {}", server_jar.display());
    let world = load_world(&inputs);
    let mut server = IntegratedServer::open(world);
    server.survival.toggle_mode();
    server.survival.load_recipes(&server_jar).expect("recipes");
    for slot in 0..9 {
        server.survival.inventory.set(slot, None);
    }
    server
        .survival
        .inventory
        .add_to_storage(ItemStack::new("minecraft:oak_log", 1));

    // Logs are a shapeless ring recipe: one log in the grid.
    let mut grid: [Option<ItemStack>; 9] = std::array::from_fn(|_| None);
    grid[4] = Some(ItemStack::new("minecraft:oak_log", 1));
    let result = server.survival.craft(&grid).expect("planks craft");
    assert_eq!(result.item, "minecraft:oak_planks");
    assert_eq!(result.count, 4);
    let count = |item: &str| -> u32 {
        server
            .survival
            .inventory
            .all_slots()
            .iter()
            .flatten()
            .filter(|stack| stack.item == item)
            .map(|stack| stack.count)
            .sum()
    };
    let planks = count("minecraft:oak_planks");
    assert_eq!(planks, 4);
    let logs = count("minecraft:oak_log");
    assert_eq!(logs, 0, "the grid log was consumed");
}

//! Windowed 1.16.5 play loop. The play binary calls [`run`].

use std::ffi::OsString;
use std::fmt::Display;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::{
    DeviceEvent, DeviceId, ElementState, KeyEvent, MouseButton, MouseScrollDelta, WindowEvent,
};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{KeyCode, PhysicalKey};
use winit::window::{CursorGrabMode, Window, WindowId};

use crate::assets::ModelPack;
use crate::atlas::{TextureAtlas, VanillaAssets};
use crate::player::{Input, Player};
use crate::render::{Camera, HudVertex, SurfaceRenderer, hud_rect};
use crate::server::{FrameInput, IntegratedServer};
use crate::touch::TouchControls;
use crate::world::World;
use crate::{BlockState, BlockTable, Mesh};

/// One 1/20 s tick.
const TICK: Duration = Duration::from_millis(50);
/// Yaw/pitch degrees per pixel of raw mouse motion.
const MOUSE_SENSITIVITY: f64 = 0.15;
/// Ticks run per `about_to_wait` at most, so a stalled loop cannot spiral.
const MAX_CATCHUP_TICKS: u32 = 10;
/// The chunk radius used when there is no command line (Android).
const DEFAULT_RADIUS: i32 = 4;
/// A smaller fixed radius for phones; remeshing 81 columns at startup is
/// unnecessary for the development slice.
#[cfg(target_os = "android")]
const ANDROID_RADIUS: i32 = 2;
/// A thin, light crosshair over the centre of the screen.
const CROSSHAIR_COLOR: [f32; 4] = [1.0, 1.0, 1.0, 0.75];
/// Half the crosshair arm length and its line thickness in pixels.
const CROSSHAIR_ARM: f32 = 6.0;
const CROSSHAIR_THICKNESS: f32 = 2.0;
/// How often the running world is flushed to disk, in wall-clock time.
const SAVE_INTERVAL: Duration = Duration::from_secs(30);
/// Unselected hotbar slot background.
const HOTBAR_SLOT_COLOR: [f32; 4] = [0.05, 0.05, 0.05, 0.45];
/// Extra overlay on the selected hotbar slot.
const HOTBAR_SELECTED_COLOR: [f32; 4] = [1.0, 1.0, 1.0, 0.40];
/// Fraction of a hotbar slot left free around the item swatch.
const HOTBAR_SWATCH_INSET: f32 = 0.2;

const USAGE: &str = "usage: horizon-1165 --world <dir> --tables <file> --client-jar <file> \
--assets-index <file> --assets-objects <dir> [--width <px>] [--height <px>] \
[--spawn <x> <y> <z>] [--radius <chunks>] [--frames <n>]";

/// The parsed play arguments.
#[derive(Debug)]
struct PlayOptions {
    world: PathBuf,
    tables: PathBuf,
    client_jar: PathBuf,
    assets_index: PathBuf,
    assets_objects: PathBuf,
    width: u32,
    height: u32,
    /// Player feet position in block coordinates.
    spawn: Option<[f64; 3]>,
    radius: i32,
    /// Exit after this many presented frames; 0 runs forever.
    frames: u32,
}

/// Parses the play arguments. Every flag takes its value as the following
/// argument; `--spawn` takes three. Unknown flags and stray positional
/// arguments are rejected with a clear error.
fn parse<I>(args: I) -> Result<PlayOptions, String>
where
    I: IntoIterator<Item = OsString>,
{
    let args: Vec<OsString> = args.into_iter().collect();
    let mut world = None;
    let mut tables = None;
    let mut client_jar = None;
    let mut assets_index = None;
    let mut assets_objects = None;
    let mut width = 1280u32;
    let mut height = 720u32;
    let mut spawn = None;
    let mut radius = DEFAULT_RADIUS;
    let mut frames = 0u32;

    let mut i = 0;
    while i < args.len() {
        let flag = args[i]
            .to_str()
            .ok_or_else(|| format!("argument {} is not valid UTF-8", i + 1))?;
        match flag {
            "--world" => world = Some(parse_path(&args, &mut i, flag)?),
            "--tables" => tables = Some(parse_path(&args, &mut i, flag)?),
            "--client-jar" => client_jar = Some(parse_path(&args, &mut i, flag)?),
            "--assets-index" => assets_index = Some(parse_path(&args, &mut i, flag)?),
            "--assets-objects" => assets_objects = Some(parse_path(&args, &mut i, flag)?),
            "--width" => width = parse_number(&args, &mut i, flag)?,
            "--height" => height = parse_number(&args, &mut i, flag)?,
            "--radius" => radius = parse_number(&args, &mut i, flag)?,
            "--frames" => frames = parse_number(&args, &mut i, flag)?,
            "--spawn" => {
                let x = parse_number(&args, &mut i, flag)?;
                let y = parse_number(&args, &mut i, flag)?;
                let z = parse_number(&args, &mut i, flag)?;
                spawn = Some([x, y, z]);
            }
            _ if flag.starts_with('-') => {
                return Err(format!("unknown argument '{flag}'\n{USAGE}"));
            }
            _ => return Err(format!("unexpected argument '{flag}'\n{USAGE}")),
        }
        i += 1;
    }

    let mut missing = Vec::new();
    if world.is_none() {
        missing.push("--world");
    }
    if tables.is_none() {
        missing.push("--tables");
    }
    if client_jar.is_none() {
        missing.push("--client-jar");
    }
    if assets_index.is_none() {
        missing.push("--assets-index");
    }
    if assets_objects.is_none() {
        missing.push("--assets-objects");
    }
    if !missing.is_empty() {
        return Err(format!(
            "missing required argument{}: {}\n{USAGE}",
            if missing.len() == 1 { "" } else { "s" },
            missing.join(", "),
        ));
    }
    if width == 0 || height == 0 {
        return Err("--width and --height must be at least 1".into());
    }
    if radius < 1 {
        return Err("--radius must be at least 1".into());
    }

    Ok(PlayOptions {
        world: world.unwrap(),
        tables: tables.unwrap(),
        client_jar: client_jar.unwrap(),
        assets_index: assets_index.unwrap(),
        assets_objects: assets_objects.unwrap(),
        width,
        height,
        spawn,
        radius,
        frames,
    })
}

/// Consumes the value following the flag at `i` as a path.
fn parse_path(args: &[OsString], i: &mut usize, flag: &str) -> Result<PathBuf, String> {
    *i += 1;
    let value = args
        .get(*i)
        .ok_or_else(|| format!("{flag} needs a value"))?;
    Ok(PathBuf::from(value))
}

/// Consumes the value following the flag at `i` and parses it as a number.
fn parse_number<T>(args: &[OsString], i: &mut usize, flag: &str) -> Result<T, String>
where
    T: FromStr,
    T::Err: Display,
{
    *i += 1;
    let value = args
        .get(*i)
        .ok_or_else(|| format!("{flag} needs a value"))?;
    let text = value
        .to_str()
        .ok_or_else(|| format!("{flag} value is not valid UTF-8"))?;
    text.parse()
        .map_err(|error| format!("{flag} expects a number, got '{text}': {error}"))
}

/// Everything opened from the world, tables and client jar.
struct Loaded {
    /// The world directory the chunks are saved back into.
    world_root: PathBuf,
    server: IntegratedServer,
    models: ModelPack,
    assets: VanillaAssets,
    atlas: TextureAtlas,
    mesh: Mesh,
}

impl Loaded {
    /// Opens the world and the client jar. `assets_index`/`assets_objects`
    /// are the optional MoJang asset objects; without them textures come
    /// straight from the jar (the Android layout).
    fn open(
        world: PathBuf,
        tables: &Path,
        client_jar: &Path,
        assets_index: Option<&Path>,
        assets_objects: Option<&Path>,
        spawn: Option<[f64; 3]>,
        radius: i32,
    ) -> Result<Self, String> {
        let table = BlockTable::load(tables)?;
        let world_root = world;
        let world = World::open(&world_root, table)?;
        let models = ModelPack::load_client_jar(client_jar)?;
        let mut assets = match (assets_index, assets_objects) {
            (Some(index), Some(objects)) => {
                VanillaAssets::open(client_jar, Some(index), Some(objects))?
            }
            // Android bundles only the client jar: no index and no objects.
            _ => VanillaAssets::from_jar(client_jar)?,
        };
        let server = match spawn {
            Some(spawn) => IntegratedServer::new(world, spawn),
            None => IntegratedServer::open(world),
        };
        let mesh = server.render_mesh(&models, radius);
        let atlas = TextureAtlas::load(&mut assets, mesh.textures.iter().cloned())?;
        Ok(Self {
            world_root,
            server,
            models,
            assets,
            atlas,
            mesh,
        })
    }
}

/// Everything loaded before the window exists.
struct Pending {
    world_root: PathBuf,
    server: IntegratedServer,
    models: ModelPack,
    assets: VanillaAssets,
    atlas: TextureAtlas,
    mesh: Mesh,
    width: u32,
    height: u32,
    radius: i32,
    frames: u32,
}

/// The live game state: world loop, window and renderer.
struct PlayState {
    window: Arc<Window>,
    renderer: SurfaceRenderer,
    server: IntegratedServer,
    /// The world directory chunks are saved into on the timer and on exit.
    world_root: PathBuf,
    models: ModelPack,
    assets: VanillaAssets,
    atlas: TextureAtlas,
    mesh: Mesh,
    radius: i32,
    frames_limit: u32,
    frames_presented: u32,
    cursor_grabbed: bool,
    movement: Input,
    /// Virtual joystick, look area and place button. Fed by touch events and
    /// drained once per tick alongside the keyboard and mouse.
    touch: TouchControls,
    /// Whether the touch HUD is shown: always on Android, and on a desktop
    /// as soon as a touch screen reports a finger.
    touch_active: bool,
    /// Reused clip-space HUD vertices, refilled every frame.
    hud: Vec<HudVertex>,
    /// Absolute yaw/pitch in degrees, updated by the mouse and by touch look
    /// deltas, and consumed each tick by [`FrameInput`].
    look: Option<[f32; 2]>,
    break_block: bool,
    place_block: bool,
    select: Option<usize>,
    scroll: i32,
    last_tick: Instant,
    /// When the world was last flushed by the periodic save timer.
    last_save: Instant,
    /// The chunk the visible mesh was built around.
    last_chunk: [i32; 2],
}

impl PlayState {
    /// Flushes dirty chunks under [`Self::world_root`] and stamps the periodic
    /// timer.
    ///
    /// `has_unsaved_changes` keeps a clean world from touching the disk. A
    /// failure is logged instead of propagated, so a broken save location
    /// cannot kill the loop; whatever stays dirty is retried on the next
    /// interval.
    fn save(&mut self, now: Instant) -> bool {
        self.last_save = now;
        if !self.server.world.has_unsaved_changes() {
            return false;
        }
        match self.server.world.save(&self.world_root) {
            Ok(wrote) => wrote,
            Err(error) => {
                eprintln!("saving the world to {}: {error}", self.world_root.display());
                false
            }
        }
    }
}

/// The winit application: owns the play state and funnels errors out.
struct PlayApp {
    pending: Option<Pending>,
    state: Option<PlayState>,
    error: Option<String>,
}

/// Parses the play arguments, opens the world and runs the window loop.
pub fn run() -> Result<(), String> {
    let options = parse(std::env::args_os().skip(1))?;

    let loaded = Loaded::open(
        options.world,
        &options.tables,
        &options.client_jar,
        Some(&options.assets_index),
        Some(&options.assets_objects),
        options.spawn,
        options.radius,
    )?;

    let mut app = PlayApp {
        pending: Some(Pending {
            world_root: loaded.world_root,
            server: loaded.server,
            models: loaded.models,
            assets: loaded.assets,
            atlas: loaded.atlas,
            mesh: loaded.mesh,
            width: options.width,
            height: options.height,
            radius: options.radius,
            frames: options.frames,
        }),
        state: None,
        error: None,
    };
    let event_loop =
        EventLoop::new().map_err(|error| format!("creating the event loop: {error}"))?;
    event_loop
        .run_app(&mut app)
        .map_err(|error| format!("event loop: {error}"))?;
    match app.error {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

/// Opens the bundled world and runs the window loop inside an Android
/// `NativeActivity`. Unlike [`run`] there is no command line: the inputs are
/// the files the entry point unpacked from the APK, textures come straight
/// from the client jar, and the loop runs until the activity is destroyed.
#[cfg(target_os = "android")]
pub fn run_android(
    app: winit::platform::android::activity::AndroidApp,
    world: PathBuf,
    tables: PathBuf,
    client_jar: PathBuf,
) -> Result<(), String> {
    use winit::platform::android::EventLoopBuilderExtAndroid;

    let loaded = Loaded::open(
        world,
        &tables,
        &client_jar,
        None,
        None,
        None,
        ANDROID_RADIUS,
    )?;
    let event_loop = EventLoop::builder()
        .with_android_app(app)
        .build()
        .map_err(|error| format!("creating the event loop: {error}"))?;
    let mut play = PlayApp {
        pending: Some(Pending {
            world_root: loaded.world_root,
            server: loaded.server,
            models: loaded.models,
            assets: loaded.assets,
            atlas: loaded.atlas,
            mesh: loaded.mesh,
            width: 1,
            height: 1,
            radius: ANDROID_RADIUS,
            frames: 0,
        }),
        state: None,
        error: None,
    };
    event_loop
        .run_app(&mut play)
        .map_err(|error| format!("event loop: {error}"))?;
    match play.error {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

impl PlayApp {
    /// Runs one 1/20 s tick and rebuilds the visible mesh when the world or
    /// the player's chunk changed.
    fn tick(&mut self) -> Result<(), String> {
        let Some(state) = self.state.as_mut() else {
            return Ok(());
        };
        let touch = state.touch.frame();
        let mut movement = merge_movement(state.movement, touch.movement);
        // A joystick-zone tap is a one-tick jump, ORed with a held Space.
        movement.jump |= touch.jump;
        let input = FrameInput {
            movement,
            look: merged_look(
                state.look.take(),
                touch.look,
                [state.server.player.yaw, state.server.player.pitch],
            ),
            break_block: state.break_block || touch.break_block,
            place_block: state.place_block || touch.place_block,
            select: merge_select(state.select.take(), touch.select),
            scroll: std::mem::take(&mut state.scroll),
        };
        state.break_block = false;
        state.place_block = false;
        state.server.tick(&input);
        let chunk = player_chunk(&state.server.player);
        if state.server.dirty || chunk != state.last_chunk {
            state.mesh = state.server.render_mesh(&state.models, state.radius);
            state.atlas =
                TextureAtlas::load(&mut state.assets, state.mesh.textures.iter().cloned())?;
            state.server.dirty = false;
            state.last_chunk = chunk;
        }
        Ok(())
    }

    /// Renders one frame with the player's first-person camera.
    fn render(&mut self, event_loop: &ActiveEventLoop) -> Result<(), String> {
        let Some(state) = self.state.as_mut() else {
            return Ok(());
        };
        let camera = camera_for(&state.server.player);
        let size = state.window.inner_size();
        // Keyboard digits, the wheel and touch all converge on the inventory,
        // so mirror its selection into the touch HUD's highlight.
        state.touch.set_selected(state.server.inventory.selected());
        fill_hud(
            &mut state.hud,
            &state.touch,
            state.server.inventory.slots(),
            state.touch_active,
            size.width,
            size.height,
        );
        state
            .renderer
            .render_with_hud(&state.mesh, &state.atlas, camera, &state.hud)
            .map_err(|error| error.to_string())?;
        state.frames_presented += 1;
        if state.frames_limit > 0 && state.frames_presented >= state.frames_limit {
            println!("rendered {} frames", state.frames_presented);
            event_loop.exit();
        }
        Ok(())
    }

    fn key(&mut self, event_loop: &ActiveEventLoop, event: KeyEvent) {
        let PhysicalKey::Code(code) = event.physical_key else {
            return;
        };
        let pressed = event.state == ElementState::Pressed;
        let Some(state) = self.state.as_mut() else {
            return;
        };
        match code {
            KeyCode::KeyW => state.movement.forward = pressed,
            KeyCode::KeyS => state.movement.back = pressed,
            KeyCode::KeyA => state.movement.left = pressed,
            KeyCode::KeyD => state.movement.right = pressed,
            // Jump only on the press edge so auto-repeat cannot re-trigger it.
            KeyCode::Space if !event.repeat => state.movement.jump = pressed,
            KeyCode::ShiftLeft | KeyCode::ShiftRight => state.movement.sprint = pressed,
            KeyCode::ControlLeft | KeyCode::ControlRight => state.movement.sneak = pressed,
            KeyCode::Escape if pressed && !event.repeat => {
                if state.cursor_grabbed {
                    let _ = state.window.set_cursor_grab(CursorGrabMode::None);
                    state.window.set_cursor_visible(true);
                    state.cursor_grabbed = false;
                } else {
                    state.save(Instant::now());
                    event_loop.exit();
                }
            }
            // Digits select hotbar slots 1-9.
            _ if pressed && !event.repeat => {
                if let Some(slot) = hotbar_slot(code) {
                    state.select = Some(slot);
                }
            }
            _ => {}
        }
    }

    fn mouse_motion(&mut self, dx: f64, dy: f64) {
        let Some(state) = self.state.as_mut() else {
            return;
        };
        if !state.cursor_grabbed {
            return;
        }
        // The player convention: yaw 0 looks toward +Z, +90 toward +X (left
        // from +Z), and positive pitch looks down. So moving the mouse right
        // turns right (yaw decreases) and moving it up looks up (pitch
        // decreases).
        let base = state
            .look
            .unwrap_or([state.server.player.yaw, state.server.player.pitch]);
        let delta = [
            (dx * MOUSE_SENSITIVITY) as f32,
            (dy * MOUSE_SENSITIVITY) as f32,
        ];
        state.look = Some(apply_look_delta(base, delta));
    }
}

impl ApplicationHandler for PlayApp {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.state.is_some() || self.error.is_some() {
            return;
        }
        let Some(pending) = self.pending.take() else {
            return;
        };
        let attributes = Window::default_attributes()
            .with_title("Horizon 1.16.5")
            .with_inner_size(LogicalSize::new(pending.width, pending.height));
        let window = match event_loop.create_window(attributes) {
            Ok(window) => Arc::new(window),
            Err(error) => {
                self.error = Some(format!("creating the window: {error}"));
                event_loop.exit();
                return;
            }
        };
        let renderer = match SurfaceRenderer::new(window.clone()) {
            Ok(renderer) => renderer,
            Err(error) => {
                self.error = Some(format!("initializing the renderer: {error}"));
                event_loop.exit();
                return;
            }
        };
        // A failed grab (e.g. a server without pointer constraints) just
        // leaves the cursor free; the loop itself keeps running.
        let _ = window.set_cursor_grab(CursorGrabMode::Locked);
        window.set_cursor_visible(false);
        let mut touch = TouchControls::new();
        let size = window.inner_size();
        touch.resize(size.width as f32, size.height as f32);
        let last_chunk = player_chunk(&pending.server.player);
        let state = PlayState {
            window,
            renderer,
            server: pending.server,
            world_root: pending.world_root,
            models: pending.models,
            assets: pending.assets,
            atlas: pending.atlas,
            mesh: pending.mesh,
            radius: pending.radius,
            frames_limit: pending.frames,
            frames_presented: 0,
            cursor_grabbed: true,
            movement: Input::default(),
            touch,
            touch_active: cfg!(target_os = "android")
                || std::env::var_os("HORIZON_1165_TOUCH_HUD").is_some(),
            hud: Vec::new(),
            look: None,
            break_block: false,
            place_block: false,
            select: None,
            scroll: 0,
            last_tick: Instant::now(),
            last_save: Instant::now(),
            last_chunk,
        };
        self.state = Some(state);
        if let Some(state) = &self.state {
            state.window.request_redraw();
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _: WindowId, event: WindowEvent) {
        if self.state.is_none() {
            return;
        }
        match event {
            WindowEvent::CloseRequested => {
                if let Some(state) = self.state.as_mut() {
                    state.save(Instant::now());
                }
                event_loop.exit();
            }
            WindowEvent::Resized(size) => {
                let state = self.state.as_mut().unwrap();
                state.renderer.resize(size.width, size.height);
                state.touch.resize(size.width as f32, size.height as f32);
            }
            WindowEvent::RedrawRequested => {
                if let Err(error) = self.render(event_loop) {
                    self.error = Some(error);
                    event_loop.exit();
                }
            }
            WindowEvent::KeyboardInput { event, .. } => self.key(event_loop, event),
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button,
                ..
            } => {
                let state = self.state.as_mut().unwrap();
                match button {
                    MouseButton::Left => state.break_block = true,
                    MouseButton::Right => state.place_block = true,
                    _ => {}
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let steps = match delta {
                    MouseScrollDelta::LineDelta(_, y) => wheel_steps(y),
                    MouseScrollDelta::PixelDelta(position) => wheel_steps(position.y as f32),
                };
                if steps != 0 {
                    self.state.as_mut().unwrap().scroll += steps;
                }
            }
            WindowEvent::Touch(touch) => {
                let state = self.state.as_mut().unwrap();
                state.touch_active = true;
                state.touch.touch(
                    touch.id,
                    touch.phase,
                    touch.location.x as f32,
                    touch.location.y as f32,
                );
            }
            _ => {}
        }
    }

    fn device_event(&mut self, _: &ActiveEventLoop, _: DeviceId, event: DeviceEvent) {
        if let DeviceEvent::MouseMotion { delta: (x, y) } = event {
            self.mouse_motion(x, y);
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        let now = Instant::now();
        let ticks = {
            let Some(state) = self.state.as_mut() else {
                event_loop.set_control_flow(ControlFlow::Wait);
                return;
            };
            let mut ticks = 0;
            while now.saturating_duration_since(state.last_tick) >= TICK
                && ticks < MAX_CATCHUP_TICKS
            {
                state.last_tick += TICK;
                ticks += 1;
            }
            // Still behind after the catch-up cap: drop the backlog.
            if now.saturating_duration_since(state.last_tick) >= TICK {
                state.last_tick = now;
            }
            ticks
        };
        for _ in 0..ticks {
            if let Err(error) = self.tick() {
                self.error = Some(error);
                event_loop.exit();
                return;
            }
        }
        let Some(state) = self.state.as_mut() else {
            return;
        };
        if save_due(state.last_save, now) {
            state.save(now);
        }
        if ticks > 0 {
            state.window.request_redraw();
        }
        event_loop.set_control_flow(ControlFlow::WaitUntil(state.last_tick + TICK));
    }

    /// Last-chance flush when the event loop shuts down for any reason.
    fn exiting(&mut self, _: &ActiveEventLoop) {
        if let Some(state) = self.state.as_mut() {
            state.save(Instant::now());
        }
    }
}

/// The chunk column of a player feet position in block coordinates.
fn player_chunk(player: &Player) -> [i32; 2] {
    [
        (player.position[0] / 16.0).floor() as i32,
        (player.position[2] / 16.0).floor() as i32,
    ]
}

/// A first-person camera at the player's eye looking along [`Player::look`].
fn camera_for(player: &Player) -> Camera {
    let eye = player.eye();
    let eye = [eye[0] as f32, eye[1] as f32, eye[2] as f32];
    let look = player.look();
    Camera::looking_at(eye, [eye[0] + look[0], eye[1] + look[1], eye[2] + look[2]])
}

/// The hotbar slot a digit key selects: `Digit1` is slot 0.
fn hotbar_slot(code: KeyCode) -> Option<usize> {
    match code {
        KeyCode::Digit1 => Some(0),
        KeyCode::Digit2 => Some(1),
        KeyCode::Digit3 => Some(2),
        KeyCode::Digit4 => Some(3),
        KeyCode::Digit5 => Some(4),
        KeyCode::Digit6 => Some(5),
        KeyCode::Digit7 => Some(6),
        KeyCode::Digit8 => Some(7),
        KeyCode::Digit9 => Some(8),
        _ => None,
    }
}

/// The scroll direction of one wheel event: up is +1.
fn wheel_steps(y: f32) -> i32 {
    if y > 0.0 {
        1
    } else if y < 0.0 {
        -1
    } else {
        0
    }
}

/// Combines keyboard movement with held joystick movement.
fn merge_movement(keyboard: Input, touch: Input) -> Input {
    Input {
        forward: keyboard.forward || touch.forward,
        back: keyboard.back || touch.back,
        left: keyboard.left || touch.left,
        right: keyboard.right || touch.right,
        jump: keyboard.jump || touch.jump,
        sneak: keyboard.sneak || touch.sneak,
        sprint: keyboard.sprint || touch.sprint,
    }
}

/// Combines a keyboard/mouse hotbar pick for this tick with a touch pick.
///
/// A digit key pressed since the last tick wins over a simultaneous tap; at
/// most one selection reaches [`FrameInput`] so the inventory cannot apply
/// two picks in one tick.
fn merge_select(keyboard: Option<usize>, touch: Option<usize>) -> Option<usize> {
    keyboard.or(touch)
}

/// Whether the 30 second periodic save is due.
fn save_due(last_save: Instant, now: Instant) -> bool {
    now.saturating_duration_since(last_save) >= SAVE_INTERVAL
}

/// Applies a look delta in degrees to the pending mouse look, falling back to
/// the player's current look. The convention matches the mouse and
/// [`crate::touch::TouchFrame`]: `yaw -= delta[0]`, `pitch += delta[1]`.
fn merged_look(pending: Option<[f32; 2]>, delta: [f32; 2], current: [f32; 2]) -> Option<[f32; 2]> {
    if delta == [0.0, 0.0] {
        return pending;
    }
    Some(apply_look_delta(pending.unwrap_or(current), delta))
}

/// Adds a yaw/pitch delta, wrapping yaw into `-180..180` and clamping pitch
/// to the straight-up/straight-down limits.
fn apply_look_delta(base: [f32; 2], delta: [f32; 2]) -> [f32; 2] {
    let yaw = (base[0] - delta[0] + 180.0).rem_euclid(360.0) - 180.0;
    let pitch = (base[1] + delta[1]).clamp(-90.0, 90.0);
    [yaw, pitch]
}

/// Rebuilds `hud` for one frame: the touch controls and hotbar when they are
/// active, plus a small crosshair. An inactive HUD is left empty, so the
/// desktop frame is identical to [`SurfaceRenderer::render`].
fn fill_hud(
    hud: &mut Vec<HudVertex>,
    touch: &TouchControls,
    slots: &[Option<BlockState>; 9],
    active: bool,
    width: u32,
    height: u32,
) {
    hud.clear();
    if !active {
        return;
    }
    for (rect, color) in touch.hud_rects() {
        hud.extend_from_slice(&hud_rect(rect[0], rect[1], rect[2], rect[3], color));
    }
    let hotbar = touch.hotbar_rects();
    for (rect, _) in &hotbar {
        hud.extend_from_slice(&hud_rect(
            rect[0],
            rect[1],
            rect[2],
            rect[3],
            HOTBAR_SLOT_COLOR,
        ));
    }
    for (slot, (rect, _)) in hotbar.iter().enumerate() {
        let Some(state) = slots[slot].as_ref() else {
            continue;
        };
        let inset_x = rect[2] * HOTBAR_SWATCH_INSET;
        let inset_y = rect[3] * HOTBAR_SWATCH_INSET;
        hud.extend_from_slice(&hud_rect(
            rect[0] + inset_x,
            rect[1] + inset_y,
            rect[2] - inset_x * 2.0,
            rect[3] - inset_y * 2.0,
            block_swatch_color(&state.name),
        ));
    }
    // The brighter overlay goes last so it also tints the item swatch.
    if let Some((rect, _)) = hotbar.iter().find(|(_, selected)| *selected) {
        hud.extend_from_slice(&hud_rect(
            rect[0],
            rect[1],
            rect[2],
            rect[3],
            HOTBAR_SELECTED_COLOR,
        ));
    }
    hud.extend_from_slice(&crosshair_vertices(width, height));
}

/// Swatch color for one hotbar block, keyed by its name without the
/// `minecraft:` namespace. Unknown blocks use a neutral gray.
fn block_swatch_color(name: &str) -> [f32; 4] {
    match name.strip_prefix("minecraft:").unwrap_or(name) {
        "stone" => [0.50, 0.50, 0.50, 1.0],
        "dirt" => [0.55, 0.40, 0.24, 1.0],
        "grass_block" => [0.34, 0.62, 0.30, 1.0],
        "oak_planks" => [0.72, 0.60, 0.40, 1.0],
        "oak_log" => [0.42, 0.32, 0.19, 1.0],
        "glass" => [0.72, 0.86, 0.96, 1.0],
        "bricks" => [0.62, 0.24, 0.20, 1.0],
        "stone_bricks" => [0.45, 0.45, 0.45, 1.0],
        "white_wool" => [0.93, 0.93, 0.93, 1.0],
        _ => [0.65, 0.65, 0.65, 1.0],
    }
}

/// Two thin quads forming a crosshair centred on the screen. Sizes are
/// converted from pixels so the crosshair keeps its physical size at every
/// resolution.
fn crosshair_vertices(width: u32, height: u32) -> [HudVertex; 12] {
    let width = width.max(1) as f32;
    let height = height.max(1) as f32;
    // On a tiny surface a fixed pixel size would overflow the screen; cap the
    // arms and the thickness to a quarter/half of it.
    let arm_x = (CROSSHAIR_ARM / width).min(0.25);
    let arm_y = (CROSSHAIR_ARM / height).min(0.25);
    let thick_x = (CROSSHAIR_THICKNESS / width).min(arm_x);
    let thick_y = (CROSSHAIR_THICKNESS / height).min(arm_y);
    let horizontal = hud_rect(
        0.5 - arm_x,
        0.5 - thick_y * 0.5,
        arm_x * 2.0,
        thick_y,
        CROSSHAIR_COLOR,
    );
    let vertical = hud_rect(
        0.5 - thick_x * 0.5,
        0.5 - arm_y,
        thick_x,
        arm_y * 2.0,
        CROSSHAIR_COLOR,
    );
    let mut vertices = [horizontal[0]; 12];
    vertices[..6].copy_from_slice(&horizontal);
    vertices[6..].copy_from_slice(&vertical);
    vertices
}

#[cfg(test)]
mod tests {
    use super::*;

    fn arg(text: &str) -> OsString {
        OsString::from(text)
    }

    fn base() -> Vec<OsString> {
        [
            "--world",
            "w",
            "--tables",
            "t",
            "--client-jar",
            "j",
            "--assets-index",
            "i",
            "--assets-objects",
            "o",
        ]
        .iter()
        .map(|text| arg(text))
        .collect()
    }

    #[test]
    fn defaults() {
        let options = parse(base()).unwrap();
        assert_eq!(options.world, PathBuf::from("w"));
        assert_eq!(options.tables, PathBuf::from("t"));
        assert_eq!(options.client_jar, PathBuf::from("j"));
        assert_eq!(options.assets_index, PathBuf::from("i"));
        assert_eq!(options.assets_objects, PathBuf::from("o"));
        assert_eq!(options.width, 1280);
        assert_eq!(options.height, 720);
        assert_eq!(options.spawn, None);
        assert_eq!(options.radius, 4);
        assert_eq!(options.frames, 0);
    }

    #[test]
    fn optional_arguments() {
        let mut args = base();
        args.extend(
            [
                "--spawn", "10", "64.5", "-3", "--width", "800", "--height", "600", "--radius",
                "8", "--frames", "5",
            ]
            .iter()
            .map(|text| arg(text)),
        );
        let options = parse(args).unwrap();
        assert_eq!(options.spawn, Some([10.0, 64.5, -3.0]));
        assert_eq!((options.width, options.height), (800, 600));
        assert_eq!(options.radius, 8);
        assert_eq!(options.frames, 5);
    }

    #[test]
    fn unknown_argument_is_rejected() {
        let mut args = base();
        args.push(arg("--turbo"));
        let error = parse(args).unwrap_err();
        assert!(error.contains("--turbo"), "{error}");
    }

    #[test]
    fn positional_argument_is_rejected() {
        let mut args = base();
        args.push(arg("stray"));
        let error = parse(args).unwrap_err();
        assert!(error.contains("stray"), "{error}");
    }

    #[test]
    fn missing_required_argument_is_reported() {
        let error = parse([arg("--world"), arg("w")]).unwrap_err();
        assert!(error.contains("--tables"), "{error}");
        assert!(error.contains("--assets-objects"), "{error}");
    }

    #[test]
    fn invalid_number_is_rejected() {
        let mut args = base();
        args.extend([arg("--width"), arg("wide")]);
        let error = parse(args).unwrap_err();
        assert!(error.contains("--width"), "{error}");
    }

    #[test]
    fn missing_value_is_rejected() {
        let mut args = base();
        args.push(arg("--radius"));
        assert!(parse(args).is_err());
    }

    #[test]
    fn incomplete_spawn_is_rejected() {
        let mut args = base();
        args.extend([arg("--spawn"), arg("1"), arg("2")]);
        assert!(parse(args).is_err());
    }

    #[test]
    fn zero_size_is_rejected() {
        let mut args = base();
        args.extend([arg("--width"), arg("0")]);
        assert!(parse(args).is_err());
    }

    #[test]
    fn zero_radius_is_rejected() {
        let mut args = base();
        args.extend([arg("--radius"), arg("0")]);
        assert!(parse(args).is_err());
    }

    #[test]
    fn chunk_of_negative_and_positive_positions() {
        let player = Player {
            position: [33.0, 64.0, -17.0],
            velocity: [0.0; 3],
            yaw: 0.0,
            pitch: 0.0,
            on_ground: true,
            flying: false,
            sneaking: false,
        };
        assert_eq!(player_chunk(&player), [2, -2]);
        let player = Player {
            position: [-0.5, 64.0, 0.5],
            ..player
        };
        assert_eq!(player_chunk(&player), [-1, 0]);
    }

    #[test]
    fn digits_map_to_hotbar_slots() {
        assert_eq!(hotbar_slot(KeyCode::Digit1), Some(0));
        assert_eq!(hotbar_slot(KeyCode::Digit5), Some(4));
        assert_eq!(hotbar_slot(KeyCode::Digit9), Some(8));
        assert_eq!(hotbar_slot(KeyCode::Digit0), None);
        assert_eq!(hotbar_slot(KeyCode::KeyW), None);
    }

    #[test]
    fn wheel_sign_follows_direction() {
        assert_eq!(wheel_steps(1.0), 1);
        assert_eq!(wheel_steps(0.25), 1);
        assert_eq!(wheel_steps(0.0), 0);
        assert_eq!(wheel_steps(-0.5), -1);
    }

    #[test]
    fn movement_merges_keyboard_and_touch_with_or() {
        let keyboard = Input {
            forward: true,
            sprint: true,
            ..Input::default()
        };
        let touch = Input {
            forward: true,
            left: true,
            jump: true,
            ..Input::default()
        };
        let merged = merge_movement(keyboard, touch);
        assert!(merged.forward);
        assert!(merged.left);
        assert!(merged.jump);
        assert!(merged.sprint);
        assert!(!merged.back);
        assert!(!merged.right);
        assert!(!merged.sneak);
        assert_eq!(
            merge_movement(Input::default(), Input::default()),
            Input::default()
        );
    }

    #[test]
    fn touch_look_is_relative_to_the_current_look() {
        // yaw -= delta[0], pitch += delta[1].
        assert_eq!(
            merged_look(Some([10.0, 5.0]), [-5.0, 3.0], [0.0, 0.0]),
            Some([15.0, 8.0])
        );
        // Without a pending mouse look the player's own look is the base.
        assert_eq!(
            merged_look(None, [20.0, -10.0], [30.0, 2.0]),
            Some([10.0, -8.0])
        );
        // A zero delta leaves the pending look (including `None`) alone.
        assert_eq!(
            merged_look(Some([1.0, 2.0]), [0.0, 0.0], [9.0, 9.0]),
            Some([1.0, 2.0])
        );
        assert_eq!(merged_look(None, [0.0, 0.0], [9.0, 9.0]), None);
    }

    #[test]
    fn look_delta_wraps_yaw_and_clamps_pitch() {
        assert_eq!(apply_look_delta([179.0, 0.0], [-5.0, 0.0]), [-176.0, 0.0]);
        assert_eq!(apply_look_delta([-179.0, 0.0], [5.0, 0.0]), [176.0, 0.0]);
        assert_eq!(apply_look_delta([0.0, 85.0], [0.0, 10.0]), [0.0, 90.0]);
        assert_eq!(apply_look_delta([0.0, -85.0], [0.0, -10.0]), [0.0, -90.0]);
    }

    #[test]
    fn touch_hud_is_hidden_until_touch_is_active() {
        let mut touch = TouchControls::new();
        touch.resize(800.0, 600.0);
        let slots: [Option<BlockState>; 9] = std::array::from_fn(|_| None);
        let mut hud = Vec::new();
        fill_hud(&mut hud, &touch, &slots, false, 800, 600);
        assert!(hud.is_empty());
        fill_hud(&mut hud, &touch, &slots, true, 800, 600);
        // The four touch rectangles, nine hotbar backgrounds, the selected
        // overlay and the two-quad crosshair.
        assert_eq!(hud.len(), touch.hud_rects().len() * 6 + 9 * 6 + 6 + 12);
        for vertex in &hud {
            assert!(vertex.position[0].is_finite());
            assert!(vertex.position[1].is_finite());
            assert!((-1.0..=1.0).contains(&vertex.position[0]));
            assert!((-1.0..=1.0).contains(&vertex.position[1]));
        }
        // The buffer is reused, not appended to, on the next frame.
        fill_hud(&mut hud, &touch, &slots, false, 800, 600);
        assert!(hud.is_empty());
    }

    #[test]
    fn hotbar_swatch_colors_cover_the_creative_slots() {
        let expected = [
            ("minecraft:stone", [0.50, 0.50, 0.50, 1.0]),
            ("minecraft:dirt", [0.55, 0.40, 0.24, 1.0]),
            ("minecraft:grass_block", [0.34, 0.62, 0.30, 1.0]),
            ("minecraft:oak_planks", [0.72, 0.60, 0.40, 1.0]),
            ("minecraft:oak_log", [0.42, 0.32, 0.19, 1.0]),
            ("minecraft:glass", [0.72, 0.86, 0.96, 1.0]),
            ("minecraft:bricks", [0.62, 0.24, 0.20, 1.0]),
            ("minecraft:stone_bricks", [0.45, 0.45, 0.45, 1.0]),
            ("minecraft:white_wool", [0.93, 0.93, 0.93, 1.0]),
        ];
        for (name, color) in expected {
            assert_eq!(block_swatch_color(name), color, "{name}");
            // The namespace is optional.
            assert_eq!(
                block_swatch_color(name.trim_start_matches("minecraft:")),
                color,
                "{name}"
            );
        }
        // Every creative slot maps to its own swatch, and unknown blocks get
        // a neutral gray instead of disappearing.
        let inventory = crate::inventory::Inventory::creative();
        let colors: Vec<[f32; 4]> = inventory
            .slots()
            .iter()
            .map(|slot| block_swatch_color(&slot.as_ref().unwrap().name))
            .collect();
        for (i, a) in colors.iter().enumerate() {
            for b in &colors[i + 1..] {
                assert_ne!(a, b, "creative slots must be distinguishable");
            }
        }
        assert_eq!(
            block_swatch_color("minecraft:emerald_block"),
            [0.65, 0.65, 0.65, 1.0]
        );
    }

    #[test]
    fn keyboard_and_touch_selection_merge_into_one_pick() {
        assert_eq!(merge_select(Some(2), None), Some(2));
        assert_eq!(merge_select(None, Some(7)), Some(7));
        // A digit pressed in the same tick wins, so only one pick is applied.
        assert_eq!(merge_select(Some(2), Some(7)), Some(2));
        assert_eq!(merge_select(None, None), None);
    }

    #[test]
    fn save_timer_fires_every_thirty_seconds() {
        let start = Instant::now();
        assert!(!save_due(start, start));
        assert!(!save_due(
            start,
            start + Duration::from_secs(29) + Duration::from_millis(999)
        ));
        assert!(save_due(start, start + SAVE_INTERVAL));
        assert!(save_due(start, start + Duration::from_secs(75)));
        // A clock that jumps backwards never reports a save as due.
        assert!(!save_due(start, start - Duration::from_secs(10)));
    }

    #[test]
    fn hud_draws_hotbar_swatches_and_a_brighter_selection() {
        let mut touch = TouchControls::new();
        touch.resize(800.0, 600.0);
        touch.set_selected(3);
        let mut slots: [Option<BlockState>; 9] = std::array::from_fn(|_| None);
        slots[3] = Some(BlockState::new("minecraft:grass_block"));
        let mut hud = Vec::new();
        fill_hud(&mut hud, &touch, &slots, true, 800, 600);

        // Four controls, nine backgrounds and one swatch, then the selection.
        let expected = 4 * 6 + 9 * 6 + 6 + 6 + 12;
        assert_eq!(hud.len(), expected);
        let selected = hud
            .iter()
            .filter(|vertex| vertex.color == HOTBAR_SELECTED_COLOR)
            .count();
        assert_eq!(selected, 6, "only the selected slot gets the overlay");
        let swatch = hud
            .iter()
            .filter(|vertex| vertex.color == block_swatch_color("minecraft:grass_block"))
            .count();
        assert_eq!(swatch, 6);

        // Moving the selection moves the brighter overlay: the six selected
        // vertices sit inside the new slot's rect.
        touch.set_selected(7);
        fill_hud(&mut hud, &touch, &slots, true, 800, 600);
        let rect = touch.hotbar_rects()[7].0;
        let (left, top) = (rect[0] * 2.0 - 1.0, 1.0 - rect[1] * 2.0);
        let (right, bottom) = (
            (rect[0] + rect[2]) * 2.0 - 1.0,
            1.0 - (rect[1] + rect[3]) * 2.0,
        );
        for vertex in hud
            .iter()
            .filter(|vertex| vertex.color == HOTBAR_SELECTED_COLOR)
        {
            assert!((-1.0..=1.0).contains(&vertex.position[0]));
            assert!((-1.0..=1.0).contains(&vertex.position[1]));
            assert!(vertex.position[0] >= left - 1e-5 && vertex.position[0] <= right + 1e-5);
            assert!(vertex.position[1] >= bottom - 1e-5 && vertex.position[1] <= top + 1e-5);
        }
    }

    #[test]
    fn crosshair_stays_inside_clip_space_at_tiny_sizes() {
        for (width, height) in [(1, 1), (16, 9), (3840, 2160)] {
            let crosshair = crosshair_vertices(width, height);
            for vertex in crosshair {
                assert!(vertex.position[0].is_finite());
                assert!(vertex.position[1].is_finite());
                assert!((-1.0..=1.0).contains(&vertex.position[0]));
                assert!((-1.0..=1.0).contains(&vertex.position[1]));
            }
        }
    }
}

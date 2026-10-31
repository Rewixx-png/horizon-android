//! Minecraft 1.16.5 (protocol 754) connection primitives for the second
//! engine target. This is the M1 network slice only: status ping, offline-mode
//! login, compression and the play-state keep-alive exchange. It deliberately
//! does not reuse the 26.3 packet enums or packet ids.
//!
//! Scope: a local/offline 1.16.5 server. Online-mode encryption and the
//! rest of the 1.16.5 play packet set are the next milestones; beyond
//! keep-alive this slice also parses the movement, chat and disconnect
//! packets and encodes Teleport Confirm.

use crate::{
    Error, Result,
    buf::{Reader, Writer},
    connection::Connection,
};
use serde_json::Value;
use std::collections::HashMap;
use std::path::Path;
use std::time::{Duration, Instant};

pub const PROTOCOL_VERSION: i32 = 754;
pub const GAME_VERSION: &str = "1.16.5";

/// Handshaking/serverbound ClientIntentionPacket.
pub const HANDSHAKE: i32 = 0x00;
/// Status/serverbound StatusRequestPacket and clientbound StatusResponsePacket.
pub const STATUS_REQUEST: i32 = 0x00;
pub const STATUS_RESPONSE: i32 = 0x00;
/// Status/serverbound PingRequestPacket and clientbound PongResponsePacket.
pub const PING_REQUEST: i32 = 0x01;
pub const PONG_RESPONSE: i32 = 0x01;
/// Login/clientbound.
pub const LOGIN_DISCONNECT: i32 = 0x00;
pub const LOGIN_ENCRYPTION: i32 = 0x01;
pub const LOGIN_SUCCESS: i32 = 0x02;
pub const LOGIN_COMPRESSION: i32 = 0x03;
pub const LOGIN_PLUGIN_REQUEST: i32 = 0x04;
/// Login/serverbound.
pub const LOGIN_START: i32 = 0x00;
pub const LOGIN_PLUGIN_RESPONSE: i32 = 0x02;
/// Play/packet ids used by this slice. 1.16.5 does not have the 26.3
/// packet set; these ids come from the 1.16 protocol family.
pub const PLAY_JOIN_GAME: i32 = 0x24;
pub const PLAY_CHUNK_DATA: i32 = 0x20;
pub const PLAY_KEEP_ALIVE: i32 = 0x1F;
pub const PLAY_KEEP_ALIVE_RESPONSE: i32 = 0x10;
pub const PLAY_DISCONNECT: i32 = 0x19;
/// Play/clientbound Chat Message.
pub const PLAY_CHAT_MESSAGE: i32 = 0x0E;
/// Play/clientbound Player Position And Look.
pub const PLAY_PLAYER_POSITION_AND_LOOK: i32 = 0x34;
/// Play/serverbound Teleport Confirm.
pub const PLAY_TELEPORT_CONFIRM: i32 = 0x00;

pub const STATUS: i32 = 1;
pub const LOGIN: i32 = 2;

fn packet(id: i32, payload: &[u8]) -> Vec<u8> {
    let mut body = Writer::new();
    body.varint(id);
    body.bytes(payload);
    body.data
}

fn send(connection: &mut Connection, id: i32, payload: &[u8]) -> Result<()> {
    connection.send(&packet(id, payload))
}

fn handshake_payload(host: &str, port: u16, next: i32) -> Vec<u8> {
    let mut body = Writer::new();
    body.varint(PROTOCOL_VERSION);
    body.string(host);
    body.u16(port);
    body.varint(next);
    body.data
}

fn string_payload(value: &str) -> Vec<u8> {
    let mut body = Writer::new();
    body.string(value);
    body.data
}

fn read_string(payload: &[u8], max: usize) -> Result<(String, &[u8])> {
    let mut reader = Reader::new(payload);
    let value = reader.string(max)?;
    Ok((value, reader.rest()))
}

/// 1.16.5 packets carry named NBT (type, name, payload), unlike the unnamed
/// network NBT of 26.3. Reading it with the modern helper would stop at the
/// first EMPTY name, so this slice has its own cursor advance.
fn read_nbt(reader: &mut Reader<'_>) -> Result<horizon_world::nbt::Tag> {
    let limits = horizon_world::nbt::Limits {
        max_bytes: 2_097_152,
        ..horizon_world::nbt::Limits::default()
    };
    let (named, used) = horizon_world::nbt::read_named(reader.remaining_slice(), limits)
        .map_err(|error| Error::new(format!("1.16.5 NBT: {error}")))?;
    reader.bytes(used)?;
    Ok(named.tag)
}

fn connect(host: &str, port: u16, timeout: Duration) -> Result<Connection> {
    let connection = Connection::connect(host, port, timeout)?;
    connection.reader.set_read_timeout(Some(timeout))?;
    Ok(connection)
}

/// A parsed `StatusResponsePacket`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Status {
    pub json: String,
    pub version_name: Option<String>,
    pub protocol: Option<i32>,
    pub players_online: Option<u64>,
    pub players_max: Option<u64>,
    pub description: Option<String>,
}

impl Status {
    pub fn parse(json: String) -> Result<Self> {
        let value: Value = serde_json::from_str(&json)
            .map_err(|error| Error::new(format!("1.16.5 status JSON: {error}")))?;
        let text = |value: Option<&Value>| value.and_then(Value::as_str).map(str::to_owned);
        Ok(Self {
            json,
            version_name: text(value.get("version").and_then(|v| v.get("name"))),
            protocol: value
                .get("version")
                .and_then(|v| v.get("protocol"))
                .and_then(Value::as_i64)
                .and_then(|v| i32::try_from(v).ok()),
            players_online: value
                .get("players")
                .and_then(|p| p.get("online"))
                .and_then(Value::as_u64),
            players_max: value
                .get("players")
                .and_then(|p| p.get("max"))
                .and_then(Value::as_u64),
            description: text(value.get("description")),
        })
    }
}

/// Server list ping through a fresh connection. The request uses protocol 754;
/// the server is free to answer with any version it implements.
pub fn status(host: &str, port: u16, timeout: Duration) -> Result<Status> {
    let mut connection = connect(host, port, timeout)?;
    send(
        &mut connection,
        HANDSHAKE,
        &handshake_payload(host, port, STATUS),
    )?;
    send(&mut connection, STATUS_REQUEST, &[])?;
    let (id, payload) = connection.recv()?;
    if id != STATUS_RESPONSE {
        return Err(Error::new(format!(
            "1.16.5 status: expected StatusResponse, got packet {id:#x}"
        )));
    }
    let (json, rest) = read_string(&payload, 32_767)?;
    if !rest.is_empty() {
        return Err(Error::new("1.16.5 status response has trailing bytes"));
    }
    let status = Status::parse(json)?;
    send(&mut connection, PING_REQUEST, &0i64.to_be_bytes())?;
    let (id, payload) = connection.recv()?;
    if id != PONG_RESPONSE || payload != 0i64.to_be_bytes() {
        return Err(Error::new("1.16.5 status: invalid pong"));
    }
    Ok(status)
}

/// The clientbound Join Game packet, reduced to the fields the first world
/// slice needs. The dimension codec and dimension NBT are kept as parsed tags
/// for the later registry and dimension work.
#[derive(Clone, Debug, PartialEq)]
pub struct JoinGame {
    pub entity_id: i32,
    pub hardcore: bool,
    pub game_mode: u8,
    pub previous_game_mode: u8,
    pub world_names: Vec<String>,
    pub dimension_codec: horizon_world::nbt::Tag,
    pub dimension: horizon_world::nbt::Tag,
    pub world_name: String,
    pub hashed_seed: i64,
    pub max_players: i32,
    pub view_distance: i32,
    pub reduced_debug_info: bool,
    pub enable_respawn_screen: bool,
    pub debug: bool,
    pub flat: bool,
}

pub fn parse_join_game(payload: &[u8]) -> Result<JoinGame> {
    let mut reader = Reader::new(payload);
    let entity_id = reader.i32()?;
    let hardcore = reader.bool()?;
    let game_mode = reader.u8()?;
    let previous_game_mode = reader.u8()?;
    let count = reader.count(128)?;
    let mut world_names = Vec::with_capacity(count);
    for _ in 0..count {
        world_names.push(reader.string(32_767)?);
    }
    let dimension_codec = read_nbt(&mut reader)?;
    let dimension = read_nbt(&mut reader)?;
    let world_name = reader.string(32_767)?;
    let hashed_seed = reader.i64()?;
    let max_players = reader.varint()?;
    let view_distance = reader.varint()?;
    let join = JoinGame {
        entity_id,
        hardcore,
        game_mode,
        previous_game_mode,
        world_names,
        dimension_codec,
        dimension,
        world_name,
        hashed_seed,
        max_players,
        view_distance,
        reduced_debug_info: reader.bool()?,
        enable_respawn_screen: reader.bool()?,
        debug: reader.bool()?,
        flat: reader.bool()?,
    };
    if reader.remaining() != 0 {
        return Err(Error::new(format!(
            "1.16.5 Join Game has {} trailing bytes",
            reader.remaining()
        )));
    }
    Ok(join)
}

/// One 16×16×16 chunk section after the network palette is decoded. State ids
/// are the 1.16.5 global block-state ids from `tables.json`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChunkSection {
    pub block_count: u16,
    pub bits: u8,
    pub palette: Vec<i32>,
    pub data: Vec<i64>,
}

/// The clientbound Chunk Data packet. `sections` keeps each present section
/// with its index (0 is the bottom 16 blocks, 15 the top in 1.16.5).
#[derive(Clone, Debug, PartialEq)]
pub struct ChunkData {
    pub x: i32,
    pub z: i32,
    pub full: bool,
    pub bitmask: u32,
    pub heightmaps: horizon_world::nbt::Tag,
    pub biomes: Vec<i32>,
    pub sections: Vec<(usize, ChunkSection)>,
    pub block_entities: Vec<horizon_world::nbt::Tag>,
}

pub fn parse_chunk_data(payload: &[u8]) -> Result<ChunkData> {
    let mut reader = Reader::new(payload);
    let x = reader.i32()?;
    let z = reader.i32()?;
    let full = reader.bool()?;
    let bitmask = u32::try_from(reader.varint()?)
        .map_err(|_| Error::new("1.16.5 Chunk Data has a negative section mask"))?;
    if bitmask & !0xffff != 0 {
        return Err(Error::new(
            "1.16.5 Chunk Data section mask exceeds 16 sections",
        ));
    }
    let heightmaps = read_nbt(&mut reader)?;
    let mut biomes = Vec::new();
    if full {
        let count = reader.count(65_536)?;
        biomes.reserve(count);
        for _ in 0..count {
            biomes.push(reader.varint()?);
        }
    }
    let data = reader.byte_array(16 * 1024 * 1024)?;
    let mut data = Reader::new(&data);
    let mut sections = Vec::new();
    for index in 0..16 {
        if bitmask & (1 << index) == 0 {
            continue;
        }
        let block_count = data.u16()?;
        let bits = data.u8()?;
        if bits > 15 {
            return Err(Error::new(format!(
                "1.16.5 chunk section {index} has {bits} bits per block"
            )));
        }
        let mut palette = Vec::new();
        if bits <= 8 {
            let count = data.count(256)?;
            palette.reserve(count);
            for _ in 0..count {
                palette.push(data.varint()?);
            }
        }
        let long_count = data.count(16 * 1024 * 1024)?;
        let expected = if bits == 0 {
            0
        } else {
            (4096usize * usize::from(bits)).div_ceil(64)
        };
        if long_count != expected {
            return Err(Error::new(format!(
                "1.16.5 chunk section {index}: {long_count} longs, expected {expected}"
            )));
        }
        let mut longs = Vec::with_capacity(long_count);
        for _ in 0..long_count {
            longs.push(data.i64()?);
        }
        sections.push((
            index,
            ChunkSection {
                block_count,
                bits,
                palette,
                data: longs,
            },
        ));
    }
    if data.remaining() != 0 {
        return Err(Error::new(format!(
            "1.16.5 Chunk Data has {} trailing section bytes",
            data.remaining()
        )));
    }
    let count = reader.count(1_048_576)?;
    let mut block_entities = Vec::with_capacity(count);
    for _ in 0..count {
        block_entities.push(read_nbt(&mut reader)?);
    }
    if reader.remaining() != 0 {
        return Err(Error::new(format!(
            "1.16.5 Chunk Data has {} trailing bytes after block entities",
            reader.remaining()
        )));
    }
    Ok(ChunkData {
        x,
        z,
        full,
        bitmask,
        heightmaps,
        biomes,
        sections,
        block_entities,
    })
}

impl ChunkSection {
    /// The 1.16.5 global block-state id at section-local `x/y/z` (0..15).
    /// Paletted storage packs (y, z, x) in that order, least significant
    /// bits first; sections above the palette threshold hold raw state ids.
    pub fn block_state(&self, x: u8, y: u8, z: u8) -> Result<i32> {
        if x > 15 || y > 15 || z > 15 {
            return Err(Error::new("chunk section coordinates must be 0..15"));
        }
        if self.bits == 0 {
            return Ok(self.palette.first().copied().unwrap_or(0));
        }
        let index = usize::from(y) * 256 + usize::from(z) * 16 + usize::from(x);
        let per_long = 64 / usize::from(self.bits);
        let long = *self
            .data
            .get(index / per_long)
            .ok_or_else(|| Error::new("chunk section data is too short"))?;
        let shift = (index % per_long) * usize::from(self.bits);
        let mask = (1u64 << self.bits) - 1;
        let value = ((long as u64 >> shift) & mask) as usize;
        if self.bits <= 8 {
            self.palette
                .get(value)
                .copied()
                .ok_or_else(|| Error::new(format!("chunk palette index {value} is out of range")))
        } else {
            Ok(value as i32)
        }
    }
}

impl ChunkData {
    /// A block-state id at world-section coordinates; absent sections are air.
    pub fn block_state(&self, x: i32, y: i32, z: i32) -> Result<i32> {
        if !(0..256).contains(&y) {
            return Ok(0);
        }
        let lx = x.rem_euclid(16) as u8;
        let ly = (y & 15) as u8;
        let lz = z.rem_euclid(16) as u8;
        let index = (y >> 4) as usize;
        match self.sections.iter().find(|(section, _)| *section == index) {
            Some((_, section)) => section.block_state(lx, ly, lz),
            None => Ok(0),
        }
    }

    /// The biome id at a block position. 1.16.5 sends 4×4×4 biome cells:
    /// index = (y >> 2) * 16 + (z >> 2) * 4 + (x >> 2).
    pub fn biome(&self, x: i32, y: i32, z: i32) -> Option<i32> {
        if !(0..256).contains(&y) {
            return None;
        }
        let index = ((y >> 2) as usize) * 16
            + ((z.rem_euclid(16) >> 2) as usize) * 4
            + ((x.rem_euclid(16) >> 2) as usize);
        self.biomes.get(index).copied()
    }
}

/// Flag bits of a [`PositionAndLook`]: a set bit means the matching field is
/// a delta to apply to the current position or rotation, not an absolute
/// value. Bits verified against the archived wiki.vg protocol 754 page.
pub const POSITION_FLAG_X: u8 = 0x01;
pub const POSITION_FLAG_Y: u8 = 0x02;
pub const POSITION_FLAG_Z: u8 = 0x04;
pub const POSITION_FLAG_Y_ROT: u8 = 0x08;
pub const POSITION_FLAG_X_ROT: u8 = 0x10;

/// The clientbound Player Position And Look packet (0x34). `x`/`y`/`z` are
/// absolute (or relative, per the [`POSITION_FLAG_X`] bits) and `yaw`/`pitch`
/// are in degrees. The client must answer with a Teleport Confirm carrying
/// the same `teleport_id`.
#[derive(Clone, Debug, PartialEq)]
pub struct PositionAndLook {
    pub x: f64,
    pub y: f64,
    pub z: f64,
    pub yaw: f32,
    pub pitch: f32,
    pub flags: u8,
    pub teleport_id: i32,
}

pub fn parse_position_and_look(payload: &[u8]) -> Result<PositionAndLook> {
    let mut reader = Reader::new(payload);
    let position = PositionAndLook {
        x: reader.f64()?,
        y: reader.f64()?,
        z: reader.f64()?,
        yaw: reader.f32()?,
        pitch: reader.f32()?,
        flags: reader.u8()?,
        teleport_id: reader.varint()?,
    };
    if reader.remaining() != 0 {
        return Err(Error::new(format!(
            "1.16.5 Player Position And Look has {} trailing bytes",
            reader.remaining()
        )));
    }
    Ok(position)
}

/// Where a clientbound Chat Message is displayed (its `position` byte).
pub const CHAT_POSITION_CHAT: u8 = 0;
pub const CHAT_POSITION_SYSTEM: u8 = 1;
pub const CHAT_POSITION_GAME_INFO: u8 = 2;

/// The clientbound Chat Message packet (0x0E): a JSON chat component, the
/// HUD slot it belongs in, and the sender's UUID in network (big-endian)
/// order. In 1.16.5 the sender is always present; the optional-sender
/// variant is newer than protocol 754.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChatMessage {
    pub json: String,
    pub position: u8,
    pub sender: u128,
}

pub fn parse_chat_message(payload: &[u8]) -> Result<ChatMessage> {
    let mut reader = Reader::new(payload);
    let message = ChatMessage {
        json: reader.string(262_144)?,
        position: reader.u8()?,
        sender: reader.uuid()?,
    };
    if reader.remaining() != 0 {
        return Err(Error::new(format!(
            "1.16.5 Chat Message has {} trailing bytes",
            reader.remaining()
        )));
    }
    Ok(message)
}

/// The clientbound Disconnect (play) packet (0x19): the JSON reason shown
/// before the connection is closed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Disconnect {
    pub reason: String,
}

pub fn parse_disconnect(payload: &[u8]) -> Result<Disconnect> {
    let mut reader = Reader::new(payload);
    let reason = reader.string(262_144)?;
    if reader.remaining() != 0 {
        return Err(Error::new(format!(
            "1.16.5 Disconnect has {} trailing bytes",
            reader.remaining()
        )));
    }
    Ok(Disconnect { reason })
}

/// The serverbound Teleport Confirm packet (0x00), acknowledging a
/// [`PositionAndLook`] with the same teleport id.
pub fn teleport_confirm_payload(teleport_id: i32) -> Vec<u8> {
    let mut body = Writer::new();
    body.varint(teleport_id);
    body.data
}

/// The M0 block table, indexed by 1.16.5 global block-state id.
pub struct Tables {
    blocks: Vec<String>,
}

impl Tables {
    /// Load the private `tables.json` produced by `tools/prepare_1165.py`.
    pub fn load(path: &Path) -> Result<Self> {
        let bytes = std::fs::read(path)
            .map_err(|error| Error::new(format!("{}: {error}", path.display())))?;
        let json: Value = serde_json::from_slice(&bytes)
            .map_err(|error| Error::new(format!("{}: {error}", path.display())))?;
        if json.get("version").and_then(Value::as_str) != Some(GAME_VERSION) {
            return Err(Error::new("block table is not for Minecraft 1.16.5"));
        }
        let entries = json
            .get("blocks")
            .and_then(|blocks| blocks.get("entries"))
            .and_then(Value::as_object)
            .ok_or_else(|| Error::new("block table has no blocks.entries"))?;
        let mut blocks: Vec<String> = Vec::new();
        for (name, block) in entries {
            let states = block
                .get("states")
                .and_then(Value::as_array)
                .ok_or_else(|| Error::new(format!("block {name} has no states")))?;
            for state in states {
                let id = state
                    .get("id")
                    .and_then(Value::as_i64)
                    .and_then(|id| usize::try_from(id).ok())
                    .ok_or_else(|| Error::new(format!("block {name} has an invalid state id")))?;
                if id > 10_000_000 {
                    return Err(Error::new("block state id is unreasonably large"));
                }
                if blocks.len() <= id {
                    blocks.resize(id + 1, String::new());
                }
                if !blocks[id].is_empty() {
                    return Err(Error::new(format!("duplicate block state id {id}")));
                }
                blocks[id] = name.clone();
            }
        }
        if blocks.is_empty() {
            return Err(Error::new("block table is empty"));
        }
        Ok(Self { blocks })
    }

    /// The namespaced block name for a 1.16.5 global state id.
    pub fn block(&self, state: i32) -> Option<&str> {
        usize::try_from(state)
            .ok()
            .and_then(|state| self.blocks.get(state))
            .map(String::as_str)
            .filter(|name| !name.is_empty())
    }

    /// Number of state ids the table can resolve.
    pub fn state_count(&self) -> usize {
        self.blocks.len()
    }
}

/// A loaded set of 1.16.5 chunk columns, keyed by their chunk coordinates.
pub struct World {
    tables: Tables,
    chunks: HashMap<(i32, i32), ChunkData>,
}

impl World {
    pub fn new(tables: Tables) -> Self {
        Self {
            tables,
            chunks: HashMap::new(),
        }
    }

    pub fn load(path: &Path) -> Result<Self> {
        Ok(Self::new(Tables::load(path)?))
    }

    pub fn apply_chunk(&mut self, chunk: ChunkData) {
        self.chunks.insert((chunk.x, chunk.z), chunk);
    }

    pub fn chunk(&self, x: i32, z: i32) -> Option<&ChunkData> {
        self.chunks.get(&(x, z))
    }

    /// The block name at a world position, or `None` when the chunk is not
    /// loaded. Missing sections inside a loaded 1.16.5 chunk are air.
    pub fn block(&self, x: i32, y: i32, z: i32) -> Option<&str> {
        if !(0..256).contains(&y) {
            return None;
        }
        let state = self.chunk(x >> 4, z >> 4)?.block_state(x, y, z).ok()?;
        self.tables.block(state)
    }

    pub fn biome(&self, x: i32, y: i32, z: i32) -> Option<i32> {
        self.chunk(x >> 4, z >> 4)?.biome(x, y, z)
    }

    pub fn tables(&self) -> &Tables {
        &self.tables
    }
}

/// The profile the server assigned to an offline login.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Profile {
    pub uuid: [u8; 16],
    pub name: String,
}

/// A logged-in 1.16.5 connection in the play state.
pub struct Client {
    pub connection: Connection,
    pub profile: Profile,
    /// The server's compression threshold, after Set Compression.
    pub threshold: Option<i32>,
}

impl Client {
    pub fn recv(&mut self) -> Result<(i32, Vec<u8>)> {
        self.connection.recv()
    }

    pub fn send(&mut self, id: i32, payload: &[u8]) -> Result<()> {
        send(&mut self.connection, id, payload)
    }

    /// Echo a clientbound Keep Alive and return whether this packet was one.
    pub fn answer_keep_alive(&mut self, id: i32, payload: &[u8]) -> Result<bool> {
        if id != PLAY_KEEP_ALIVE {
            return Ok(false);
        }
        if payload.len() != 8 {
            return Err(Error::new(format!(
                "1.16.5 keep-alive payload is {} bytes, expected 8",
                payload.len()
            )));
        }
        self.send(PLAY_KEEP_ALIVE_RESPONSE, payload)?;
        Ok(true)
    }

    /// Acknowledge a clientbound Player Position And Look with a Teleport
    /// Confirm carrying the same teleport id.
    pub fn send_teleport_confirm(&mut self, teleport_id: i32) -> Result<()> {
        self.send(
            PLAY_TELEPORT_CONFIRM,
            &teleport_confirm_payload(teleport_id),
        )
    }

    /// Read play packets until `duration` elapses, answering keep-alives. This
    /// is the M1 hold-a-connection probe; M2 replaces it with the play state.
    pub fn hold(&mut self, duration: Duration) -> Result<Hold> {
        let deadline = Instant::now() + duration;
        let mut hold = Hold::default();
        self.connection
            .reader
            .set_read_timeout(Some(Duration::from_millis(250)))?;
        while Instant::now() < deadline {
            match self.recv() {
                Ok((id, payload)) => {
                    hold.received += 1;
                    if self.answer_keep_alive(id, &payload)? {
                        hold.keep_alives += 1;
                    } else if id == PLAY_DISCONNECT {
                        let reason = parse_disconnect(&payload)?.reason;
                        return Err(Error::new(format!("1.16.5 server disconnected: {reason}")));
                    }
                }
                Err(error) if error.is_timed_out() => {}
                Err(error) => return Err(error),
            }
        }
        Ok(hold)
    }
}

/// Counters for one [`Client::hold`] window.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Hold {
    pub received: usize,
    pub keep_alives: usize,
}

/// Login to an offline-mode 1.16.5 server, with compression and keep-alive
/// handling. Online-mode encryption is rejected with a clear error instead of
/// being misreported as a successful login.
pub fn login_offline(host: &str, port: u16, name: &str, timeout: Duration) -> Result<Client> {
    login_on(connect(host, port, timeout)?, host, port, name)
}

/// Login over an already connected socket or in-memory pipe. Kept public so
/// protocol tests and future M2 transports share the same state machine.
pub fn login_on(mut connection: Connection, host: &str, port: u16, name: &str) -> Result<Client> {
    send(
        &mut connection,
        HANDSHAKE,
        &handshake_payload(host, port, LOGIN),
    )?;
    send(&mut connection, LOGIN_START, &string_payload(name))?;
    let mut threshold = None;
    loop {
        let (id, payload) = connection.recv()?;
        match id {
            LOGIN_DISCONNECT => {
                let (reason, _) = read_string(&payload, 32_767)?;
                return Err(Error::new(format!("1.16.5 login rejected: {reason}")));
            }
            LOGIN_ENCRYPTION => {
                return Err(Error::new(
                    "1.16.5 online-mode encryption is not implemented yet; use an offline-mode server",
                ));
            }
            LOGIN_COMPRESSION => {
                let mut reader = Reader::new(&payload);
                let value = reader.varint()?;
                if reader.remaining() != 0 {
                    return Err(Error::new("1.16.5 Set Compression has trailing bytes"));
                }
                connection.set_compression(value);
                threshold = Some(value);
            }
            LOGIN_PLUGIN_REQUEST => {
                let mut reader = Reader::new(&payload);
                let transaction = reader.varint()?;
                let _channel = reader.string(32_767)?;
                // Vanilla login plugins are optional; decline without data.
                let mut answer = Writer::new();
                answer.varint(transaction);
                answer.bool(false);
                send(&mut connection, LOGIN_PLUGIN_RESPONSE, &answer.data)?;
            }
            LOGIN_SUCCESS => {
                if payload.len() < 16 {
                    return Err(Error::new("1.16.5 Login Success is missing its UUID"));
                }
                let mut uuid = [0u8; 16];
                uuid.copy_from_slice(&payload[..16]);
                let (name, rest) = read_string(&payload[16..], 16)?;
                if !rest.is_empty() {
                    return Err(Error::new("1.16.5 Login Success has trailing bytes"));
                }
                return Ok(Client {
                    connection,
                    profile: Profile { uuid, name },
                    threshold,
                });
            }
            other => {
                return Err(Error::new(format!(
                    "1.16.5 login: unexpected packet {other:#x}"
                )));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connection::Pipe;
    use std::thread;

    #[test]
    fn handshake_payload_is_protocol_754() {
        let payload = handshake_payload("localhost", 25566, LOGIN);
        let mut reader = Reader::new(&payload);
        assert_eq!(reader.varint().unwrap(), 754);
        assert_eq!(reader.string(255).unwrap(), "localhost");
        assert_eq!(reader.u16().unwrap(), 25566);
        assert_eq!(reader.varint().unwrap(), LOGIN);
        assert_eq!(reader.remaining(), 0);
    }

    #[test]
    fn status_json_is_parsed_without_losing_the_raw_text() {
        let json = r#"{"version":{"name":"1.16.5","protocol":754},"players":{"max":20,"online":3},"description":{"text":"hi"}}"#;
        let status = Status::parse(json.into()).unwrap();
        assert_eq!(status.version_name.as_deref(), Some("1.16.5"));
        assert_eq!(status.protocol, Some(754));
        assert_eq!(
            (status.players_online, status.players_max),
            (Some(3), Some(20))
        );
        assert_eq!(status.json, json);
    }

    #[test]
    fn offline_login_compression_and_keep_alive_over_a_pipe() {
        let (client_pipe, server_pipe) = Pipe::pair();
        let server = thread::spawn(move || {
            let mut connection = Connection::local(server_pipe);
            let (id, payload) = connection.recv().unwrap();
            assert_eq!(id, HANDSHAKE);
            let mut reader = Reader::new(&payload);
            assert_eq!(reader.varint().unwrap(), 754);
            assert_eq!(reader.string(255).unwrap(), "localhost");
            assert_eq!(reader.u16().unwrap(), 25565);
            assert_eq!(reader.varint().unwrap(), LOGIN);
            let (id, payload) = connection.recv().unwrap();
            assert_eq!(id, LOGIN_START);
            assert_eq!(read_string(&payload, 16).unwrap().0, "TestBot");

            // Set Compression itself is uncompressed; later packets use it.
            let mut compression = Writer::new();
            compression.varint(16);
            send(&mut connection, LOGIN_COMPRESSION, &compression.data).unwrap();
            connection.set_compression(16);
            let mut success = Writer::new();
            success.bytes(&[7; 16]);
            success.string("TestBot");
            send(&mut connection, LOGIN_SUCCESS, &success.data).unwrap();
            let tick = 1234_i64;
            send(&mut connection, PLAY_KEEP_ALIVE, &tick.to_be_bytes()).unwrap();
            let (id, payload) = connection.recv().unwrap();
            assert_eq!(id, PLAY_KEEP_ALIVE_RESPONSE);
            assert_eq!(payload, tick.to_be_bytes());
        });
        let mut client = login_on(
            Connection::local(client_pipe),
            "localhost",
            25565,
            "TestBot",
        )
        .unwrap();
        assert_eq!(client.profile.name, "TestBot");
        assert_eq!(client.profile.uuid, [7; 16]);
        assert_eq!(client.threshold, Some(16));
        let (id, payload) = client.recv().unwrap();
        assert_eq!(id, PLAY_KEEP_ALIVE);
        assert!(client.answer_keep_alive(id, &payload).unwrap());
        server.join().unwrap();
    }

    #[test]
    #[ignore = "needs an offline-mode 1.16.5 server; set HORIZON_1165_SERVER"]
    fn live_status_login_and_hold() {
        let address =
            std::env::var("HORIZON_1165_SERVER").unwrap_or_else(|_| "127.0.0.1:25566".into());
        let (host, port) = address.rsplit_once(':').unwrap();
        let host: &str = host;
        let port: u16 = port.parse().unwrap();
        let status = status(host, port, Duration::from_secs(5)).unwrap();
        assert_eq!(status.protocol, Some(PROTOCOL_VERSION));
        assert_eq!(status.version_name.as_deref(), Some(GAME_VERSION));
        let mut client = login_offline(host, port, "HorizonBot", Duration::from_secs(5)).unwrap();
        assert_eq!(client.profile.name, "HorizonBot");
        let mut join = None;
        let mut chunk = None;
        for _ in 0..1000 {
            let (id, payload) = client.recv().unwrap();
            match id {
                PLAY_JOIN_GAME if join.is_none() => join = Some(parse_join_game(&payload).unwrap()),
                PLAY_CHUNK_DATA if chunk.is_none() => {
                    chunk = Some(parse_chunk_data(&payload).unwrap())
                }
                PLAY_DISCONNECT => panic!("server disconnected before the first chunk"),
                _ => {}
            }
            if join.is_some() && chunk.is_some() {
                break;
            }
        }
        let join = join.expect("no Join Game");
        let chunk = chunk.expect("no Chunk Data");
        assert!(
            join.world_names
                .iter()
                .any(|name| name == "minecraft:overworld"),
            "missing overworld in {:?}",
            join.world_names
        );
        assert!(chunk.full, "first chunk was not ground-up");
        assert!(!chunk.sections.is_empty(), "first chunk had no sections");
        assert_eq!(chunk.biomes.len(), 1024);
        if let Ok(path) = std::env::var("HORIZON_1165_TABLES") {
            let mut world = World::load(Path::new(&path)).unwrap();
            world.apply_chunk(chunk.clone());
            let name = world
                .block(chunk.x * 16, 64, chunk.z * 16)
                .expect("the first chunk must resolve through the M0 tables");
            eprintln!("first chunk block at (x*16, 64, z*16): {name}");
        }
        let held = client.hold(Duration::from_secs(17)).unwrap();
        assert!(held.received > 0, "no play packets");
        assert!(held.keep_alives > 0, "no keep-alive answered");
    }

    #[test]
    fn chunk_data_sections_biomes_and_block_entities_are_decoded() {
        let heightmaps = horizon_world::nbt::write_named(
            &horizon_world::nbt::NamedTag {
                name: "".into(),
                tag: horizon_world::nbt::Tag::Compound(Default::default()),
            },
            horizon_world::nbt::Limits::default(),
        )
        .unwrap();
        let mut section = Writer::new();
        section.u16(1); // one solid block
        section.u8(4);
        section.varint(1);
        section.varint(0);
        section.varint(256);
        for _ in 0..256 {
            section.i64(0);
        }
        let mut packet = Writer::new();
        packet.i32(2);
        packet.i32(-3);
        packet.bool(true);
        packet.varint(1);
        packet.bytes(&heightmaps);
        packet.varint(1024);
        for _ in 0..1024 {
            packet.varint(1);
        }
        packet.byte_array(&section.data);
        packet.varint(0);
        let chunk = parse_chunk_data(&packet.data).unwrap();
        assert_eq!((chunk.x, chunk.z), (2, -3));
        assert!(chunk.full);
        assert_eq!(chunk.bitmask, 1);
        assert_eq!(chunk.biomes.len(), 1024);
        assert_eq!(chunk.sections.len(), 1);
        let (index, section) = &chunk.sections[0];
        assert_eq!(*index, 0);
        assert_eq!(section.block_count, 1);
        assert_eq!(section.bits, 4);
        assert_eq!(section.palette, [0]);
        assert_eq!(section.data.len(), 256);
        assert!(chunk.block_entities.is_empty());
    }

    #[test]
    fn packed_palette_and_biome_lookups_follow_1_16_5_order() {
        let mut data = vec![0i64; 256];
        let index = 2 * 256 + 3 * 16 + 1; // y=2, z=3, x=1
        data[index / 16] = 1i64 << ((index % 16) * 4);
        let section = ChunkSection {
            block_count: 1,
            bits: 4,
            palette: vec![0, 5],
            data,
        };
        assert_eq!(section.block_state(1, 2, 3).unwrap(), 5);
        assert_eq!(section.block_state(0, 0, 0).unwrap(), 0);
        assert!(section.block_state(16, 0, 0).is_err());
        let chunk = ChunkData {
            x: 0,
            z: 0,
            full: true,
            bitmask: 1,
            heightmaps: horizon_world::nbt::Tag::Compound(Default::default()),
            biomes: (0..1024).collect(),
            sections: vec![(0, section)],
            block_entities: vec![],
        };
        assert_eq!(chunk.block_state(1, 2, 3).unwrap(), 5);
        assert_eq!(chunk.block_state(1, 32, 3).unwrap(), 0);
        assert_eq!(chunk.biome(4, 8, 4), Some(2 * 16 + 1 * 4 + 1));
        assert_eq!(chunk.biome(0, 256, 0), None);
    }

    #[test]
    fn world_resolves_palette_ids_through_the_m0_tables() {
        let tables = serde_json::json!({
            "version": "1.16.5",
            "blocks": {"entries": {
                "minecraft:air": {"states": [{"id": 0, "default": true}]},
                "minecraft:stone": {"states": [{"id": 5, "default": true}]},
            }},
        });
        let mut section = Writer::new();
        section.u16(1);
        section.u8(4);
        section.varint(2);
        section.varint(0);
        section.varint(5);
        section.varint(256);
        for i in 0..256 {
            section.i64(if i == (2 * 256 + 3 * 16 + 1) / 16 {
                1i64 << (((2 * 256 + 3 * 16 + 1) % 16) * 4)
            } else {
                0
            });
        }
        let heightmaps = horizon_world::nbt::write_named(
            &horizon_world::nbt::NamedTag {
                name: "".into(),
                tag: horizon_world::nbt::Tag::Compound(Default::default()),
            },
            horizon_world::nbt::Limits::default(),
        )
        .unwrap();
        let mut packet = Writer::new();
        packet.i32(0);
        packet.i32(0);
        packet.bool(true);
        packet.varint(1);
        packet.bytes(&heightmaps);
        packet.varint(1024);
        for _ in 0..1024 {
            packet.varint(1);
        }
        packet.byte_array(&section.data);
        packet.varint(0);
        let chunk = parse_chunk_data(&packet.data).unwrap();
        let tables_path =
            std::env::temp_dir().join(format!("horizon-1165-tables-{}.json", std::process::id()));
        std::fs::write(&tables_path, serde_json::to_vec(&tables).unwrap()).unwrap();
        let mut world = World::load(&tables_path).unwrap();
        std::fs::remove_file(&tables_path).unwrap();
        world.apply_chunk(chunk);
        assert_eq!(world.block(1, 2, 3), Some("minecraft:stone"));
        assert_eq!(world.block(0, 0, 0), Some("minecraft:air"));
        assert_eq!(world.block(1, 32, 3), Some("minecraft:air"));
        assert_eq!(world.block(16, 2, 3), None);
        assert_eq!(world.biome(1, 2, 3), Some(1));
    }

    #[test]
    fn a_keep_alive_must_carry_exactly_one_i64() {
        let (client_pipe, _server_pipe) = Pipe::pair();
        let mut client = Client {
            connection: Connection::local(client_pipe),
            profile: Profile {
                uuid: [0; 16],
                name: "TestBot".into(),
            },
            threshold: None,
        };
        assert!(client.answer_keep_alive(PLAY_KEEP_ALIVE, b"short").is_err());
        assert!(!client.answer_keep_alive(0, &[]).unwrap());
    }

    #[test]
    fn player_position_and_look_decodes_the_verified_754_layout() {
        // x=1.5, y=64.0, z=-2.25, yaw=90.0, pitch=-45.0, flags=0, id=5.
        let payload = [
            0x3f, 0xf8, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // x
            0x40, 0x50, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // y
            0xc0, 0x02, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // z
            0x42, 0xb4, 0x00, 0x00, // yaw
            0xc2, 0x34, 0x00, 0x00, // pitch
            0x00, // flags
            0x05, // teleport id
        ];
        let position = parse_position_and_look(&payload).unwrap();
        assert_eq!(position.x, 1.5);
        assert_eq!(position.y, 64.0);
        assert_eq!(position.z, -2.25);
        assert_eq!(position.yaw, 90.0);
        assert_eq!(position.pitch, -45.0);
        assert_eq!(position.flags, 0);
        assert_eq!(position.teleport_id, 5);
        assert!(parse_position_and_look(&payload[..33]).is_err());
        let mut trailing = payload.to_vec();
        trailing.push(0);
        assert!(parse_position_and_look(&trailing).is_err());
    }

    #[test]
    fn relative_flags_and_multi_byte_teleport_id_are_kept() {
        let payload = [
            0x40, 0x21, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // x
            0x40, 0x54, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // y
            0x40, 0x30, 0x40, 0x00, 0x00, 0x00, 0x00, 0x00, // z
            0xc2, 0xb4, 0x00, 0x00, // yaw
            0x42, 0x34, 0x00, 0x00, // pitch
            0x1f, // all five relative bits
            0xac, 0x02, // teleport id 300
        ];
        let position = parse_position_and_look(&payload).unwrap();
        assert_eq!(position.flags, 0x1f);
        assert_eq!(
            position.flags,
            POSITION_FLAG_X
                | POSITION_FLAG_Y
                | POSITION_FLAG_Z
                | POSITION_FLAG_Y_ROT
                | POSITION_FLAG_X_ROT
        );
        assert_eq!(position.teleport_id, 300);
    }

    #[test]
    fn chat_message_decodes_json_position_and_sender_uuid() {
        let payload = [
            0x0d, // 13-byte string
            0x7b, 0x22, 0x74, 0x65, 0x78, 0x74, 0x22, 0x3a, 0x22, 0x68, 0x69, 0x22, 0x7d,
            0x01, // system message
            0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, // sender UUID
            0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0x0e, 0x0f, 0x10,
        ];
        let chat = parse_chat_message(&payload).unwrap();
        assert_eq!(chat.json, r#"{"text":"hi"}"#);
        assert_eq!(chat.position, CHAT_POSITION_SYSTEM);
        assert_eq!(chat.sender, 0x0102030405060708090a0b0c0d0e0f10);
        assert!(parse_chat_message(&payload[..payload.len() - 16]).is_err());
    }

    #[test]
    fn disconnect_decodes_the_json_reason() {
        let payload = [
            0x0e, // 14-byte string
            0x7b, 0x22, 0x74, 0x65, 0x78, 0x74, 0x22, 0x3a, 0x22, 0x62, 0x79, 0x65, 0x22, 0x7d,
        ];
        let disconnect = parse_disconnect(&payload).unwrap();
        assert_eq!(disconnect.reason, r#"{"text":"bye"}"#);
        assert!(parse_disconnect(b"").is_err());
    }

    #[test]
    fn teleport_confirm_varint_round_trip() {
        for id in [0, 127, 128, -1, i32::MAX] {
            let payload = teleport_confirm_payload(id);
            let mut reader = Reader::new(&payload);
            assert_eq!(reader.varint().unwrap(), id);
            assert_eq!(reader.remaining(), 0);
        }
        // Exact 1.16.5 VarInt wire forms.
        assert_eq!(teleport_confirm_payload(0), [0x00]);
        assert_eq!(teleport_confirm_payload(127), [0x7f]);
        assert_eq!(teleport_confirm_payload(128), [0x80, 0x01]);
        assert_eq!(teleport_confirm_payload(-1), [0xff, 0xff, 0xff, 0xff, 0x0f]);
        assert_eq!(
            teleport_confirm_payload(i32::MAX),
            [0xff, 0xff, 0xff, 0xff, 0x07]
        );
    }

    #[test]
    fn send_teleport_confirm_writes_packet_id_then_varint() {
        let (client_pipe, server_pipe) = Pipe::pair();
        let server = thread::spawn(move || {
            let mut connection = Connection::local(server_pipe);
            let (id, payload) = connection.recv().unwrap();
            assert_eq!(id, PLAY_TELEPORT_CONFIRM);
            let mut reader = Reader::new(&payload);
            assert_eq!(reader.varint().unwrap(), 300);
            assert_eq!(reader.remaining(), 0);
        });
        let mut client = Client {
            connection: Connection::local(client_pipe),
            profile: Profile {
                uuid: [0; 16],
                name: "TestBot".into(),
            },
            threshold: None,
        };
        client.send_teleport_confirm(300).unwrap();
        server.join().unwrap();
    }
}

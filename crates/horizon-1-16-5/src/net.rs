//! The 1.16.5 network client thread.
//!
//! Blocking `std::net::TcpStream` and a `std::sync::mpsc` channel: no async
//! runtime, no reactor. The thread logs in (offline mode), answers every
//! keep-alive with the same payload, confirms teleports, decodes chunks
//! through [`crate::chunk_codec::ChunkCodec`] and hands the game thread one
//! [`NetEvent`] per interesting packet. The render loop polls the receiver; it
//! never blocks on the socket.

use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::Duration;

use horizon_protocol::buf::Writer;
use horizon_protocol::v1_16_5 as protocol;
use horizon_protocol::v1_16_5::{
    PLAY_CHUNK_DATA, PLAY_DISCONNECT, PLAY_JOIN_GAME, PLAY_PLAYER_POSITION_AND_LOOK,
};

use crate::chunk_codec::ChunkCodec;
use crate::world::{ChunkColumn, World};

/// What the network thread tells the game thread.
pub enum NetEvent {
    /// The server accepted the login and started the play state.
    Connected {
        name: String,
        /// The server's view distance, in chunks.
        view_distance: i32,
    },
    /// One decoded chunk column.
    Chunk(Box<ChunkColumn>),
    /// The server placed the player.
    Position {
        position: [f64; 3],
        yaw: f32,
        pitch: f32,
    },
    /// A chat line (system or player), already flattened to text.
    Chat(String),
    /// The connection ended; the reason is fit for a log line.
    Disconnected(String),
}

/// The client's `Client Settings` (0x05) packet.
///
/// 1.16.5 sends this right after Join Game; the server uses it to size the
/// chunk stream around the player.
fn client_settings_payload(view_distance: i32, locale: &str) -> Vec<u8> {
    // Protocol 754 order: locale, view distance (byte), chat mode (varint),
    // chat colours (bool), skin parts (u8), main hand (varint).
    let mut body = Writer::new();
    body.string(locale);
    body.u8(u8::try_from(view_distance.clamp(2, 32)).unwrap_or(8));
    body.varint(0); // chat mode: enabled
    body.bool(false); // chat colours
    body.u8(0x7f); // skin parts: all
    body.varint(1); // main hand: right
    body.data
}

/// The `Player Position` (0x12) packet: feet position on the ground.
fn position_payload(position: [f64; 3]) -> Vec<u8> {
    let mut body = Writer::new();
    body.f64(position[0]);
    body.f64(position[1]);
    body.f64(position[2]);
    body.bool(true); // on ground
    body.data
}

/// The `Player Position And Rotation` (0x13) packet.
fn position_rotation_payload(position: [f64; 3], yaw: f32, pitch: f32) -> Vec<u8> {
    let mut body = Writer::new();
    body.f64(position[0]);
    body.f64(position[1]);
    body.f64(position[2]);
    body.f32(yaw);
    body.f32(pitch);
    body.bool(true); // on ground
    body.data
}

/// Commands the game thread can send back to the network thread.
pub enum NetCommand {
    /// Player moved; `rotation` also sends yaw/pitch.
    Move {
        position: [f64; 3],
        yaw: f32,
        pitch: f32,
        rotation: bool,
    },
    /// Leave the server.
    Quit,
}

/// A handle to the running network thread.
pub struct NetHandle {
    pub events: Receiver<NetEvent>,
    commands: Sender<NetCommand>,
}

impl NetHandle {
    /// Polls one event without blocking.
    pub fn try_recv(&self) -> Option<NetEvent> {
        self.events.try_recv().ok()
    }

    /// Sends the player's position to the server.
    pub fn send_move(&self, position: [f64; 3], yaw: f32, pitch: f32, rotation: bool) {
        let _ = self.commands.send(NetCommand::Move {
            position,
            yaw,
            pitch,
            rotation,
        });
    }

    /// Asks the thread to disconnect and stop.
    pub fn quit(&self) {
        let _ = self.commands.send(NetCommand::Quit);
    }
}

/// Connects to `host:port` and starts the network thread.
///
/// `tables` is only used by the caller; the thread decodes chunks into global
/// 1.16.5 block-state ids, which is exactly what the tables index. The
/// returned handle is used by the game loop.
pub fn spawn(
    host: String,
    port: u16,
    name: String,
    tables: Arc<crate::BlockTable>,
) -> Result<NetHandle, String> {
    let (event_tx, events) = mpsc::channel::<NetEvent>();
    let (commands, command_rx) = mpsc::channel::<NetCommand>();
    let thread_name = format!("horizon-1165-net {host}:{port}");
    let spawned = thread::Builder::new()
        .name(thread_name)
        .spawn(move || {
            let events = event_tx;
            let report = |event: NetEvent| {
                let _ = events.send(event);
            };
            if let Err(error) = run(&host, port, &name, tables, &report, &command_rx) {
                report(NetEvent::Disconnected(error));
            }
        })
        .map_err(|error| format!("starting the network thread: {error}"))?;
    // The handle owns the join handle only for the thread's name; dropping it
    // is fine because the thread stops on its own when the socket closes.
    drop(spawned);
    Ok(NetHandle { events, commands })
}

fn run(
    host: &str,
    port: u16,
    name: &str,
    _tables: Arc<crate::BlockTable>,
    report: &dyn Fn(NetEvent),
    commands: &Receiver<NetCommand>,
) -> Result<(), String> {
    let mut client = protocol::login_offline(host, port, name, Duration::from_secs(10))
        .map_err(|error| format!("1.16.5 login: {error}"))?;
    set_read_timeout(&client, Duration::from_millis(50))?;
    report(NetEvent::Connected {
        name: client.profile.name.clone(),
        view_distance: 0,
    });
    loop {
        while let Ok(command) = commands.try_recv() {
            match command {
                NetCommand::Move {
                    position,
                    yaw,
                    pitch,
                    rotation,
                } => {
                    let payload = if rotation {
                        position_rotation_payload(position, yaw, pitch)
                    } else {
                        position_payload(position)
                    };
                    let id = if rotation { 0x13 } else { 0x12 };
                    client
                        .send(id, &payload)
                        .map_err(|error| format!("sending position: {error}"))?;
                }
                NetCommand::Quit => return Ok(()),
            }
        }
        match client.recv() {
            Ok((id, payload)) => handle_packet(&mut client, id, &payload, report)?,
            Err(error) if error.is_timed_out() => {}
            Err(error) => return Err(format!("1.16.5 connection: {error}")),
        }
    }
}

fn set_read_timeout(client: &protocol::Client, timeout: Duration) -> Result<(), String> {
    client
        .connection
        .reader
        .set_read_timeout(Some(timeout))
        .map_err(|error| format!("socket timeout: {error}"))
}

fn handle_packet(
    client: &mut protocol::Client,
    id: i32,
    payload: &[u8],
    report: &dyn Fn(NetEvent),
) -> Result<(), String> {
    if client
        .answer_keep_alive(id, payload)
        .map_err(|error| format!("keep-alive: {error}"))?
    {
        return Ok(());
    }
    match id {
        PLAY_JOIN_GAME => {
            let join = protocol::parse_join_game(payload)
                .map_err(|error| format!("Join Game: {error}"))?;
            report(NetEvent::Connected {
                name: client.profile.name.clone(),
                view_distance: join.view_distance,
            });
            // 1.16.5 servers expect Client Settings before the chunk stream.
            let settings = client_settings_payload(join.view_distance.clamp(2, 32), "en_us");
            client
                .send(0x05, &settings)
                .map_err(|error| format!("client settings: {error}"))?;
        }
        PLAY_CHUNK_DATA => {
            let data = protocol::parse_chunk_data(payload)
                .map_err(|error| format!("Chunk Data: {error}"))?;
            let column = ChunkCodec::column_from_network(&data)
                .map_err(|error| format!("Chunk Data codec: {error}"))?;
            report(NetEvent::Chunk(Box::new(column)));
        }
        PLAY_PLAYER_POSITION_AND_LOOK => {
            let position = protocol::parse_position_and_look(payload)
                .map_err(|error| format!("Player Position And Look: {error}"))?;
            report(NetEvent::Position {
                position: [position.x, position.y, position.z],
                yaw: position.yaw,
                pitch: position.pitch,
            });
            client
                .send_teleport_confirm(position.teleport_id)
                .map_err(|error| format!("Teleport Confirm: {error}"))?;
        }
        PLAY_DISCONNECT => {
            let reason = protocol::parse_disconnect(payload)
                .map(|disconnect| disconnect.reason)
                .unwrap_or_else(|_| "invalid reason".to_owned());
            report(NetEvent::Disconnected(format!(
                "kicked by the server: {reason}"
            )));
            return Err(format!("1.16.5 server disconnected: {reason}"));
        }
        0x0E => {
            if let Ok(chat) = protocol::parse_chat_message(payload) {
                report(NetEvent::Chat(chat.json));
            }
        }
        _ => {}
    }
    Ok(())
}

/// Applies a network chunk to the world (used by the game loop).
pub fn apply_chunk(world: &mut World, column: ChunkColumn) -> Result<(), String> {
    world.insert_column(column)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_settings_encode_locale_and_view_distance() {
        let payload = client_settings_payload(8, "en_us");
        assert_eq!(&payload[..6], b"\x05en_us");
        // locale, view distance byte, then chat mode, colours, skin, hand.
        assert_eq!(payload[6], 8);
        assert_eq!(&payload[7..], &[0, 0, 0x7f, 1]);
    }

    #[test]
    fn position_packets_carry_ground_flag() {
        let plain = position_payload([1.0, 2.0, 3.0]);
        assert_eq!(plain.len(), 8 + 8 + 8 + 1);
        assert_eq!(*plain.last().unwrap(), 1);
        let rotated = position_rotation_payload([1.0, 2.0, 3.0], 90.0, 10.0);
        assert_eq!(rotated.len(), 8 + 8 + 8 + 4 + 4 + 1);
    }
}

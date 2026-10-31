//! Minecraft 1.16.5 (protocol 754) connection primitives for the second
//! engine target. This is the M1 network slice only: status ping, offline-mode
//! login, compression and the play-state keep-alive exchange. It deliberately
//! does not reuse the 26.3 packet enums or packet ids.
//!
//! Scope: a local/offline 1.16.5 server. Online-mode encryption and the
//! 1.16.5 play packet set beyond keep-alive are the next milestones.

use crate::{
    Error, Result,
    buf::{Reader, Writer},
    connection::Connection,
};
use serde_json::Value;
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
pub const PLAY_KEEP_ALIVE: i32 = 0x1F;
pub const PLAY_KEEP_ALIVE_RESPONSE: i32 = 0x10;
pub const PLAY_DISCONNECT: i32 = 0x19;

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
                        let (reason, _) = read_string(&payload, 32_767)?;
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
        let held = client.hold(Duration::from_secs(17)).unwrap();
        assert!(held.received > 0, "no play packets");
        assert!(held.keep_alives > 0, "no keep-alive answered");
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
}

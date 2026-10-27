//! Server list ping: the status handshake from the Minecraft protocol.

#![forbid(unsafe_code)]

use std::io::{self, Read, Write};
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

/// Protocol version sent in the handshake. Servers only echo it back for the
/// status request, so any modern value works.
const PROTOCOL: i32 = 767;
const MAX_PACKET: usize = 4 * 1024 * 1024;

#[derive(Debug, Error)]
pub enum StatusError {
    #[error("cannot resolve {host}: {source}")]
    Resolve {
        host: String,
        #[source]
        source: io::Error,
    },
    #[error("cannot reach {host}:{port}: {source}")]
    Connect {
        host: String,
        port: u16,
        #[source]
        source: io::Error,
    },
    #[error("{0}")]
    Protocol(String),
    #[error("network error: {0}")]
    Io(#[source] io::Error),
    #[error("the server closed the connection")]
    Closed,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ServerStatus {
    /// Version name the server reports, e.g. `Paper 1.20.1`.
    pub version: String,
    pub protocol: i32,
    pub online: i64,
    pub max: i64,
    /// Names of players the server chose to list.
    pub sample: Vec<String>,
    /// MOTD with colour codes removed.
    pub motd: String,
    /// `data:image/png;base64,...` when the server sends an icon.
    pub icon: Option<String>,
    pub latency_ms: u64,
    /// `host:port` that answered.
    pub address: String,
}

/// Query one server. `timeout` covers connecting and the whole exchange.
pub fn ping(host: &str, port: u16, timeout: Duration) -> Result<ServerStatus, StatusError> {
    let addresses: Vec<SocketAddr> = (host, port)
        .to_socket_addrs()
        .map_err(|source| StatusError::Resolve {
            host: host.to_string(),
            source,
        })?
        .collect();
    if addresses.is_empty() {
        return Err(StatusError::Resolve {
            host: host.to_string(),
            source: io::Error::new(io::ErrorKind::NotFound, "no addresses"),
        });
    }
    // Broken IPv6 paths stall for seconds; IPv4 first matches the downloader.
    let mut sorted = addresses.clone();
    sorted.sort_by_key(|address| !address.is_ipv4());

    let started = Instant::now();
    let mut stream = None;
    let mut last_error = None;
    for address in sorted {
        match TcpStream::connect_timeout(&address, timeout) {
            Ok(candidate) => {
                stream = Some(candidate);
                break;
            }
            Err(error) => last_error = Some(error),
        }
    }
    let mut stream = stream.ok_or_else(|| StatusError::Connect {
        host: host.to_string(),
        port,
        source: last_error.unwrap_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no addresses")),
    })?;
    stream
        .set_read_timeout(Some(timeout))
        .and_then(|()| stream.set_write_timeout(Some(timeout)))
        .map_err(|source| StatusError::Connect {
            host: host.to_string(),
            port,
            source,
        })?;

    let mut payload = Vec::new();
    write_varint(&mut payload, 0x00);
    write_varint(&mut payload, PROTOCOL);
    write_string(&mut payload, host);
    payload.extend_from_slice(&port.to_be_bytes());
    write_varint(&mut payload, 1);
    write_packet(&mut stream, &payload)?;
    write_packet(&mut stream, &[0x00])?;

    let _length = read_varint(&mut stream)?;
    if read_varint(&mut stream)? != 0x00 {
        return Err(StatusError::Protocol("expected a status packet".to_string()));
    }
    let text = read_string(&mut stream)?;
    let latency_ms = started.elapsed().as_millis() as u64;
    let value: Value = serde_json::from_str(&text)
        .map_err(|error| StatusError::Protocol(format!("cannot read the status JSON: {error}")))?;

    let version = value
        .get("version")
        .and_then(|version| version.get("name"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let protocol = value
        .get("version")
        .and_then(|version| version.get("protocol"))
        .and_then(Value::as_i64)
        .unwrap_or_default() as i32;
    let players = value.get("players");
    let online = players
        .and_then(|players| players.get("online"))
        .and_then(Value::as_i64)
        .unwrap_or_default();
    let max = players
        .and_then(|players| players.get("max"))
        .and_then(Value::as_i64)
        .unwrap_or_default();
    let sample = players
        .and_then(|players| players.get("sample"))
        .and_then(Value::as_array)
        .map(|entries| {
            entries
                .iter()
                .filter_map(|entry| entry.get("name").and_then(Value::as_str).map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    let motd = value.get("description").map(flatten).unwrap_or_default();
    let icon = value
        .get("favicon")
        .and_then(Value::as_str)
        .filter(|icon| icon.starts_with("data:image"))
        .map(str::to_string);

    Ok(ServerStatus {
        version,
        protocol,
        online,
        max,
        sample,
        motd: strip_colours(&motd).trim().to_string(),
        icon,
        latency_ms,
        address: format!("{host}:{port}"),
    })
}

fn write_packet(stream: &mut TcpStream, payload: &[u8]) -> Result<(), StatusError> {
    let mut packet = Vec::with_capacity(payload.len() + 5);
    write_varint(&mut packet, payload.len() as i32);
    packet.extend_from_slice(payload);
    stream
        .write_all(&packet)
        .and_then(|()| stream.flush())
        .map_err(StatusError::Io)
}

impl From<io::Error> for StatusError {
    fn from(source: io::Error) -> Self {
        if source.kind() == io::ErrorKind::UnexpectedEof {
            return Self::Closed;
        }
        Self::Io(source)
    }
}

fn write_varint(out: &mut Vec<u8>, value: i32) {
    let mut value = value as u32;
    loop {
        let mut byte = (value & 0x7f) as u8;
        value >>= 7;
        if value != 0 {
            byte |= 0x80;
        }
        out.push(byte);
        if value == 0 {
            break;
        }
    }
}

fn read_varint(stream: &mut impl Read) -> Result<i32, StatusError> {
    let mut result = 0u32;
    for shift in (0..35).step_by(7) {
        let mut byte = [0u8; 1];
        stream.read_exact(&mut byte)?;
        result |= ((byte[0] & 0x7f) as u32) << shift;
        if byte[0] & 0x80 == 0 {
            return Ok(result as i32);
        }
    }
    Err(StatusError::Protocol("varint is too long".to_string()))
}

fn write_string(out: &mut Vec<u8>, value: &str) {
    write_varint(out, value.len() as i32);
    out.extend_from_slice(value.as_bytes());
}

fn read_string(stream: &mut impl Read) -> Result<String, StatusError> {
    let length = read_varint(stream)?;
    if length < 0 || length as usize > MAX_PACKET {
        return Err(StatusError::Protocol(format!("invalid string length {length}")));
    }
    let mut bytes = vec![0u8; length as usize];
    stream.read_exact(&mut bytes)?;
    String::from_utf8(bytes).map_err(|error| StatusError::Protocol(format!("invalid UTF-8: {error}")))
}

/// Chat components can nest: `{"text":"a","extra":[{"text":"b"}]}`.
fn flatten(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Array(items) => items.iter().map(flatten).collect(),
        Value::Object(map) => {
            let mut text = map
                .get("text")
                .map(flatten)
                .or_else(|| map.get("translate").map(flatten))
                .unwrap_or_default();
            if let Some(extra) = map.get("extra") {
                text.push_str(&flatten(extra));
            }
            text
        }
        Value::Number(number) => number.to_string(),
        Value::Bool(value) => value.to_string(),
        Value::Null => String::new(),
    }
}

/// Drop `§x` formatting codes.
fn strip_colours(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut skip = false;
    for ch in text.chars() {
        if skip {
            skip = false;
            continue;
        }
        if ch == '§' {
            skip = true;
            continue;
        }
        out.push(ch);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn varints_round_trip() {
        for value in [0, 1, 127, 128, 255, 300, 25565, 767, 2_147_483_647] {
            let mut buffer = Vec::new();
            write_varint(&mut buffer, value);
            assert_eq!(read_varint(&mut buffer.as_slice()).unwrap(), value);
        }
    }

    #[test]
    fn flattens_chat_components() {
        let value: Value =
            serde_json::from_str(r#"{"text":"Hello ","extra":[{"text":"world","color":"red"}]}"#).unwrap();
        assert_eq!(flatten(&value), "Hello world");
        assert_eq!(flatten(&Value::String("plain".into())), "plain");
    }

    #[test]
    fn reads_a_status_response() {
        use std::net::TcpListener;
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            let mut buffer = [0u8; 512];
            let _ = socket.read(&mut buffer);
            let json = r#"{"version":{"name":"Paper 1.20.1","protocol":763},"players":{"online":3,"max":20},"description":{"text":"Hello "}}"#;
            let mut payload = Vec::new();
            write_varint(&mut payload, 0x00);
            write_string(&mut payload, json);
            let mut packet = Vec::new();
            write_varint(&mut packet, payload.len() as i32);
            packet.extend_from_slice(&payload);
            socket.write_all(&packet).unwrap();
            socket.flush().unwrap();
        });
        let status = ping("127.0.0.1", port, Duration::from_secs(2)).unwrap();
        assert_eq!(status.version, "Paper 1.20.1");
        assert_eq!(status.online, 3);
        assert_eq!(status.motd, "Hello");
    }

    #[test]
    fn strips_colour_codes() {
        assert_eq!(strip_colours("§aHello §lWorld"), "Hello World");
    }
}

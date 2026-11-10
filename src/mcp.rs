use crate::player::{Job, Reply, submit};
use anyhow::{Result, ensure};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde_json::{Value, json};
use std::{
    io::{self, BufRead, BufReader, Read, Write},
    net::{Ipv4Addr, SocketAddrV4, TcpListener, TcpStream},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::SyncSender,
    },
    thread,
    time::Duration,
};

const TIMEOUT: Duration = Duration::from_secs(55);
const MAX_MESSAGE: u64 = 1024 * 1024;

fn tools() -> Value {
    let query = json!({"query":{"type":"string","minLength":1,"maxLength":500}});
    let id = json!({"track_id":{"type":"integer","minimum":1}});
    let index = json!({"index":{"type":"integer","minimum":0}});
    let toggle = json!({"enabled":{"type":"boolean"}});
    let no_args = json!({});
    let entries = [
        (
            "equalizer",
            "Enable or bypass the equalizer and/or set 5 gains in dB for 60, 250, 1000, 4000 and 12000 Hz.",
            &json!({
                "enabled":{"type":"boolean"},
                "gains":{"type":"array","minItems":5,"maxItems":5,"items":{"type":"integer","minimum":-12,"maximum":12}}
            }),
            &[][..],
        ),
        (
            "wave_start",
            "Start the signed-in listener's personalized My Wave station.",
            &no_args,
            &[][..],
        ),
        (
            "state",
            "Read playback state, current track, queue and settings.",
            &no_args,
            &[][..],
        ),
        (
            "search",
            "Search tracks by title or artist.",
            &query,
            &["query"][..],
        ),
        (
            "artist",
            "Read an artist's description, cover, stats and top tracks by artist id.",
            &json!({"artist_id":{"type":"integer","minimum":1}}),
            &["artist_id"][..],
        ),
        ("library", "Read all liked tracks.", &no_args, &[][..]),
        (
            "playlists",
            "Read the signed-in listener's playlists from their profile.",
            &no_args,
            &[][..],
        ),
        (
            "playlist_tracks",
            "Read tracks from a playlist by its playlist id.",
            &json!({"playlist_id":{"type":"string","minLength":1}}),
            &["playlist_id"][..],
        ),
        (
            "play_query",
            "Search and play the first result, using the results as a queue.",
            &query,
            &["query"][..],
        ),
        (
            "play_track",
            "Replace the queue and play a track by ID.",
            &id,
            &["track_id"][..],
        ),
        ("play", "Resume playback.", &no_args, &[][..]),
        ("pause", "Pause playback.", &no_args, &[][..]),
        (
            "next",
            "Play the next track in the queue.",
            &no_args,
            &[][..],
        ),
        (
            "previous",
            "Play the previous track in the queue.",
            &no_args,
            &[][..],
        ),
        (
            "like",
            "Like the current or specified track in the signed-in account.",
            &id,
            &[][..],
        ),
        (
            "unlike",
            "Remove the current or specified track from liked tracks.",
            &id,
            &[][..],
        ),
        (
            "queue_add",
            "Add a track to the queue by ID.",
            &id,
            &["track_id"][..],
        ),
        (
            "queue_play",
            "Play the track at a zero-based queue position.",
            &index,
            &["index"][..],
        ),
        (
            "queue_remove",
            "Remove a track at a zero-based queue position.",
            &index,
            &["index"][..],
        ),
        (
            "seek",
            "Seek to a position in seconds.",
            &json!({"seconds":{"type":"number","minimum":0}}),
            &["seconds"][..],
        ),
        (
            "volume",
            "Set volume between 0 and 1.",
            &json!({"value":{"type":"number","minimum":0,"maximum":1}}),
            &["value"][..],
        ),
        (
            "repeat",
            "Set the repeat mode.",
            &json!({"mode":{"type":"string","enum":["off","track","queue"]}}),
            &["mode"][..],
        ),
        ("shuffle", "Set shuffle.", &toggle, &["enabled"][..]),
        (
            "lossless",
            "Request lossless quality for the next track.",
            &toggle,
            &["enabled"][..],
        ),
        (
            "discord_rpc",
            "Enable or disable Discord Rich Presence.",
            &toggle,
            &["enabled"][..],
        ),
    ];
    Value::Array(entries.iter().map(|(name, desc, properties, required)| json!({
        "name": name, "description": desc,
        "inputSchema": { "type":"object", "properties": properties, "required": required, "additionalProperties":false }
    })).collect())
}

pub struct Bridge {
    port: u16,
    token: String,
    active: Arc<AtomicBool>,
}

impl Bridge {
    pub fn start(tx: SyncSender<Job>) -> Result<Self> {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
        let port = listener.local_addr()?.port();
        let mut entropy = [0u8; 32];
        getrandom::fill(&mut entropy)
            .map_err(|_| anyhow::anyhow!("Не удалось создать ключ MCP"))?;
        let token = URL_SAFE_NO_PAD.encode(entropy);
        let key = token.clone();
        let active = Arc::new(AtomicBool::new(true));
        let running = active.clone();
        thread::spawn(move || {
            for socket in listener.incoming() {
                if !running.load(Ordering::Acquire) {
                    break;
                }
                let Ok(mut socket) = socket else { break };
                let _ = socket.set_read_timeout(Some(Duration::from_secs(2)));
                let _ = socket.set_write_timeout(Some(Duration::from_secs(2)));
                let reply = handle_socket(&socket, &key, &running, &tx)
                    .unwrap_or_else(|e| Err(e.to_string()));
                let _ = send(&mut socket, &json!(reply));
            }
        });
        Ok(Self {
            port,
            token,
            active,
        })
    }

    pub fn config(&self) -> Result<Value> {
        let exe = std::env::current_exe()?;
        Ok(json!({"mcpServers":{"yandex-music":{
            "command": exe, "args": ["--mcp", self.port.to_string()],
            "env": {"YANDEX_MUSIC_MCP_KEY": self.token}
        }}}))
    }
}

impl Drop for Bridge {
    fn drop(&mut self) {
        self.active.store(false, Ordering::Release);
        let _ = TcpStream::connect_timeout(&address(self.port), Duration::from_millis(100));
    }
}

fn address(port: u16) -> std::net::SocketAddr {
    SocketAddrV4::new(Ipv4Addr::LOCALHOST, port).into()
}

fn read_line(input: &mut impl BufRead) -> Result<Option<Vec<u8>>> {
    let mut bytes = Vec::new();
    let len = input.take(MAX_MESSAGE + 1).read_until(b'\n', &mut bytes)?;
    if len == 0 {
        return Ok(None);
    }
    ensure!(len as u64 <= MAX_MESSAGE, "MCP message exceeds 1 MiB");
    Ok(Some(bytes))
}

fn send(output: &mut impl Write, value: &Value) -> Result<()> {
    serde_json::to_writer(&mut *output, value)?;
    output.write_all(b"\n")?;
    output.flush()?;
    Ok(())
}

fn handle_socket(
    socket: &TcpStream,
    token: &str,
    active: &AtomicBool,
    tx: &SyncSender<Job>,
) -> Result<Reply> {
    let mut reader = BufReader::new(socket);
    let message: Value = serde_json::from_slice(&read_line(&mut reader)?.unwrap_or_default())?;
    ensure!(
        active.load(Ordering::Acquire) && message["token"].as_str() == Some(token),
        "MCP отключён или ключ недействителен"
    );
    let name = message["name"].as_str().unwrap_or_default();
    ensure!(
        tools()
            .as_array()
            .unwrap()
            .iter()
            .any(|tool| tool["name"] == name),
        "Неизвестный инструмент"
    );
    let args = message["args"].clone();
    ensure!(args.is_object(), "Аргументы должны быть объектом");
    Ok(submit(tx, name, args)?.recv_timeout(TIMEOUT)?)
}

fn invoke(port: u16, token: &str, name: &Value, args: &Value) -> Result<Reply> {
    let mut socket =
        TcpStream::connect_timeout(&address(port), Duration::from_secs(2)).map_err(|_| {
            anyhow::anyhow!("Откройте клиент, включите MCP и скопируйте текущую конфигурацию")
        })?;
    socket.set_read_timeout(Some(TIMEOUT + Duration::from_secs(3)))?;
    socket.set_write_timeout(Some(Duration::from_secs(2)))?;
    send(&mut socket, &json!({"token":token,"name":name,"args":args}))?;
    let bytes = read_line(&mut BufReader::new(socket))?.unwrap_or_default();
    Ok(serde_json::from_slice(&bytes)?)
}

fn error(id: &Value, code: i32, message: &str) -> Value {
    json!({"jsonrpc":"2.0","id":id,"error":{"code":code,"message":message}})
}

pub fn stdio(port: u16, token: &str) -> Result<()> {
    let mut input = io::stdin().lock();
    let mut output = io::stdout().lock();
    let mut initialized = false;
    while let Some(line) = read_line(&mut input)? {
        let request: Value = match serde_json::from_slice(&line) {
            Ok(value) => value,
            Err(_) => {
                send(&mut output, &error(&Value::Null, -32700, "Parse error"))?;
                continue;
            }
        };
        let id = &request["id"];
        let Some(method) = request["method"]
            .as_str()
            .filter(|_| request["jsonrpc"] == "2.0")
        else {
            send(&mut output, &error(id, -32600, "Invalid request"))?;
            continue;
        };
        if request.get("id").is_none() {
            continue;
        }
        let result = match method {
            "initialize" => {
                initialized = true;
                let version = request
                    .pointer("/params/protocolVersion")
                    .and_then(Value::as_str)
                    .unwrap_or("2025-11-25");
                json!({"protocolVersion":version,"serverInfo":{"name":"yandex-music-rust","version":env!("CARGO_PKG_VERSION")},"capabilities":{"tools":{}}})
            }
            "ping" => json!({}),
            "tools/list" if initialized => json!({"tools":tools()}),
            "tools/call" if initialized => {
                let p = &request["params"];
                let reply = invoke(
                    port,
                    token,
                    &p["name"],
                    &p.get("arguments").cloned().unwrap_or_else(|| json!({})),
                )
                .unwrap_or_else(|e| Err(e.to_string()));
                match reply {
                    Ok(value) => json!({"content":[{"type":"text","text":value.to_string()}]}),
                    Err(message) => {
                        json!({"content":[{"type":"text","text":message}],"isError":true})
                    }
                }
            }
            _ => {
                send(&mut output, &error(id, -32601, "Method not found"))?;
                continue;
            }
        };
        send(
            &mut output,
            &json!({"jsonrpc":"2.0","id":id,"result":result}),
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bridge_changes_the_shared_player_and_checks_access() {
        let (tx, snapshot) = crate::player::start(|| {}, None);
        let bridge = Bridge::start(tx).unwrap();
        let result = invoke(
            bridge.port,
            "wrong-key",
            &json!("volume"),
            &json!({"value":0.1}),
        );
        assert!(result.unwrap().is_err());
        invoke(
            bridge.port,
            &bridge.token,
            &json!("volume"),
            &json!({"value":0.25}),
        )
        .unwrap()
        .unwrap();
        assert_eq!(snapshot.lock().unwrap().volume, 0.25);
        let port = bridge.port;
        let key = bridge.token.clone();
        drop(bridge);
        let result = invoke(port, &key, &json!("state"), &json!({}));
        assert!(!matches!(result, Ok(Ok(_))));
    }

    #[test]
    #[ignore = "calls the live Yandex Music API"]
    fn public_search_through_bridge() {
        let (tx, _) = crate::player::start(|| {}, None);
        let bridge = Bridge::start(tx).unwrap();
        let tracks = invoke(
            bridge.port,
            &bridge.token,
            &json!("search"),
            &json!({"query":"Daft Punk"}),
        )
        .unwrap()
        .unwrap();
        assert!(!tracks.as_array().unwrap().is_empty());
        assert!(tracks[0]["id"].as_u64().is_some());
    }
}

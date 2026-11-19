use anyhow::{Result, bail, ensure};
use discord_rich_presence::DiscordIpc;
use serde_json::{Value, json};

#[cfg(not(windows))]
pub use discord_rich_presence::DiscordIpcClient as Client;
#[cfg(windows)]
pub use windows_client::Client;

#[cfg(any(windows, test))]
const MAX_PAYLOAD: usize = 1024 * 1024;

// Shared music application IDs used by FozerG/WinYandexMusicRPC.
pub fn application_id(language: &str) -> &'static str {
    if language == "en" {
        "1269807014393942046"
    } else {
        "1269826362399522849"
    }
}

pub fn discord_text(value: &str, fallback: &str) -> String {
    let value = value.trim();
    let mut text: String = if value.is_empty() { fallback } else { value }
        .chars()
        .take(128)
        .collect();
    if text.chars().count() == 1 {
        text.push('\u{200b}');
    }
    text
}

pub fn prepare_activity(mut activity: Value, activity_type: &str) -> Value {
    activity["type"] = json!(if activity_type == "playing" { 0 } else { 2 });
    activity["status_display_type"] = json!(0);
    activity["instance"] = json!(true);
    activity
}

#[cfg(any(windows, test))]
fn encode_frame(opcode: u32, payload: &[u8]) -> Result<Vec<u8>> {
    ensure!(
        payload.len() <= MAX_PAYLOAD,
        "Пакет Discord слишком большой"
    );
    let mut frame = Vec::with_capacity(8 + payload.len());
    frame.extend_from_slice(&opcode.to_le_bytes());
    frame.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    frame.extend_from_slice(payload);
    Ok(frame)
}

#[cfg(windows)]
fn receive(client: &mut impl DiscordIpc) -> Result<(u32, Value)> {
    for _ in 0..8 {
        let mut header = [0; 8];
        client
            .read(&mut header)
            .map_err(|error| anyhow::anyhow!("{error}"))?;
        let opcode = u32::from_le_bytes(header[..4].try_into()?);
        let length = u32::from_le_bytes(header[4..].try_into()?) as usize;
        ensure!(length <= MAX_PAYLOAD, "Пакет Discord слишком большой");
        let mut payload = vec![0; length];
        client
            .read(&mut payload)
            .map_err(|error| anyhow::anyhow!("{error}"))?;
        if opcode == 3 {
            let pong = encode_frame(4, &payload)?;
            client
                .write(&pong)
                .map_err(|error| anyhow::anyhow!("{error}"))?;
            continue;
        }
        return Ok((opcode, serde_json::from_slice(&payload)?));
    }
    bail!("Discord не прислал ответ после ping")
}

pub fn publish(client: &mut impl DiscordIpc, activity: Value, nonce: &str) -> Result<()> {
    client
        .send(
            json!({"cmd": "SET_ACTIVITY", "args": {
                "pid": std::process::id(), "activity": activity
            }, "nonce": nonce}),
            1,
        )
        .map_err(|error| anyhow::anyhow!("{error}"))?;
    for _ in 0..8 {
        let (opcode, response) = client.recv().map_err(|error| anyhow::anyhow!("{error}"))?;
        if opcode == 3 {
            client
                .send(response, 4)
                .map_err(|error| anyhow::anyhow!("{error}"))?;
            continue;
        }
        ensure!(opcode == 1, "Discord закрыл соединение");
        if response["nonce"].as_str() != Some(nonce) {
            continue;
        }
        return validate_reply(&response);
    }
    bail!("Discord не подтвердил обновление активности")
}

fn validate_reply(reply: &Value) -> Result<()> {
    ensure!(
        reply["evt"] != "ERROR",
        "{} (код {})",
        reply["data"]["message"]
            .as_str()
            .unwrap_or("Discord отклонил активность"),
        reply["data"]["code"]
    );
    ensure!(
        reply["cmd"] == "SET_ACTIVITY" && reply["data"].is_object(),
        "Discord не подтвердил активность"
    );
    Ok(())
}

#[cfg(windows)]
mod windows_client {
    use discord_rich_presence::DiscordIpc;
    use serde_json::json;
    use std::{
        error::Error,
        fs::{File, OpenOptions},
        io::{self, Read, Write},
        os::windows::io::AsRawHandle,
        thread,
        time::{Duration, Instant},
    };
    use windows::Win32::{Foundation::HANDLE, System::Pipes::PeekNamedPipe};

    type IpcResult<T> = std::result::Result<T, Box<dyn Error>>;

    pub struct Client {
        id: String,
        socket: Option<File>,
    }

    impl Client {
        pub fn new(id: &str) -> IpcResult<Self> {
            Ok(Self {
                id: id.into(),
                socket: None,
            })
        }

        fn socket(&mut self) -> io::Result<&mut File> {
            self.socket.as_mut().ok_or_else(|| {
                io::Error::new(io::ErrorKind::NotConnected, "Соединение с Discord закрыто")
            })
        }
    }

    impl DiscordIpc for Client {
        fn get_client_id(&self) -> &String {
            &self.id
        }

        fn connect_ipc(&mut self) -> IpcResult<()> {
            for index in 0..10 {
                if let Ok(socket) = OpenOptions::new()
                    .read(true)
                    .write(true)
                    .open(format!(r"\\.\pipe\discord-ipc-{index}"))
                {
                    self.socket = Some(socket);
                    return Ok(());
                }
            }
            Err("Запустите настольный Discord".into())
        }

        fn send_handshake(&mut self) -> IpcResult<()> {
            self.send(json!({"v": 1, "client_id": self.id}), 0)?;
            let (opcode, reply) = self.recv()?;
            if opcode != 1 || reply["evt"] != "READY" {
                return Err("Discord отклонил подключение RPC".into());
            }
            Ok(())
        }

        fn send(&mut self, data: serde_json::Value, opcode: u8) -> IpcResult<()> {
            let frame = super::encode_frame(opcode as u32, &serde_json::to_vec(&data)?)?;
            self.write(&frame)
        }

        fn recv(&mut self) -> IpcResult<(u32, serde_json::Value)> {
            super::receive(self).map_err(|error| error.into())
        }

        fn write(&mut self, data: &[u8]) -> IpcResult<()> {
            self.socket()?.write_all(data)?;
            Ok(())
        }

        fn read(&mut self, buffer: &mut [u8]) -> IpcResult<()> {
            let socket = self.socket()?;
            let deadline = Instant::now() + Duration::from_millis(750);
            let mut offset = 0;
            while offset < buffer.len() {
                let mut available = 0;
                unsafe {
                    PeekNamedPipe(
                        HANDLE(socket.as_raw_handle() as isize),
                        None,
                        0,
                        None,
                        Some(&mut available),
                        None,
                    )?;
                }
                if available > 0 {
                    let end = (offset + available as usize).min(buffer.len());
                    let read = socket.read(&mut buffer[offset..end])?;
                    if read == 0 {
                        return Err("Discord закрыл соединение".into());
                    }
                    offset += read;
                } else {
                    if Instant::now() >= deadline {
                        return Err("Discord не ответил вовремя".into());
                    }
                    thread::sleep(Duration::from_millis(5));
                }
            }
            Ok(())
        }

        fn close(&mut self) -> IpcResult<()> {
            let result = self.send(json!({}), 2);
            self.socket = None;
            result
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "publishes and clears a diagnostic activity in the local Discord client"]
    fn live_discord_accepts_listening_and_playing() {
        for language in ["ru", "en"] {
            let mut client = Client::new(application_id(language)).unwrap();
            client.connect().unwrap();
            for mode in ["listening", "playing"] {
                let result = publish(
                    &mut client,
                    prepare_activity(
                        json!({"name": "Яндекс Музыка", "details": "Проверка RPC", "state": "Диагностика плеера"}),
                        mode,
                    ),
                    &format!("diagnostic-{language}-{mode}"),
                );
                let _ = client.clear_activity();
                result.unwrap();
            }
            let _ = client.close();
        }
    }

    #[test]
    fn music_application_and_activity_format_match_the_selected_settings() {
        assert_ne!(application_id("ru"), application_id("en"));
        for (mode, kind) in [("playing", 0), ("listening", 2)] {
            let activity = prepare_activity(
                json!({
                    "name": "Artist — Song", "details": "Song", "state": "Artist",
                    "assets": {"large_image": "https://example.com/cover.png"},
                    "timestamps": {"start": 10, "end": 100},
                    "buttons": [{"label": "Track", "url": "https://example.com/track"}]
                }),
                mode,
            );
            assert_eq!(activity["type"], kind);
            assert_eq!(activity["name"], "Artist — Song");
            assert_eq!(activity["details"], "Song");
            assert_eq!(activity["state"], "Artist");
            assert_eq!(activity["instance"], true);
            assert_eq!(activity["status_display_type"], 0);
            assert!(activity["assets"].is_object());
            assert!(activity["timestamps"].is_object());
            assert!(activity["buttons"].is_array());
        }
    }

    #[test]
    fn short_and_empty_metadata_still_meet_discord_text_requirements() {
        assert_eq!(discord_text(" Я ", "Музыка"), "Я\u{200b}");
        assert_eq!(discord_text(" ", "Музыка"), "Музыка");
        assert_eq!(
            discord_text(&"я".repeat(140), "Музыка").chars().count(),
            128
        );
    }

    #[cfg(windows)]
    #[test]
    fn binary_ping_is_echoed_and_oversized_incoming_frames_are_rejected() {
        struct Pipe {
            id: String,
            incoming: std::io::Cursor<Vec<u8>>,
            outgoing: Vec<u8>,
        }
        type IpcResult<T> = std::result::Result<T, Box<dyn std::error::Error>>;
        impl DiscordIpc for Pipe {
            fn get_client_id(&self) -> &String {
                &self.id
            }
            fn connect_ipc(&mut self) -> IpcResult<()> {
                Ok(())
            }
            fn close(&mut self) -> IpcResult<()> {
                Ok(())
            }
            fn read(&mut self, buffer: &mut [u8]) -> IpcResult<()> {
                use std::io::Read;
                self.incoming.read_exact(buffer)?;
                Ok(())
            }
            fn write(&mut self, bytes: &[u8]) -> IpcResult<()> {
                self.outgoing.extend_from_slice(bytes);
                Ok(())
            }
        }
        let ping = [0xff, 0x00, 0x20];
        let reply = json!({"cmd": "SET_ACTIVITY", "nonce": "test", "data": {"type": 0}});
        let mut incoming = encode_frame(3, &ping).unwrap();
        incoming.extend(encode_frame(1, &serde_json::to_vec(&reply).unwrap()).unwrap());
        let mut pipe = Pipe {
            id: "test".into(),
            incoming: std::io::Cursor::new(incoming),
            outgoing: vec![],
        };
        assert_eq!(receive(&mut pipe).unwrap(), (1, reply));
        assert_eq!(pipe.outgoing, encode_frame(4, &ping).unwrap());

        let mut oversized = 1u32.to_le_bytes().to_vec();
        oversized.extend_from_slice(&((MAX_PAYLOAD + 1) as u32).to_le_bytes());
        pipe.incoming = std::io::Cursor::new(oversized);
        assert!(
            receive(&mut pipe)
                .unwrap_err()
                .to_string()
                .contains("слишком большой")
        );
    }

    #[test]
    fn frames_have_a_little_endian_header_and_enforce_payload_limit() {
        let frame = encode_frame(1, br#"{"cmd":"SET_ACTIVITY"}"#).unwrap();
        assert_eq!(&frame[..4], &1u32.to_le_bytes());
        assert_eq!(
            u32::from_le_bytes(frame[4..8].try_into().unwrap()) as usize,
            frame.len() - 8
        );
        assert_eq!(&frame[8..], br#"{"cmd":"SET_ACTIVITY"}"#);
        assert!(encode_frame(1, &vec![0; MAX_PAYLOAD + 1]).is_err());
    }

    #[test]
    fn ipc_errors_and_missing_confirmation_are_not_reported_as_success() {
        assert!(
            validate_reply(&json!({
                "cmd": "SET_ACTIVITY", "evt": "ERROR",
                "data": {"code": 4000, "message": "Invalid activity"}
            }))
            .unwrap_err()
            .to_string()
            .contains("Invalid activity")
        );
        assert!(
            validate_reply(&json!({
                "cmd": "SET_ACTIVITY", "evt": null, "data": null
            }))
            .is_err()
        );
        assert!(
            validate_reply(&json!({
                "cmd": "SET_ACTIVITY", "evt": null, "data": {"type": 2}
            }))
            .is_ok()
        );
    }
}

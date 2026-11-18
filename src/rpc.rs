use anyhow::{Result, bail, ensure};
use discord_rich_presence::DiscordIpc;
use serde_json::{Value, json};

#[cfg(not(windows))]
pub use discord_rich_presence::DiscordIpcClient as Client;
#[cfg(windows)]
pub use windows_client::Client;

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
        let mut client = Client::new("1124055337234858005").unwrap();
        client.connect().unwrap();
        for kind in [2, 0] {
            let result = publish(
                &mut client,
                json!({"type": kind, "details": "Проверка RPC", "state": "Диагностика плеера"}),
                &format!("diagnostic-{kind}"),
            );
            let _ = client.clear_activity();
            result.unwrap();
        }
        let _ = client.close();
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

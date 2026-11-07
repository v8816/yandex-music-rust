use anyhow::{Context, Result, bail, ensure};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use reqwest::Url;

const MUSIC_CLIENT_ID: &str = "23cabbbdc6cd418abb4b39c32c41195d";

pub struct BrowserLogin {
    url: Url,
    state: String,
    started: std::time::Instant,
}

impl BrowserLogin {
    pub fn new() -> Result<Self> {
        let mut bytes = [0; 24];
        getrandom::fill(&mut bytes)
            .map_err(|_| anyhow::anyhow!("Не удалось начать безопасный вход"))?;
        let state = URL_SAFE_NO_PAD.encode(bytes);
        let mut url = Url::parse("https://oauth.yandex.ru/authorize")?;
        url.query_pairs_mut()
            .append_pair("response_type", "token")
            .append_pair("client_id", MUSIC_CLIENT_ID)
            .append_pair("redirect_uri", "https://music.yandex.ru/")
            .append_pair("state", &state);
        Ok(Self {
            url,
            state,
            started: std::time::Instant::now(),
        })
    }

    pub fn url(&self) -> &str {
        self.url.as_str()
    }

    pub fn token(&self, pasted: &str) -> Result<String> {
        ensure!(
            self.started.elapsed().as_secs() < 900,
            "Время входа истекло. Откройте браузер заново."
        );
        ensure!(pasted.len() <= 16384, "Ссылка слишком длинная");
        let url = Url::parse(pasted.trim())
            .map_err(|_| anyhow::anyhow!("Скопируйте полную ссылку из адресной строки браузера"))?;
        ensure!(
            url.scheme() == "https"
                && url.host_str() == Some("music.yandex.ru")
                && url.port().is_none()
                && url.username().is_empty()
                && url.password().is_none(),
            "Нужна ссылка с music.yandex.ru после входа через эту кнопку"
        );
        let fragment = url.fragment().context("В ссылке нет данных входа. Откройте Яндекс ID заново и скопируйте адрес сразу после подтверждения.")?;
        let parsed = Url::parse(&format!("https://music.yandex.ru/?{fragment}"))
            .map_err(|_| anyhow::anyhow!("Не удалось прочитать ссылку входа"))?;
        let mut token = None;
        let mut state = None;
        for (name, value) in parsed.query_pairs() {
            match name.as_ref() {
                "error" => bail!("Яндекс не выдал доступ. Начните вход заново."),
                "state" if state.is_none() => state = Some(value.into_owned()),
                "access_token" if token.is_none() => token = Some(value.into_owned()),
                "state" | "access_token" => bail!("В ссылке повторяются параметры входа"),
                _ => {}
            }
        }
        ensure!(
            state.as_deref() == Some(self.state.as_str()),
            "Ссылка относится к другому входу. Откройте Яндекс ID заново."
        );
        let token = token.context("В ссылке нет данных входа")?;
        ensure!(
            !token.is_empty() && token.len() <= 2560 && token.bytes().all(|b| b.is_ascii_graphic()),
            "Яндекс вернул некорректные данные входа"
        );
        Ok(token)
    }
}

#[cfg(windows)]
mod storage {
    use anyhow::{Result, ensure};
    use windows::{
        Win32::{
            Foundation::ERROR_NOT_FOUND,
            Security::Credentials::{
                CRED_PERSIST_LOCAL_MACHINE, CRED_TYPE_GENERIC, CREDENTIALW, CredDeleteW, CredFree,
                CredReadW, CredWriteW,
            },
        },
        core::{PWSTR, w},
    };

    const TARGET: windows::core::PCWSTR = w!("YandexMusicRust/OAuth");

    pub fn read() -> Result<Option<String>> {
        let mut pointer = std::ptr::null_mut();
        unsafe {
            match CredReadW(TARGET, CRED_TYPE_GENERIC, 0, &mut pointer) {
                Ok(()) => {
                    let credential = &*pointer;
                    let result = if credential.CredentialBlobSize == 0 {
                        Ok(None)
                    } else {
                        let bytes = std::slice::from_raw_parts(
                            credential.CredentialBlob,
                            credential.CredentialBlobSize as usize,
                        );
                        std::str::from_utf8(bytes).map(|s| Some(s.to_owned()))
                    };
                    CredFree(pointer.cast());
                    Ok(result?)
                }
                Err(error) if error.code() == ERROR_NOT_FOUND.to_hresult() => Ok(None),
                Err(error) => Err(error.into()),
            }
        }
    }

    pub fn save(token: &str) -> Result<()> {
        ensure!(
            token.len() <= 2560,
            "Сессия слишком длинная для хранилища Windows"
        );
        let mut name: Vec<u16> = "YandexMusicRust/OAuth".encode_utf16().chain([0]).collect();
        let mut blob = token.as_bytes().to_vec();
        let credential = CREDENTIALW {
            Type: CRED_TYPE_GENERIC,
            TargetName: PWSTR(name.as_mut_ptr()),
            CredentialBlobSize: blob.len() as u32,
            CredentialBlob: blob.as_mut_ptr(),
            Persist: CRED_PERSIST_LOCAL_MACHINE,
            ..Default::default()
        };
        let result = unsafe { CredWriteW(&credential, 0) };
        blob.fill(0);
        Ok(result?)
    }

    pub fn clear() -> Result<()> {
        match unsafe { CredDeleteW(TARGET, CRED_TYPE_GENERIC, 0) } {
            Ok(()) => Ok(()),
            Err(error) if error.code() == ERROR_NOT_FOUND.to_hresult() => Ok(()),
            Err(error) => Err(error.into()),
        }
    }
}

#[cfg(not(windows))]
mod storage {
    pub fn read() -> anyhow::Result<Option<String>> {
        Ok(None)
    }
    pub fn save(_: &str) -> anyhow::Result<()> {
        anyhow::bail!("Сохранение сессии поддерживается в Windows")
    }
    pub fn clear() -> anyhow::Result<()> {
        Ok(())
    }
}

pub use storage::{clear, read, save};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_only_the_expected_music_redirect() {
        let login = BrowserLogin::new().unwrap();
        let valid = format!(
            "https://music.yandex.ru/#access_token=test-token&state={}",
            login.state
        );
        assert_eq!(login.token(&valid).unwrap(), "test-token");
        assert!(
            login
                .token(&valid.replace("music.yandex.ru", "example.com"))
                .is_err()
        );
        assert!(login.token(&valid.replace(&login.state, "wrong")).is_err());
        assert!(login.token(&format!("{valid}&access_token=other")).is_err());
    }
}

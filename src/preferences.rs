use serde::{Deserialize, Serialize};
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct RpcSettings {
    pub title: String,
    pub language: String,
    pub links: bool,
}
impl Default for RpcSettings {
    fn default() -> Self {
        Self {
            title: "artist_track".into(),
            language: "ru".into(),
            links: true,
        }
    }
}
impl RpcSettings {
    fn path() -> std::path::PathBuf {
        std::env::var_os("LOCALAPPDATA")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(std::env::temp_dir)
            .join("YandexMusicRust")
            .join("rpc.json")
    }
    pub fn load() -> Self {
        std::fs::read(Self::path())
            .ok()
            .and_then(|b| serde_json::from_slice::<Self>(&b).ok())
            .filter(|s| s.validate().is_ok())
            .unwrap_or_default()
    }
    pub fn validate(&self) -> anyhow::Result<()> {
        anyhow::ensure!(
            ["app", "artist", "artist_track", "track"].contains(&self.title.as_str())
                && ["ru", "en"].contains(&self.language.as_str()),
            "Некорректные настройки RPC"
        );
        Ok(())
    }
    pub fn save(&self) -> anyhow::Result<()> {
        let path = Self::path();
        std::fs::create_dir_all(path.parent().unwrap())?;
        std::fs::write(path, serde_json::to_vec(self)?)?;
        Ok(())
    }
    pub fn display_name(&self, artist: &str, title: &str) -> String {
        match self.title.as_str() {
            "artist" => artist.into(),
            "track" => title.into(),
            "artist_track" => format!("{artist} — {title}"),
            _ => "Яндекс Музыка".into(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WaveStation {
    pub seed_track: Option<u64>,
    pub title: String,
}

impl Default for WaveStation {
    fn default() -> Self {
        Self {
            seed_track: None,
            title: "Моя волна".into(),
        }
    }
}

impl WaveStation {
    fn path() -> std::path::PathBuf {
        RpcSettings::path().with_file_name("pinned-wave.json")
    }

    pub fn load_pin() -> Option<Self> {
        std::fs::read(Self::path())
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
    }

    pub fn save_pin(station: Option<&Self>) -> anyhow::Result<()> {
        let path = Self::path();
        let dir = path.parent().expect("preferences directory");
        std::fs::create_dir_all(dir)?;
        let mut temp = tempfile::NamedTempFile::new_in(dir)?;
        serde_json::to_writer(&mut temp, &station)?;
        temp.persist(path)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn all_status_titles_show_the_selected_metadata() {
        let mut settings = RpcSettings::default();
        for (mode, expected) in [
            ("app", "Яндекс Музыка"),
            ("artist", "Автор"),
            ("artist_track", "Автор — Песня"),
            ("track", "Песня"),
        ] {
            settings.title = mode.into();
            assert_eq!(settings.display_name("Автор", "Песня"), expected);
            assert!(settings.validate().is_ok());
        }
        settings.title = "invalid".into();
        assert!(settings.validate().is_err());
    }
    #[test]
    fn settings_round_trip_preserves_language_and_link_choice() {
        let settings = RpcSettings {
            title: "track".into(),
            language: "en".into(),
            links: false,
        };
        let decoded: RpcSettings =
            serde_json::from_slice(&serde_json::to_vec(&settings).unwrap()).unwrap();
        assert_eq!(decoded.title, "track");
        assert_eq!(decoded.language, "en");
        assert!(!decoded.links);
    }
}

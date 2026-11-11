use crate::stream::{self, Reader, SharedBuffer};
use anyhow::{Context, Result, bail, ensure};
use base64::{Engine, engine::general_purpose::STANDARD_NO_PAD};
use hmac::{Hmac, Mac};
use reqwest::{Url, blocking::Client};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::Sha256;
use std::{
    collections::HashMap,
    io::Read,
    sync::{Arc, OnceLock},
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

const API: &str = "https://api.music.yandex.net/";
const MAX_AUDIO: u64 = 120 * 1024 * 1024;
const PREFIX: u64 = 256 * 1024;
const LIKED_KIND: &str = "3";
const LONG_TIMEOUT: Duration = Duration::from_secs(45);
const TRACKS_CHUNK: usize = 200;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Track {
    #[serde(deserialize_with = "track_id")]
    pub id: u64,
    pub title: String,
    #[serde(default)]
    pub artists: Vec<Artist>,
    #[serde(default)]
    pub albums: Vec<Album>,
    #[serde(default, rename = "durationMs")]
    pub duration_ms: u64,
    #[serde(default, rename = "coverUri")]
    pub cover_uri: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub available: Option<bool>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Artist {
    #[serde(default)]
    pub id: u64,
    pub name: String,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct ArtistInfo {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default, rename = "coverUri")]
    pub cover_uri: Option<String>,
    #[serde(default)]
    pub likes: u64,
    #[serde(default)]
    pub listeners: u64,
    #[serde(default)]
    pub tracks: Vec<Track>,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Album {
    pub id: u64,
    #[serde(default, rename = "coverUri")]
    pub cover_uri: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Playlist {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub owner: String,
    #[serde(default, rename = "trackCount")]
    pub track_count: u64,
    #[serde(default, rename = "coverUri")]
    pub cover_uri: Option<String>,
}

pub struct AudioStream {
    pub reader: Reader,
    pub buffer: Arc<SharedBuffer>,
    pub codec: String,
}

impl Track {
    pub fn cover(&self) -> Option<&str> {
        self.cover_uri
            .as_deref()
            .filter(|s| !s.is_empty())
            .or_else(|| {
                self.albums
                    .iter()
                    .find_map(|album| album.cover_uri.as_deref().filter(|s| !s.is_empty()))
            })
    }
    pub fn artist(&self) -> String {
        self.artists
            .iter()
            .map(|a| a.name.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    }
    pub fn url(&self) -> String {
        match self.albums.first() {
            Some(album) => format!(
                "https://music.yandex.ru/album/{}/track/{}",
                album.id, self.id
            ),
            None => format!("https://music.yandex.ru/track/{}", self.id),
        }
    }
}

#[derive(Clone)]
pub struct Api {
    http: Client,
    token: String,
    uid: Option<u64>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WaveBatch {
    pub radio_session_id: Option<String>,
    pub batch_id: String,
    #[serde(default)]
    unknown_session: bool,
    sequence: Vec<WaveItem>,
}

#[derive(Deserialize)]
struct WaveItem {
    track: Option<Track>,
}

fn track_id<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> std::result::Result<u64, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Id {
        Number(u64),
        Text(String),
    }
    match Id::deserialize(deserializer)? {
        Id::Number(id) => Ok(id),
        Id::Text(id) => id.parse().map_err(serde::de::Error::custom),
    }
}

fn list_items(body: &Value, key: &str) -> Vec<Value> {
    body.pointer(&format!("/{key}"))
        .and_then(Value::as_array)
        .cloned()
        .or_else(|| body.as_array().cloned())
        .unwrap_or_default()
}

fn track_of(item: &Value) -> Option<Track> {
    let track = item.get("track").unwrap_or(item);
    serde_json::from_value::<Track>(track.clone())
        .ok()
        .filter(|track| !track.title.trim().is_empty())
}

fn item_id(item: &Value) -> Option<u64> {
    let track = item.get("track").unwrap_or(item);
    let id = &track["id"];
    id.as_u64()
        .or_else(|| id.as_str().and_then(|id| id.parse().ok()))
        .filter(|id| *id > 0)
}

fn tracks_from_items(items: &[Value]) -> Vec<Track> {
    items.iter().filter_map(track_of).collect()
}

fn playlists_from_items(items: Vec<Value>) -> Vec<Playlist> {
    items
        .into_iter()
        .map(|item| Playlist {
            id: item
                .pointer("/kind")
                .and_then(Value::as_u64)
                .map(|n| n.to_string())
                .or_else(|| item["id"].as_str().map(str::to_owned))
                .unwrap_or_default(),
            title: item["title"].as_str().unwrap_or("Без названия").to_owned(),
            owner: item
                .pointer("/owner/name")
                .and_then(Value::as_str)
                .or_else(|| item["owner"].as_str())
                .unwrap_or("")
                .to_owned(),
            track_count: item["trackCount"]
                .as_u64()
                .or_else(|| item["track_count"].as_u64())
                .unwrap_or(0),
            cover_uri: item
                .pointer("/cover/uri")
                .and_then(Value::as_str)
                .map(str::to_owned)
                .filter(|s| !s.is_empty()),
        })
        .collect()
}

fn tracks_from(value: Value) -> Vec<Track> {
    match value {
        Value::Array(items) => tracks_from_items(&items),
        _ => Vec::new(),
    }
}

fn kinds_from(value: &Value) -> Vec<String> {
    value
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(|kind| {
                    kind.as_u64()
                        .map(|n| n.to_string())
                        .or_else(|| kind.as_str().map(str::to_owned))
                })
                .filter(|kind| {
                    !kind.is_empty()
                        && kind
                            .bytes()
                            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
                })
                .collect()
        })
        .unwrap_or_default()
}

fn kinds_form(kinds: &[String]) -> String {
    kinds
        .iter()
        .map(|kind| format!("kinds={kind}"))
        .collect::<Vec<_>>()
        .join("&")
}

fn service_error(status: reqwest::StatusCode, body: &Value) -> String {
    let name = body.pointer("/error/name").and_then(Value::as_str);
    let message = body.pointer("/error/message").and_then(Value::as_str);
    let message = message.map(|message| message.chars().take(200).collect::<String>());
    match (name, message) {
        (Some(name), Some(message)) => {
            format!("Яндекс Музыка вернула HTTP {status}: {name} — {message}")
        }
        (None, Some(message)) => format!("Яндекс Музыка вернула HTTP {status}: {message}"),
        _ => format!("Яндекс Музыка вернула HTTP {status}"),
    }
}

impl WaveBatch {
    pub fn tracks(self) -> Vec<Track> {
        self.sequence
            .into_iter()
            .filter_map(|item| item.track.filter(|track| track.available != Some(false)))
            .collect()
    }
}

pub(crate) fn http_client() -> Result<Client> {
    static CLIENT: OnceLock<Result<Client, reqwest::Error>> = OnceLock::new();
    CLIENT
        .get_or_init(|| {
            Client::builder()
                .timeout(Duration::from_secs(25))
                .connect_timeout(Duration::from_secs(5))
                .pool_max_idle_per_host(2)
                .redirect(reqwest::redirect::Policy::none())
                .build()
        })
        .as_ref()
        .cloned()
        .map_err(|error| anyhow::anyhow!("Не удалось создать HTTP-клиент: {error}"))
}

impl Api {
    pub fn new() -> Result<Self> {
        Ok(Self {
            http: http_client()?,
            token: String::new(),
            uid: None,
        })
    }

    fn request(&self, method: reqwest::Method, path: &str) -> reqwest::blocking::RequestBuilder {
        let req = self
            .http
            .request(method, format!("{API}{path}"))
            .header(
                "X-Yandex-Music-Client",
                "YandexMusicDesktopAppWindows/5.85.0",
            )
            .header("X-Yandex-Music-Without-Invocation-Info", "1")
            .header("Accept-Language", "ru");
        if self.token.is_empty() {
            req
        } else {
            req.header("Authorization", format!("OAuth {}", self.token))
        }
    }

    fn get(&self, path: &str, query: &[(&str, &str)]) -> Result<Value> {
        self.get_with_timeout(path, query, Duration::from_secs(25))
    }

    fn get_with_timeout(
        &self,
        path: &str,
        query: &[(&str, &str)],
        timeout: Duration,
    ) -> Result<Value> {
        let response = self
            .request(reqwest::Method::GET, path)
            .query(query)
            .timeout(timeout)
            .send()
            .context("Нет связи с API Яндекс Музыки")?;
        let status = response.status();
        let mut body: Value = response.json().unwrap_or(Value::Null);
        ensure!(status.is_success(), "{}", service_error(status, &body));
        ensure!(!body.is_null(), "Не удалось прочитать ответ Яндекс Музыки");
        Ok(body.get_mut("result").map(Value::take).unwrap_or(body))
    }

    pub fn wave_start(&self, seed_track: Option<u64>, queue: Vec<String>) -> Result<WaveBatch> {
        let seed =
            seed_track.map_or_else(|| "user:onyourwave".to_owned(), |id| format!("track:{id}"));
        self.wave_request(
            "rotor/session/new",
            json!({
                "seeds":[seed], "includeTracksInResponse":true, "interactive":true,
                "queue":queue
            }),
        )
    }

    pub fn wave_more(
        &self,
        session: &str,
        queue: Vec<String>,
        seed_track: Option<u64>,
    ) -> Result<WaveBatch> {
        ensure!(valid_session(session), "Некорректный номер сессии");
        let batch = self.wave_request(
            &format!("rotor/session/{session}/tracks"),
            json!({"queue":&queue}),
        )?;
        if batch.unknown_session {
            self.wave_start(seed_track, queue)
        } else {
            Ok(batch)
        }
    }

    fn wave_request(&self, path: &str, body: Value) -> Result<WaveBatch> {
        let response = self
            .request(reqwest::Method::POST, path)
            .json(&body)
            .send()
            .context("Не удалось связаться с Моей волной")?;
        ensure!(
            response.status().is_success(),
            "Моя волна вернула HTTP {}",
            response.status()
        );
        let mut response: Value = response.json()?;
        let result = response
            .get_mut("result")
            .map(Value::take)
            .unwrap_or(response);
        serde_json::from_value(result).context("Не удалось прочитать подборку Моей волны")
    }

    pub fn wave_feedback(&self, session: &str, event: &Value) {
        if !valid_session(session) {
            return;
        }
        let _ = self
            .request(
                reqwest::Method::POST,
                &format!("rotor/session/{session}/feedback"),
            )
            .timeout(Duration::from_secs(4))
            .json(event)
            .send();
    }

    pub fn login(&mut self, token: String) -> Result<String> {
        let old = std::mem::replace(&mut self.token, token.trim().to_owned());
        match self.get("account/status", &[]).and_then(|v| {
            let account = &v["account"];
            let uid = account["uid"]
                .as_u64()
                .context("Ответ не содержит идентификатор аккаунта")?;
            let name = ["displayName", "fullName", "login"]
                .into_iter()
                .find_map(|key| account[key].as_str().filter(|s| !s.trim().is_empty()))
                .unwrap_or("Яндекс ID")
                .chars()
                .take(70)
                .collect();
            Ok((uid, name))
        }) {
            Ok((uid, name)) => {
                self.uid = Some(uid);
                Ok(name)
            }
            Err(error) => {
                self.token = old;
                Err(error)
            }
        }
    }

    pub fn logout(&mut self) {
        self.uid = None;
        self.token.clear();
    }

    pub fn search(&self, query: &str) -> Result<Vec<Track>> {
        let v = self.get(
            "search",
            &[("text", query), ("type", "track"), ("page", "0")],
        )?;
        let tracks: Vec<Track> = serde_json::from_value(
            v.pointer("/tracks/results")
                .cloned()
                .unwrap_or(Value::Array(vec![])),
        )?;
        Ok(tracks
            .into_iter()
            .filter(|track| track.available != Some(false))
            .collect())
    }

    pub fn track(&self, id: u64) -> Result<Track> {
        let v = self.get(&format!("tracks/{id}"), &[])?;
        Ok(serde_json::from_value(
            v.get(0).context("Трек не найден")?.clone(),
        )?)
    }

    pub fn recommendations(&self) -> Result<Vec<Track>> {
        let uid = self.uid.context("Сначала войдите в аккаунт")?;
        let daily = self.get("playlists/personal/daily", &[])?;
        let playlist = daily
            .get("data")
            .filter(|data| data.is_object())
            .unwrap_or(&daily);
        let body;
        let items = if let Some(items) = playlist["tracks"]
            .as_array()
            .filter(|items| !items.is_empty())
        {
            items
        } else {
            let kind = playlist["kind"]
                .as_u64()
                .context("Плейлист дня ещё не готов")?;
            let owner = playlist["uid"]
                .as_u64()
                .or_else(|| playlist.pointer("/owner/uid").and_then(Value::as_u64))
                .unwrap_or(uid);
            body = self.get(&format!("users/{owner}/playlists/{kind}"), &[])?;
            body["tracks"]
                .as_array()
                .context("Не удалось получить треки Плейлиста дня")?
        };
        let mut seen = std::collections::HashSet::new();
        let ids = items
            .iter()
            .filter_map(item_id)
            .filter(|id| seen.insert(*id))
            .take(24)
            .collect::<Vec<_>>();
        Ok(self
            .tracks_by_ids(&ids)?
            .into_iter()
            .filter(|track| track.available != Some(false))
            .collect())
    }

    pub fn library(&self) -> Result<Vec<Track>> {
        let uid = self.uid.context("Сначала войдите в аккаунт")?;
        let list = self.get(&format!("users/{uid}/likes/tracks"), &[])?;
        let entries = list
            .pointer("/library/tracks")
            .and_then(Value::as_array)
            .context("Не удалось получить список любимых треков")?;
        let ids = entries.iter().filter_map(item_id).collect::<Vec<_>>();
        if ids.is_empty() {
            return Ok(vec![]);
        }
        self.tracks_by_ids(&ids)
    }

    fn tracks_by_ids(&self, ids: &[u64]) -> Result<Vec<Track>> {
        let mut tracks = Vec::with_capacity(ids.len());
        for chunk in ids.chunks(TRACKS_CHUNK) {
            let list = chunk
                .iter()
                .map(u64::to_string)
                .collect::<Vec<_>>()
                .join(",");
            let response = self
                .request(reqwest::Method::POST, "tracks")
                .form(&[("track-ids", list)])
                .send()
                .context("Не удалось получить треки")?;
            let status = response.status();
            let mut body: Value = response.json().unwrap_or(Value::Null);
            ensure!(
                status.is_success(),
                "Не удалось получить треки: {}",
                service_error(status, &body)
            );
            let result = body.get_mut("result").map(Value::take);
            tracks.extend(tracks_from(result.unwrap_or(body)));
        }
        Ok(tracks)
    }

    fn liked_tracks(&self) -> Result<Vec<Track>> {
        let uid = self.uid.context("Сначала войдите в аккаунт")?;
        let list = self.get(&format!("users/{uid}/likes/tracks"), &[])?;
        let ids = list
            .pointer("/library/tracks")
            .and_then(Value::as_array)
            .map(|entries| entries.iter().filter_map(item_id).collect::<Vec<_>>())
            .unwrap_or_default();
        if ids.is_empty() {
            return Ok(vec![]);
        }
        self.tracks_by_ids(&ids)
    }

    pub fn artist(&self, id: u64) -> Result<ArtistInfo> {
        ensure!(id > 0, "Некорректный идентификатор исполнителя");
        let info = self.get(&format!("artists/{id}/info"), &[])?;
        let tracks = self
            .get(
                &format!("artists/{id}/tracks"),
                &[("page", "0"), ("page-size", "50")],
            )
            .map(|body| tracks_from_items(&list_items(&body, "tracks")))
            .unwrap_or_default();
        Ok(ArtistInfo {
            name: info
                .pointer("/artist/name")
                .and_then(Value::as_str)
                .unwrap_or("Исполнитель")
                .to_owned(),
            description: info["description"].as_str().unwrap_or("").to_owned(),
            cover_uri: info
                .pointer("/artist/cover/uri")
                .and_then(Value::as_str)
                .filter(|uri| !uri.is_empty())
                .map(str::to_owned),
            likes: info["likesCount"].as_u64().unwrap_or(0),
            listeners: info
                .pointer("/stats/lastMonthListeners")
                .and_then(Value::as_u64)
                .unwrap_or(0),
            tracks,
        })
    }

    pub fn liked_ids(&self) -> Result<std::collections::HashSet<u64>> {
        let uid = self.uid.context("Сначала войдите")?;
        let body = self.get(&format!("users/{uid}/likes/tracks"), &[])?;
        Ok(body
            .pointer("/library/tracks")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|t| t["id"].as_u64().or_else(|| t["id"].as_str()?.parse().ok()))
            .collect())
    }

    fn playlist_post(&self, path: &str, form: &[(&str, String)]) -> Result<Value> {
        let response = self
            .request(reqwest::Method::POST, path)
            .form(form)
            .send()?;
        let status = response.status();
        let body: Value = response.json()?;
        ensure!(
            status.is_success() && body.get("error").is_none(),
            "{}",
            service_error(status, &body)
        );
        Ok(body.get("result").cloned().unwrap_or(body))
    }

    pub fn create_playlist(&self, title: &str) -> Result<Value> {
        ensure!(
            !title.trim().is_empty() && title.chars().count() <= 100,
            "Название: 1–100 символов"
        );
        let uid = self.uid.context("Сначала войдите")?;
        self.playlist_post(
            &format!("users/{uid}/playlists/create"),
            &[
                ("title", title.trim().into()),
                ("visibility", "private".into()),
            ],
        )
    }

    pub fn add_to_playlist(&self, kind: &str, id: u64) -> Result<Value> {
        ensure!(
            !kind.is_empty() && kind.bytes().all(|b| b.is_ascii_digit()) && kind != "3",
            "Выберите собственный плейлист"
        );
        let uid = self.uid.context("Сначала войдите")?;
        let body = self.get(&format!("users/{uid}/playlists/{kind}"), &[])?;
        let revision = body["revision"].as_u64().context("Нет ревизии плейлиста")?;
        let track = self.track(id)?;
        let album = track.albums.first().context("Нет альбома трека")?.id;
        let diff = json!([{"op":"insert","at":0,"tracks":[{"id":id.to_string(),"albumId":album.to_string()}]}]);
        self.playlist_post(
            &format!("users/{uid}/playlists/{kind}/change-relative"),
            &[
                ("revision", revision.to_string()),
                ("diff", diff.to_string()),
            ],
        )
    }

    pub fn like(&self, track: u64, liked: bool) -> Result<()> {
        let uid = self.uid.context("Сначала войдите в аккаунт")?;
        let action = if liked { "add-multiple" } else { "remove" };
        let res = self
            .request(
                reqwest::Method::POST,
                &format!("users/{uid}/likes/tracks/{action}"),
            )
            .form(&[("track-ids", track.to_string())])
            .send()
            .context("Не удалось обновить отметку")?;
        let status = res.status();
        let body: Value = res.json().unwrap_or(Value::Null);
        ensure!(
            status.is_success(),
            "Не удалось обновить отметку: {}",
            service_error(status, &body)
        );
        Ok(())
    }

    pub fn playlists(&self) -> Result<Vec<Playlist>> {
        let uid = self.uid.context("Сначала войдите в аккаунт")?;
        if let Ok(body) =
            self.get_with_timeout(&format!("users/{uid}/playlists/list"), &[], LONG_TIMEOUT)
            && (body.get("playlists").is_some() || body.is_array())
        {
            return Ok(playlists_from_items(list_items(&body, "playlists")));
        }
        let kinds = kinds_from(&self.get(&format!("users/{uid}/playlists/list/kinds"), &[])?);
        if kinds.is_empty() {
            return Ok(vec![]);
        }
        let response = self
            .request(reqwest::Method::POST, &format!("users/{uid}/playlists"))
            .header(
                reqwest::header::CONTENT_TYPE,
                "application/x-www-form-urlencoded",
            )
            .body(kinds_form(&kinds))
            .send()
            .context("Не удалось получить плейлисты")?;
        let status = response.status();
        let body: Value = response.json().unwrap_or(Value::Null);
        ensure!(
            status.is_success(),
            "Не удалось получить плейлисты: {}",
            service_error(status, &body)
        );
        let result = body.get("result").cloned().unwrap_or(body);
        Ok(playlists_from_items(list_items(&result, "playlists")))
    }

    pub fn playlist_tracks(&self, playlist_id: &str) -> Result<Vec<Track>> {
        let uid = self.uid.context("Сначала войдите в аккаунт")?;
        ensure!(
            !playlist_id.is_empty()
                && playlist_id
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'),
            "Некорректный идентификатор плейлиста"
        );
        let likes = playlist_id == LIKED_KIND;
        let body = match self.get_with_timeout(
            &format!("users/{uid}/playlists/{playlist_id}"),
            &[],
            LONG_TIMEOUT,
        ) {
            Ok(body) => body,
            Err(_) if likes => return self.liked_tracks(),
            Err(error) => return Err(error),
        };
        let items = list_items(&body, "tracks");
        let likes = likes || body.pointer("/kind").and_then(Value::as_u64) == Some(3);
        if items.is_empty() {
            return if likes {
                self.liked_tracks()
            } else {
                Ok(vec![])
            };
        }
        let mut tracks = Vec::with_capacity(items.len());
        let mut missing = Vec::new();
        for item in &items {
            if let Some(track) = track_of(item) {
                tracks.push(track);
            } else if let Some(id) = item_id(item) {
                tracks.push(Track {
                    id,
                    title: String::new(),
                    artists: Vec::new(),
                    albums: Vec::new(),
                    duration_ms: 0,
                    cover_uri: None,
                    available: None,
                });
                missing.push(id);
            }
        }
        if !missing.is_empty() {
            let found = self
                .tracks_by_ids(&missing)?
                .into_iter()
                .map(|track| (track.id, track))
                .collect::<HashMap<_, _>>();
            tracks = tracks
                .into_iter()
                .filter_map(|track| {
                    if track.title.is_empty() {
                        found.get(&track.id).cloned()
                    } else {
                        Some(track)
                    }
                })
                .collect();
        }
        Ok(tracks)
    }

    pub fn stream_audio(
        &self,
        track: u64,
        lossless: bool,
        keep: impl Fn() -> bool + Send + 'static,
    ) -> Result<AudioStream> {
        let (url, codec) = self.download_url(track, lossless)?;
        let mut response = self.open_audio(url)?;
        let total = response.content_length();
        let buffer = SharedBuffer::new(MAX_AUDIO as usize, total)
            .context("Не удалось создать временный аудиокэш")?;
        buffer.set_patience(stream::PROBE_PATIENCE);
        let prefix = PREFIX.min(total.unwrap_or(u64::MAX)) as usize;
        let mut chunk = vec![0u8; 64 * 1024];
        let mut downloaded = 0usize;
        while downloaded < prefix {
            if !keep() {
                buffer.finish();
                bail!("Загрузка трека отменена");
            }
            let read = response.read(&mut chunk)?;
            if read == 0 {
                break;
            }
            ensure!(buffer.push(&chunk[..read]), "Аудиофайл слишком большой");
            downloaded += read;
        }
        let tail = buffer.clone();
        thread::spawn(move || {
            let mut chunk = vec![0u8; 64 * 1024];
            loop {
                if !keep() {
                    tail.finish();
                    return;
                }
                match response.read(&mut chunk) {
                    Ok(0) => {
                        tail.finish();
                        return;
                    }
                    Ok(read) => {
                        if !tail.push(&chunk[..read]) {
                            return;
                        }
                    }
                    Err(error) => {
                        tail.fail(error.to_string());
                        return;
                    }
                }
            }
        });
        Ok(AudioStream {
            reader: buffer.reader(),
            buffer,
            codec,
        })
    }

    fn download_url(&self, track: u64, lossless: bool) -> Result<(Url, String)> {
        ensure!(
            self.uid.is_some(),
            "Войдите в аккаунт Яндекс Музыки в настройках"
        );
        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)?
            .as_secs()
            .to_string();
        let id = track.to_string();
        let (quality, codecs) = if lossless {
            ("lossless", "flac,mp3,aac")
        } else {
            ("hq", "mp3,aac")
        };
        let transport = "raw";
        let mut signer = Hmac::<Sha256>::new_from_slice(b"kzqU4XhfCaY6B6JTHODeq5")?;
        signer
            .update(format!("{ts}{id}{quality}{}{transport}", codecs.replace(',', "")).as_bytes());
        let sign = STANDARD_NO_PAD.encode(signer.finalize().into_bytes());
        let info = self.get(
            "get-file-info",
            &[
                ("trackId", &id),
                ("ts", &ts),
                ("quality", quality),
                ("codecs", codecs),
                ("transports", transport),
                ("sign", &sign),
            ],
        )?;
        let info = info
            .get("downloadInfo")
            .context("Сервис не выдал аудиофайл. Проверьте подписку и доступность трека")?;
        if info["key"].as_str().is_some_and(|key| !key.is_empty()) {
            bail!("Сервис вернул формат, который эта сборка не воспроизводит");
        }
        let codec = info["codec"].as_str().unwrap_or("audio").to_owned();
        let url = Url::parse(info["url"].as_str().context("В ответе нет адреса аудио")?)?;
        ensure!(
            url.scheme() == "https" && url.host_str().is_some_and(|h| h.ends_with(".yandex.net")),
            "Неожиданный адрес аудио"
        );
        Ok((url, codec))
    }

    fn open_audio(&self, url: Url) -> Result<reqwest::blocking::Response> {
        let response = self
            .http
            .get(url)
            .send()
            .map_err(|_| anyhow::anyhow!("Не удалось загрузить аудио"))?;
        ensure!(
            response.status().is_success(),
            "Сервер аудио вернул HTTP {}",
            response.status()
        );
        ensure!(
            response.content_length().unwrap_or(0) <= MAX_AUDIO,
            "Аудиофайл слишком большой"
        );
        Ok(response)
    }
}

fn valid_session(session: &str) -> bool {
    !session.is_empty()
        && session.len() <= 512
        && session
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn null_lists_become_empty_without_errors() {
        assert!(list_items(&Value::Null, "playlists").is_empty());
        assert!(list_items(&json!({"playlists": null}), "playlists").is_empty());
        assert!(list_items(&json!({"result": {"playlists": null}}), "playlists").is_empty());
        assert!(tracks_from_items(&list_items(&json!({"tracks": null}), "tracks")).is_empty());
        assert!(tracks_from(Value::Null).is_empty());
        assert!(tracks_from(json!({"unexpected": true})).is_empty());
    }

    #[test]
    fn playlist_payload_converts_to_a_list() {
        let body = json!({
            "playlists": [{
                "kind": 3,
                "title": "Дорожное",
                "trackCount": 12,
                "owner": {"name": "Я"},
                "cover": {"uri": "avatars.yandex.net/get-music-content/1/%%"}
            }]
        });
        let playlists = playlists_from_items(list_items(&body, "playlists"));
        assert_eq!(playlists.len(), 1);
        assert_eq!(playlists[0].id, "3");
        assert_eq!(playlists[0].title, "Дорожное");
        assert_eq!(playlists[0].owner, "Я");
        assert_eq!(playlists[0].track_count, 12);
    }

    #[test]
    fn playlist_tracks_skip_broken_entries() {
        let body = json!({
            "tracks": [
                {"track": {"id": "42", "title": "Трек", "durationMs": 1000}},
                {"track": null},
                {"id": 7, "title": "Без обложки"}
            ]
        });
        let tracks = tracks_from_items(&list_items(&body, "tracks"));
        assert_eq!(tracks.len(), 2);
        assert_eq!(tracks[0].id, 42);
        assert_eq!(tracks[1].id, 7);
    }

    #[test]
    fn bare_arrays_are_accepted() {
        let body = json!([{"kind": 5, "title": "Мне нравится"}]);
        let playlists = playlists_from_items(list_items(&body, "playlists"));
        assert_eq!(playlists.len(), 1);
        assert_eq!(playlists[0].id, "5");
    }

    #[test]
    fn kinds_are_read_and_sent_as_repeated_parameters() {
        let kinds = kinds_from(&json!([3, "5", "b2f9-uuid", 0.5, null, ""]));
        assert_eq!(kinds, vec!["3", "5", "b2f9-uuid"]);
        assert_eq!(kinds_form(&kinds), "kinds=3&kinds=5&kinds=b2f9-uuid");
        assert!(kinds_from(&Value::Null).is_empty());
    }

    #[test]
    fn service_error_keeps_the_reason_from_the_api() {
        let body = json!({"error":{"name":"bad-request","message":"owner must be specified"}});
        let text = service_error(reqwest::StatusCode::BAD_REQUEST, &body);
        assert!(text.contains("400"), "{text}");
        assert!(text.contains("bad-request"), "{text}");
        assert!(text.contains("owner must be specified"), "{text}");
        assert_eq!(
            service_error(reqwest::StatusCode::BAD_REQUEST, &Value::Null),
            "Яндекс Музыка вернула HTTP 400 Bad Request"
        );
    }

    #[test]
    fn playlist_id_is_validated_before_the_request() {
        let mut api = Api::new().unwrap();
        api.uid = Some(1);
        let error = api.playlist_tracks("../steal").unwrap_err().to_string();
        assert_eq!(error, "Некорректный идентификатор плейлиста");
        assert!(api.playlist_tracks("").is_err());
    }

    #[test]
    fn short_entries_keep_their_ids() {
        let short = json!({"id": "42", "timestamp": "2024-01-01T00:00:00+03:00"});
        assert!(track_of(&short).is_none(), "без названия трек не годится");
        assert_eq!(item_id(&short), Some(42));

        let nested = json!({"track": {"id": 7, "title": "Трек"}});
        assert_eq!(item_id(&nested), Some(7));
        assert_eq!(track_of(&nested).unwrap().id, 7);

        assert_eq!(item_id(&json!({"track": null})), None);
        assert_eq!(item_id(&json!({"id": 0})), None);
    }

    #[test]
    #[ignore = "calls the live public Yandex Music API"]
    fn public_playlists_list_and_tracks_decode() {
        let mut api = Api::new().unwrap();
        api.uid = Some(103_372_440);
        let playlists = api.playlists().unwrap();
        assert!(
            !playlists.is_empty(),
            "список плейлистов не должен быть пустым"
        );
        assert!(
            playlists.iter().all(|playlist| !playlist.title.is_empty()),
            "у каждого плейлиста должно быть название"
        );
        let playlist = playlists
            .iter()
            .find(|playlist| playlist.track_count > 0 && !playlist.id.is_empty())
            .expect("должен найтись непустой плейлист");
        let tracks = api.playlist_tracks(&playlist.id).unwrap();
        assert!(!tracks.is_empty(), "треки плейлиста должны читаться");
        assert!(tracks[0].id > 0 && !tracks[0].title.is_empty());
        let ids = tracks
            .iter()
            .take(3)
            .map(|track| track.id)
            .collect::<Vec<_>>();
        let hydrated = api.tracks_by_ids(&ids).unwrap();
        assert_eq!(
            hydrated.len(),
            ids.len(),
            "треки добираются по идентификаторам"
        );
        assert!(hydrated.iter().all(|track| !track.title.is_empty()));
    }

    #[test]
    #[ignore = "calls the live public Yandex Music API"]
    fn public_artist_info_and_tracks_decode() {
        let api = Api::new().unwrap();
        let info = api.artist(13_905).unwrap();
        assert_eq!(info.name, "Daft Punk");
        assert!(
            !info.description.is_empty(),
            "описание исполнителя приходит"
        );
        assert!(info.cover_uri.is_some(), "обложка исполнителя приходит");
        assert!(info.listeners > 0, "статистика приходит");
        assert!(!info.tracks.is_empty(), "треки исполнителя приходят");
        assert!(info.tracks[0].id > 0 && !info.tracks[0].title.is_empty());
    }

    #[test]
    #[ignore = "calls the live public Rotor metadata API"]
    fn wave_metadata_decodes_and_continues() {
        let api = Api::new().unwrap();
        let first = api.wave_start(None, vec![]).unwrap();
        let id = first.radio_session_id.clone().unwrap();
        let tracks = first.tracks();
        assert!(!tracks.is_empty());
        let queue = tracks.iter().map(|track| track.id.to_string()).collect();
        let next = api.wave_more(&id, queue, None).unwrap();
        assert!(
            next.tracks()
                .iter()
                .any(|next| tracks.iter().all(|old| old.id != next.id))
        );
    }
}

#[cfg(test)]
mod session_validation_tests {
    #[test]
    fn session_cannot_change_endpoint() {
        for value in ["", "../account", "id?x=1", "a/b", "a#b", "a%2fb"] {
            assert!(!super::valid_session(value));
        }
        assert!(super::valid_session("abc-123_DEF"));
    }
}

use crate::api::{Api, Track};
use crate::equalizer::{self, Equalized};
use crate::worker::{Loaded, Loader};
use anyhow::{Context, Result, bail, ensure};
use discord_rich_presence::{DiscordIpc, DiscordIpcClient, activity};
use rodio::{Decoder, OutputStream, OutputStreamBuilder, Sink, Source};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{HashMap, HashSet},
    io::BufReader,
    sync::{
        Arc, Mutex,
        mpsc::{self, Receiver, SyncSender},
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

const READ_PATIENCE: Duration = Duration::from_secs(20);

pub type Reply = std::result::Result<Value, String>;

pub struct Job {
    pub name: String,
    pub args: Value,
    pub reply: mpsc::Sender<Reply>,
}

pub fn submit(tx: &SyncSender<Job>, name: &str, args: Value) -> Result<Receiver<Reply>> {
    let (reply, rx) = mpsc::channel();
    tx.try_send(Job {
        name: name.into(),
        args,
        reply,
    })
    .map_err(|_| anyhow::anyhow!("Плеер занят или закрыт"))?;
    Ok(rx)
}

#[derive(Clone, Default, Serialize, Deserialize)]
pub struct Snapshot {
    pub track: Option<Track>,
    pub playing: bool,
    #[serde(default)]
    pub loading: bool,
    pub position: f64,
    pub duration: f64,
    pub volume: f32,
    pub codec: String,
    pub queue: Vec<Track>,
    pub index: Option<usize>,
    pub repeat: String,
    pub shuffle: bool,
    pub wave: bool,
    pub equalizer: equalizer::Settings,
    pub logged_in: bool,
    #[serde(skip_serializing, default)]
    pub account_name: String,
    pub rpc: bool,
    #[serde(default)]
    pub liked: HashSet<u64>,
    #[serde(default)]
    pub rpc_settings: crate::preferences::RpcSettings,
    pub lossless: bool,
    pub error: String,
}

pub fn start(
    wake_ui: impl Fn() + Send + 'static,
    window: Option<isize>,
) -> (SyncSender<Job>, Arc<Mutex<Snapshot>>) {
    let (tx, rx) = mpsc::sync_channel::<Job>(12);
    let state = Arc::new(Mutex::new(Snapshot::default()));
    let published = state.clone();
    thread::spawn(move || {
        let mut player = match Player::new() {
            Ok(player) => player,
            Err(error) => {
                published.lock().unwrap().error = error.to_string();
                wake_ui();
                return;
            }
        };
        let (media_tx, media_rx) = mpsc::sync_channel::<Job>(8);
        #[cfg(windows)]
        let mut media =
            window.and_then(|hwnd| crate::media_session::Session::new(hwnd, media_tx).ok());
        #[cfg(not(windows))]
        let _ = (window, media_tx);
        let mut publish = |player: &Player| {
            let snapshot = player.snapshot();
            #[cfg(windows)]
            if let Some(media) = &mut media {
                media.update(&snapshot);
            }
            *published.lock().unwrap() = snapshot;
        };
        publish(&player);
        wake_ui();
        loop {
            let wait = if player.state.loading {
                30
            } else if player.state.playing {
                120
            } else {
                750
            };
            match media_rx
                .try_recv()
                .map_or_else(|_| rx.recv_timeout(Duration::from_millis(wait)), Ok)
            {
                Ok(job) => {
                    if player.forward(&job) {
                        continue;
                    }
                    let result = player
                        .command(&job.name, &job.args)
                        .map_err(|e| e.to_string());
                    publish(&player);
                    let _ = job.reply.send(result);
                    wake_ui();
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    let changed = player.tick();
                    if changed || player.state.playing || player.state.loading {
                        publish(&player);
                        wake_ui();
                    }
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }
        player.clear_rpc();
    });
    (tx, state)
}

struct Player {
    api: Api,
    state: Snapshot,
    stream: Option<OutputStream>,
    sink: Option<Sink>,
    discord: Option<DiscordIpcClient>,
    last_rpc_attempt: Option<Instant>,
    wave: Option<Wave>,
    wave_pending: Option<(String, Receiver<Result<crate::api::WaveBatch>>)>,
    wave_retry: Option<Instant>,
    feedback: SyncSender<(Api, String, Value)>,
    eq_control: equalizer::Control,
    loader: Loader,
    reads: SyncSender<ReadTask>,
    generation: u64,
    autoplay: bool,
}

type ReadTask = (Api, String, Value, mpsc::Sender<Reply>);

struct Wave {
    session: String,
    batches: HashMap<u64, String>,
}

impl Player {
    fn new() -> Result<Self> {
        let api = Api::new()?;
        let (feedback, events) = mpsc::sync_channel::<(Api, String, Value)>(24);
        thread::spawn(move || {
            for (api, session, event) in events {
                api.wave_feedback(&session, &event);
            }
        });
        let (reads, tasks) = mpsc::sync_channel::<ReadTask>(16);
        thread::spawn(move || {
            for (api, name, args, reply) in tasks {
                let result = match name.as_str() {
                    "search" => api
                        .search(args["query"].as_str().unwrap_or_default())
                        .map(|tracks| json!(tracks)),
                    "library" => api.library().map(|tracks| json!(tracks)),
                    "playlists" => api.playlists().map(|playlists| json!(playlists)),
                    "playlist_create" => {
                        api.create_playlist(args["title"].as_str().unwrap_or_default())
                    }
                    "playlist_add" => api.add_to_playlist(
                        args["playlist_id"].as_str().unwrap_or_default(),
                        args["track_id"].as_u64().unwrap_or(0),
                    ),
                    "playlist_tracks" => api
                        .playlist_tracks(args["playlist_id"].as_str().unwrap_or_default())
                        .map(|tracks| json!(tracks)),
                    "artist" => api
                        .artist(args["artist_id"].as_u64().unwrap_or_default())
                        .map(|info| json!(info)),
                    _ => Err(anyhow::anyhow!("Неизвестный запрос: {name}")),
                }
                .map_err(|error| error.to_string());
                let _ = reply.send(result);
            }
        });
        Ok(Self {
            api,
            state: Snapshot {
                volume: 0.75,
                repeat: "off".into(),
                rpc: true,
                rpc_settings: crate::preferences::RpcSettings::load(),
                ..Snapshot::default()
            },
            stream: None,
            sink: None,
            discord: None,
            last_rpc_attempt: None,
            wave: None,
            wave_pending: None,
            wave_retry: None,
            feedback,
            eq_control: equalizer::Control::default(),
            loader: Loader::new(),
            reads,
            generation: 0,
            autoplay: true,
        })
    }

    fn snapshot(&self) -> Snapshot {
        let mut s = self.state.clone();
        if let Some(sink) = &self.sink {
            s.position = sink.get_pos().as_secs_f64();
            s.playing = !sink.is_paused() && !sink.empty();
        }
        s
    }

    fn play_audio(&mut self, track: &Track, index: Option<usize>) -> u64 {
        self.generation = self.loader.play(&self.api, track, self.state.lossless);
        self.autoplay = true;
        self.sink = None;
        self.state.track = Some(track.clone());
        self.state.index = index;
        self.state.duration = track.duration_ms as f64 / 1000.0;
        self.state.position = 0.0;
        self.state.playing = false;
        self.state.loading = true;
        self.state.codec.clear();
        self.state.error.clear();
        self.update_rpc();
        self.generation
    }

    fn attach(&mut self, loaded: Loaded) {
        match loaded {
            Loaded::Stream { audio, .. } => {
                let buffer = audio.buffer.clone();
                let codec = audio.codec;
                let mut reader = audio.reader;
                match Decoder::try_from(BufReader::new(reader.clone())) {
                    Ok(decoded) => {
                        buffer.set_patience(READ_PATIENCE);
                        self.start_source(decoded, codec);
                    }
                    Err(_) => {
                        buffer.wait_finished();
                        buffer.set_patience(READ_PATIENCE);
                        reader.rewind();
                        match Decoder::try_from(BufReader::new(reader)) {
                            Ok(decoded) => self.start_source(decoded, codec),
                            Err(error) => {
                                self.fail(format!("Не удалось декодировать аудио: {error}"))
                            }
                        }
                    }
                }
            }
            Loaded::Failed { error, .. } => self.fail(error),
        }
    }

    fn start_source<S>(&mut self, audio: S, codec: String)
    where
        S: Source<Item = f32> + Send + 'static,
    {
        if self.stream.is_none() {
            match OutputStreamBuilder::open_default_stream() {
                Ok(stream) => self.stream = Some(stream),
                Err(error) => {
                    return self.fail(format!("Устройство вывода недоступно: {error}"));
                }
            }
        }
        let sink = Sink::connect_new(self.stream.as_ref().unwrap().mixer());
        sink.pause();
        sink.set_volume(self.state.volume);
        sink.append(Equalized::new(audio, self.eq_control.clone()));
        if self.autoplay {
            sink.play();
        }
        self.sink = Some(sink);
        self.state.loading = false;
        self.state.playing = self.autoplay;
        self.state.codec = codec;
        if let Some(track) = self.state.track.as_ref().map(|track| track.id) {
            self.wave_event("trackStarted", track, None);
        }
        self.update_rpc();
        self.prefetch_next();
    }

    fn fail(&mut self, error: String) {
        self.state.loading = false;
        self.state.playing = false;
        self.state.error = error;
        self.update_rpc();
    }

    fn prefetch_next(&mut self) {
        let Some(index) = self.state.index else {
            return;
        };
        if let Some(next) = self.state.queue.get(index + 1).cloned() {
            self.loader.prefetch(&self.api, &next, self.state.lossless);
        }
    }

    fn active_sink(&self) -> Result<&Sink> {
        ensure!(!self.state.loading, "Трек ещё загружается");
        self.sink.as_ref().context("Сначала выберите трек")
    }

    fn forward(&self, job: &Job) -> bool {
        let (read, args) = match job.name.as_str() {
            "search" => match job.args["query"].as_str().map(str::trim) {
                Some(query) if !query.is_empty() && query.len() <= 500 => {
                    ("search", json!({ "query": query }))
                }
                _ => {
                    let _ = job.reply.send(Err("Введите запрос до 500 байт".into()));
                    return true;
                }
            },
            "library" => ("library", json!({})),
            "playlists" => ("playlists", json!({})),
            "playlist_add" => ("playlist_add", job.args.clone()),
            "playlist_create" => ("playlist_create", job.args.clone()),
            "playlist_tracks" | "play_playlist" => match job.args["playlist_id"].as_str() {
                Some(id) if !id.is_empty() => ("playlist_tracks", json!({ "playlist_id": id })),
                _ => {
                    let _ = job.reply.send(Err("Укажите playlist_id".into()));
                    return true;
                }
            },
            "artist" => match job.args["artist_id"].as_u64() {
                Some(id) if id > 0 => ("artist", json!({ "artist_id": id })),
                _ => {
                    let _ = job.reply.send(Err("Укажите artist_id".into()));
                    return true;
                }
            },
            _ => return false,
        };
        let task = (self.api.clone(), read.to_owned(), args, job.reply.clone());
        if self.reads.try_send(task).is_err() {
            let _ = job.reply.send(Err("Плеер занят, повторите запрос".into()));
        }
        true
    }

    fn select(&mut self, index: usize) -> Result<()> {
        self.select_after(index, "skip")
    }

    fn select_after(&mut self, index: usize, event: &str) -> Result<()> {
        let track = self
            .state
            .queue
            .get(index)
            .context("Нет трека на этой позиции")?
            .clone();
        let old = self.wave_position();
        self.play_audio(&track, Some(index));
        if let Some((id, seconds)) = old {
            self.wave_event(event, id, Some(seconds));
        }
        Ok(())
    }

    fn replace_queue(&mut self, tracks: Vec<Track>, index: usize) -> Result<()> {
        let track = tracks.get(index).context("Трек не найден")?.clone();
        let old = self.wave_position();
        self.state.queue = tracks;
        self.state.index = Some(index);
        self.play_audio(&track, Some(index));
        if let Some((id, seconds)) = old {
            self.wave_event("skip", id, Some(seconds));
        }
        self.wave = None;
        self.state.wave = false;
        Ok(())
    }

    fn wave_position(&self) -> Option<(u64, f64)> {
        self.wave.as_ref()?;
        let track = self.state.track.as_ref()?;
        let seconds = self.sink.as_ref()?.get_pos().as_secs_f64();
        Some((track.id, seconds.min(self.state.duration)))
    }

    fn wave_event(&self, kind: &str, track_id: u64, seconds: Option<f64>) {
        let Some(wave) = &self.wave else { return };
        let Some(batch_id) = wave.batches.get(&track_id) else {
            return;
        };
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        let mut event = json!({"type":kind,"timestamp":now});
        if kind != "radioStarted" {
            event["trackId"] = json!(track_id.to_string());
        }
        if let Some(seconds) = seconds {
            event["totalPlayedSeconds"] = json!(seconds);
        }
        let _ = self.feedback.try_send((
            self.api.clone(),
            wave.session.clone(),
            json!({
                "event":event,"batchId":batch_id,"from":"desktop"
            }),
        ));
    }

    fn start_wave(&mut self) -> Result<()> {
        ensure!(
            self.state.logged_in,
            "Войдите в Яндекс ID, чтобы запустить Мою волну"
        );
        let response = self.api.wave_start()?;
        let session = response
            .radio_session_id
            .clone()
            .context("Моя волна не выдала номер сессии")?;
        let batch_id = response.batch_id.clone();
        let mut unique = HashSet::new();
        let tracks = response
            .tracks()
            .into_iter()
            .filter(|track| unique.insert(track.id))
            .collect::<Vec<_>>();
        let first = tracks.first().context("Моя волна пока не нашла треки")?.id;
        let batches = tracks
            .iter()
            .map(|track| (track.id, batch_id.clone()))
            .collect();
        self.replace_queue(tracks, 0)?;
        self.wave_pending = None;
        self.wave_retry = None;
        self.wave = Some(Wave { session, batches });
        self.state.wave = true;
        self.wave_event("radioStarted", first, None);
        Ok(())
    }

    fn request_wave(&mut self) {
        if self.wave_pending.is_some()
            || self
                .wave_retry
                .is_some_and(|t| t.elapsed() < Duration::from_secs(15))
        {
            return;
        }
        let Some(wave) = &self.wave else {
            return;
        };
        let session = wave.session.clone();
        let queue = self
            .state
            .queue
            .iter()
            .rev()
            .take(60)
            .map(|t| t.id.to_string())
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        let api = self.api.clone();
        let (tx, rx) = mpsc::channel();
        let key = session.clone();
        thread::spawn(move || {
            let _ = tx.send(api.wave_more(&key, queue));
        });
        self.wave_pending = Some((session, rx));
        self.wave_retry = Some(Instant::now());
    }

    fn poll_wave(&mut self) {
        let Some((session, rx)) = &self.wave_pending else {
            return;
        };
        let Ok(result) = rx.try_recv() else {
            return;
        };
        let valid = self.wave.as_ref().is_some_and(|w| w.session == *session);
        self.wave_pending = None;
        if valid {
            match result.and_then(|batch| self.append_wave(batch)) {
                Ok(()) => self.prefetch_next(),
                Err(error) => self.state.error = format!("Не удалось обновить волну: {error}"),
            }
        }
    }

    fn extend_wave(&mut self) -> Result<()> {
        let wave = self.wave.as_mut().context("Моя волна выключена")?;
        let queue = self
            .state
            .queue
            .iter()
            .rev()
            .take(60)
            .map(|track| track.id.to_string())
            .collect::<Vec<_>>();
        let batch = self
            .api
            .wave_more(&wave.session, queue.into_iter().rev().collect())?;
        self.append_wave(batch)
    }

    fn append_wave(&mut self, batch: crate::api::WaveBatch) -> Result<()> {
        let wave = self.wave.as_mut().context("Моя волна выключена")?;
        let batch_id = batch.batch_id.clone();
        let replacement_session = batch
            .radio_session_id
            .clone()
            .filter(|id| id != &wave.session);
        let mut known = self
            .state
            .queue
            .iter()
            .map(|track| track.id)
            .collect::<HashSet<_>>();
        let tracks = batch
            .tracks()
            .into_iter()
            .filter(|track| known.insert(track.id))
            .collect::<Vec<_>>();
        ensure!(!tracks.is_empty(), "Моя волна пока не выдала новых треков");
        let first = tracks[0].id;
        let restarted = replacement_session.is_some();
        if let Some(session) = replacement_session {
            wave.session = session;
            wave.batches.clear();
        }
        for track in &tracks {
            wave.batches.insert(track.id, batch_id.clone());
        }
        self.state.queue.extend(tracks);
        if let Some(index) = self.state.index.as_mut() {
            let excess = index.saturating_sub(120);
            for track in self.state.queue.drain(..excess) {
                wave.batches.remove(&track.id);
            }
            *index -= excess;
        }
        if restarted {
            self.wave_event("radioStarted", first, None);
        }
        Ok(())
    }

    fn advance(&mut self, previous: bool, automatic: bool) -> Result<()> {
        let index = self.state.index.context("Очередь пуста")?;
        let len = self.state.queue.len();
        let event = if automatic { "trackFinished" } else { "skip" };
        if self.wave.is_some() && !previous && !(automatic && self.state.repeat == "track") {
            if index + 1 >= len {
                self.extend_wave()?;
            }
            return self.select_after(self.state.index.unwrap_or(index) + 1, event);
        }
        let next = if automatic && self.state.repeat == "track" {
            index
        } else if previous {
            if index == 0 && self.state.repeat == "queue" {
                len - 1
            } else {
                index.saturating_sub(1)
            }
        } else if self.state.shuffle && len > 1 {
            let mut random = [0u8; 8];
            getrandom::fill(&mut random)
                .map_err(|_| anyhow::anyhow!("Не удалось перемешать очередь"))?;
            (index + 1 + (u64::from_ne_bytes(random) % (len - 1) as u64) as usize) % len
        } else if index + 1 < len {
            index + 1
        } else if self.state.repeat == "queue" {
            0
        } else {
            bail!("Достигнут конец очереди");
        };
        self.select_after(next, event)
    }

    fn tick(&mut self) -> bool {
        self.poll_wave();
        if self.wave.is_some()
            && self
                .state
                .index
                .is_some_and(|i| self.state.queue.len().saturating_sub(i + 1) <= 3)
        {
            self.request_wave();
        }
        let mut changed = false;
        for loaded in self.loader.poll() {
            if loaded.generation() != self.generation {
                continue;
            }
            self.attach(loaded);
            changed = true;
        }
        if self.state.playing && self.sink.as_ref().is_some_and(Sink::empty) {
            self.state.playing = false;
            if let Err(error) = self.advance(false, true) {
                self.state.error = error.to_string();
                self.update_rpc();
            }
            changed = true;
        }
        if self.state.rpc && self.state.playing && self.discord.is_none() {
            self.update_rpc();
        }
        changed
    }

    fn clear_rpc(&mut self) {
        if let Some(mut rpc) = self.discord.take() {
            let _ = rpc.clear_activity();
            let _ = rpc.close();
        }
    }

    fn update_rpc(&mut self) {
        let state = self.snapshot();
        if !state.rpc {
            self.clear_rpc();
            return;
        }
        let Some(track) = state.track.filter(|_| state.playing) else {
            if let Some(rpc) = &mut self.discord
                && rpc.clear_activity().is_err()
            {
                self.discord = None;
            }
            return;
        };
        if self.discord.is_none() {
            if self
                .last_rpc_attempt
                .is_some_and(|t| t.elapsed() < Duration::from_secs(12))
            {
                return;
            }
            self.last_rpc_attempt = Some(Instant::now());
            if let Ok(mut rpc) = DiscordIpcClient::new("1124055337234858005")
                && rpc.connect().is_ok()
            {
                self.discord = Some(rpc);
            }
        }
        let artist = track.artist().chars().take(100).collect::<String>();
        let title = track.title.chars().take(100).collect::<String>();
        let link = track.url();
        let app_link = format!("yandexmusic://track/{}", track.id);
        let cover = track.cover().map(|uri| {
            format!(
                "https://{}",
                uri.trim_start_matches("https://").replace("%%", "400x400")
            )
        });
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;
        let mut activity = activity::Activity::new()
            .activity_type(activity::ActivityType::Listening)
            .details(&title)
            .state(if artist.is_empty() {
                "Яндекс Музыка"
            } else {
                &artist
            })
            .buttons(vec![
                activity::Button::new("Открыть в приложении", &app_link),
                activity::Button::new("Открыть на сайте", &link),
            ]);
        if let Some(cover) = &cover {
            activity = activity.assets(
                activity::Assets::new()
                    .large_image(cover)
                    .large_text(&title),
            );
        }
        if state.duration > state.position {
            activity = activity.timestamps(
                activity::Timestamps::new()
                    .start(now - state.position as i64)
                    .end(now + (state.duration - state.position) as i64),
            );
        }
        let mut payload = serde_json::to_value(activity).unwrap_or_default();
        payload["name"] = json!(
            state
                .rpc_settings
                .display_name(&artist, &title)
                .chars()
                .take(128)
                .collect::<String>()
        );
        payload["status_display_type"] = json!(0);
        if !state.rpc_settings.links {
            payload.as_object_mut().unwrap().remove("buttons");
        }
        if state.rpc_settings.language == "en" && state.rpc_settings.links {
            payload["buttons"] = json!([{"label":"Open in app","url":app_link},{"label":"Open on website","url":link}]);
        }
        if let Some(rpc) = &mut self.discord
            && rpc.send(json!({"cmd":"SET_ACTIVITY","args":{"pid":std::process::id(),"activity":payload},"nonce":format!("{}-{now}", track.id)}), 1).is_err()
        {
            self.discord = None;
        }
    }

    fn command(&mut self, name: &str, a: &Value) -> Result<Value> {
        let track_id = || {
            a["track_id"]
                .as_u64()
                .filter(|&id| id > 0)
                .context("Укажите track_id")
        };
        let number = |key: &str| {
            a[key]
                .as_f64()
                .filter(|n| n.is_finite())
                .context("Укажите число")
        };
        let boolean = || a["enabled"].as_bool().context("Укажите enabled");
        let query = || {
            a["query"]
                .as_str()
                .map(str::trim)
                .filter(|q| !q.is_empty() && q.len() <= 500)
                .context("Введите запрос до 500 байт")
        };
        match name {
            "state" => return Ok(serde_json::to_value(self.snapshot())?),
            "login" => {
                let token = a["token"]
                    .as_str()
                    .map(str::trim)
                    .filter(|t| !t.is_empty() && t.len() <= 8192)
                    .context("Введите OAuth-токен")?;
                self.state.account_name = self.api.login(token.to_owned())?;
                self.state.logged_in = true;
                self.state.liked = self.api.liked_ids().unwrap_or_default();
                let stored = if a["remember"].as_bool() == Some(true) {
                    crate::auth::save(token)
                } else if a["remember"].as_bool() == Some(false) {
                    crate::auth::clear()
                } else {
                    Ok(())
                };
                return Ok(json!({"storage_warning":stored.err().map(|e| e.to_string())}));
            }
            "logout" => {
                let storage_error = crate::auth::clear().err();
                self.generation = self.loader.cancel();
                self.autoplay = false;
                self.sink = None;
                self.stream = None;
                self.state.track = None;
                self.state.queue.clear();
                self.state.index = None;
                self.state.playing = false;
                self.state.loading = false;
                self.wave = None;
                self.state.wave = false;
                self.state.position = 0.0;
                self.state.duration = 0.0;
                self.state.codec.clear();
                self.state.error.clear();
                self.api.logout();
                self.state.logged_in = false;
                self.state.account_name.clear();
                self.state.liked.clear();
                self.update_rpc();
                if let Some(error) = storage_error {
                    self.state.error =
                        format!("Вы вышли. Не удалось удалить сохранённый токен: {error}");
                }
            }
            "play_query" => {
                let tracks = self.api.search(query()?)?;
                self.replace_queue(tracks, 0)?;
            }
            "wave_start" => self.start_wave()?,
            "play_track" => {
                let track = self.api.track(track_id()?)?;
                self.replace_queue(vec![track], 0)?;
            }
            "play_results" => {
                let tracks: Vec<Track> = serde_json::from_value(a["tracks"].clone())?;
                let index = a["index"].as_u64().context("Укажите позицию")? as usize;
                self.replace_queue(tracks, index)?;
            }
            "queue_add" => self.state.queue.push(self.api.track(track_id()?)?),
            "queue_play" => self.select(a["index"].as_u64().context("Укажите позицию")? as usize)?,
            "queue_remove" => {
                let index = a["index"].as_u64().context("Укажите позицию")? as usize;
                ensure!(index < self.state.queue.len(), "Нет трека на этой позиции");
                ensure!(
                    Some(index) != self.state.index,
                    "Сначала переключите текущий трек"
                );
                self.state.queue.remove(index);
                if let Some(current) = self
                    .state
                    .index
                    .as_mut()
                    .filter(|current| index < **current)
                {
                    *current -= 1;
                }
            }
            "play" | "pause" => {
                let play = name == "play";
                if self.state.loading {
                    self.autoplay = play;
                } else if play {
                    self.autoplay = true;
                    if self.sink.is_none() {
                        if self.state.queue.is_empty() {
                            bail!("Очередь пуста");
                        }
                        self.select(
                            self.state
                                .index
                                .unwrap_or(0)
                                .min(self.state.queue.len() - 1),
                        )?;
                    } else if self.sink.as_ref().is_some_and(Sink::empty) {
                        self.select(self.state.index.context("Очередь пуста")?)?;
                    } else {
                        self.sink.as_ref().unwrap().play();
                        self.state.playing = true;
                        self.update_rpc();
                    }
                } else {
                    self.autoplay = false;
                    self.active_sink()?.pause();
                    self.state.playing = false;
                    self.update_rpc();
                }
            }
            "next" => self.advance(false, false)?,
            "previous" => self.advance(true, false)?,
            "seek" => {
                let seconds = number("seconds")?;
                ensure!(
                    seconds >= 0.0 && seconds <= self.state.duration,
                    "Позиция за пределами трека"
                );
                self.active_sink()?
                    .try_seek(Duration::from_secs_f64(seconds))
                    .map_err(|error| anyhow::anyhow!("Не удалось перемотать: {error}"))?;
                self.update_rpc();
            }
            "volume" => {
                let volume = number("value")?;
                ensure!(
                    (0.0..=1.0).contains(&volume),
                    "Громкость должна быть от 0 до 1"
                );
                self.state.volume = volume as f32;
                if let Some(sink) = &self.sink {
                    sink.set_volume(self.state.volume);
                }
            }
            "like" | "unlike" => {
                let id = if a.get("track_id").is_some() {
                    track_id()?
                } else {
                    self.state
                        .track
                        .as_ref()
                        .context("Сначала выберите трек")?
                        .id
                };
                self.api.like(id, name == "like")?;
                if name == "like" {
                    self.state.liked.insert(id);
                } else {
                    self.state.liked.remove(&id);
                }
            }
            "repeat" => {
                let mode = a["mode"].as_str().context("Укажите режим повтора")?;
                ensure!(
                    matches!(mode, "off" | "track" | "queue"),
                    "Неизвестный режим повтора"
                );
                self.state.repeat = mode.to_owned();
            }
            "shuffle" => {
                ensure!(!self.state.wave, "Порядок треков задаёт Моя волна");
                self.state.shuffle = boolean()?;
            }
            "equalizer" => {
                ensure!(
                    a.get("enabled").is_some() || a.get("gains").is_some(),
                    "Укажите enabled или gains"
                );
                let mut settings = self.state.equalizer;
                if a.get("enabled").is_some() {
                    settings.enabled = boolean()?;
                }
                if let Some(gains) = a.get("gains") {
                    settings.gains = serde_json::from_value(gains.clone())
                        .context("Нужны пять целых значений в дБ")?;
                    ensure!(
                        settings.gains.iter().all(|gain| (-12..=12).contains(gain)),
                        "Диапазон эквалайзера от −12 до +12 дБ"
                    );
                }
                self.eq_control.set(settings);
                self.state.equalizer = settings;
            }
            "lossless" => self.state.lossless = boolean()?,
            "stop" => {
                self.loader.cancel();
                self.sink = None;
                self.stream = None;
                self.state.playing = false;
                self.state.loading = false;
                self.clear_rpc();
            }
            "rpc_settings" => {
                let settings: crate::preferences::RpcSettings = serde_json::from_value(a.clone())?;
                settings.validate()?;
                settings.save()?;
                self.state.rpc_settings = settings;
                self.update_rpc();
            }
            "discord_rpc" => {
                self.state.rpc = boolean()?;
                self.update_rpc();
            }
            _ => bail!("Неизвестная команда"),
        }
        Ok(json!({"ok": true}))
    }
}

#[cfg(test)]
mod snapshot_tests {
    #[test]
    fn public_snapshot_round_trip_does_not_expose_account() {
        let state = super::Snapshot {
            account_name: "private account".into(),
            ..Default::default()
        };
        let json = serde_json::to_value(&state).unwrap();
        assert!(json.get("account_name").is_none());
        let restored: super::Snapshot = serde_json::from_value(json).unwrap();
        assert!(restored.account_name.is_empty());
    }
}

#[cfg(test)]
mod like_state_tests {
    #[test]
    fn snapshot_contains_all_liked_tracks_not_only_the_current_one() {
        let mut snapshot = super::Snapshot::default();
        snapshot.liked.extend([123, 456]);
        let decoded: super::Snapshot =
            serde_json::from_value(serde_json::to_value(snapshot).unwrap()).unwrap();
        assert!(decoded.liked.contains(&123));
        assert!(decoded.liked.contains(&456));
        assert!(!decoded.liked.contains(&789));
    }
}

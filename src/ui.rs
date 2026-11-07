use crate::{
    api::{ArtistInfo, Playlist, Track},
    auth::{self, BrowserLogin},
    covers::Covers,
    equalizer::{FREQUENCIES, Settings as EqualizerSettings},
    mcp::Bridge,
    media::{self, MediaKeys},
    player::{self, Job, Reply, Snapshot},
};
use eframe::egui::{
    self, Align, Color32, Context, FontData, FontDefinitions, FontFamily, Frame, Key, Layout,
    Margin, RichText, ScrollArea, Slider, TextEdit, ViewportBuilder, ViewportCommand,
};
use serde_json::{Value, json};
use std::{
    sync::{
        Arc, Mutex,
        mpsc::{Receiver, SyncSender},
    },
    time::{Duration, Instant},
};

const YELLOW: Color32 = Color32::from_rgb(255, 220, 60);
const MIN_VOLUME_DB: f32 = -60.0;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Page {
    Home,
    Search,
    Library,
    Playlists,
    Artist,
    Queue,
    Settings,
}

struct App {
    tx: SyncSender<Job>,
    state: Arc<Mutex<Snapshot>>,
    page: Page,
    bridge: Option<Bridge>,
    token: String,
    browser_login: Option<BrowserLogin>,
    callback_url: String,
    remember: bool,
    account_status: String,
    query: String,
    submitted_query: String,
    search_after: Option<Instant>,
    search_error: String,
    focus_search: bool,
    covers: Covers,
    results: Vec<Track>,
    library: Vec<Track>,
    playlists: Vec<Playlist>,
    opened_playlist: Option<Playlist>,
    playlist_tracks: Vec<Track>,
    playlist_error: String,
    opened_artist: Option<(u64, String)>,
    artist_info: Option<ArtistInfo>,
    artist_error: String,
    previous_page: Page,
    pending: Vec<(String, Receiver<Reply>)>,
    status: String,
    mini: bool,
    seek: Option<f64>,
    volume_target: Option<f32>,
    volume_sent: Option<f32>,
    equalizer_open: bool,
    eq_draft: EqualizerSettings,
    eq_dragging: bool,
    media: Option<MediaKeys>,
    media_error: String,
    text_focus: bool,
}

pub fn run() -> eframe::Result {
    eframe::run_native(
        "Яндекс Музыка · Rust",
        eframe::NativeOptions {
            viewport: ViewportBuilder::default()
                .with_inner_size([1060.0, 710.0])
                .with_min_inner_size([410.0, 155.0]),
            ..Default::default()
        },
        Box::new(|cc| Ok(Box::new(App::new(&cc.egui_ctx)))),
    )
}

impl App {
    fn new(ctx: &Context) -> Self {
        let mut visuals = egui::Visuals::dark();
        visuals.panel_fill = Color32::from_rgb(17, 17, 20);
        visuals.extreme_bg_color = Color32::from_gray(13);
        visuals.widgets.inactive.bg_fill = Color32::from_gray(37);
        visuals.widgets.inactive.weak_bg_fill = Color32::from_rgb(36, 36, 43);
        visuals.widgets.hovered.weak_bg_fill = Color32::from_rgb(57, 55, 67);
        for widget in [
            &mut visuals.widgets.inactive,
            &mut visuals.widgets.hovered,
            &mut visuals.widgets.active,
        ] {
            widget.corner_radius = 10.into();
            widget.bg_stroke = egui::Stroke::NONE;
        }
        visuals.selection.bg_fill = Color32::from_rgb(105, 86, 16);
        ctx.set_visuals(visuals);
        ctx.style_mut(|style| {
            style.spacing.item_spacing = egui::vec2(10.0, 9.0);
            style.spacing.button_padding = egui::vec2(12.0, 6.0);
        });
        let mut fonts = FontDefinitions::default();
        if let Some(windows) = std::env::var_os("WINDIR") {
            if let Ok(bytes) =
                std::fs::read(std::path::PathBuf::from(windows).join("Fonts/segoeui.ttf"))
            {
                fonts
                    .font_data
                    .insert("segoe".into(), Arc::new(FontData::from_owned(bytes)));
                fonts
                    .families
                    .entry(FontFamily::Proportional)
                    .or_default()
                    .insert(0, "segoe".into());
            }
        }
        ctx.set_fonts(fonts);
        let repaint = ctx.clone();
        let (tx, state) = player::start(move || repaint.request_repaint());
        let mut app = Self {
            tx,
            state,
            page: Page::Home,
            bridge: None,
            token: String::new(),
            browser_login: None,
            callback_url: String::new(),
            remember: true,
            account_status: String::new(),
            query: String::new(),
            submitted_query: String::new(),
            search_after: None,
            search_error: String::new(),
            focus_search: false,
            covers: Covers::new(ctx),
            results: vec![],
            library: vec![],
            playlists: vec![],
            opened_playlist: None,
            playlist_tracks: vec![],
            playlist_error: String::new(),
            opened_artist: None,
            artist_info: None,
            artist_error: String::new(),
            previous_page: Page::Search,
            pending: vec![],
            status: String::new(),
            mini: false,
            seek: None,
            volume_target: None,
            volume_sent: None,
            equalizer_open: false,
            eq_draft: EqualizerSettings::default(),
            eq_dragging: false,
            media: None,
            media_error: String::new(),
            text_focus: false,
        };
        match media::start(app.tx.clone(), app.state.clone()) {
            Some(keys) => app.media = Some(keys),
            None => {
                app.media_error =
                    "Не удалось перехватить мультимедийные клавиши".into();
            }
        }
        if let Ok(token) = std::env::var("YANDEX_MUSIC_TOKEN") {
            app.send("login", json!({"token":token}));
        } else {
            match auth::read() {
                Ok(Some(token)) => app.send("login", json!({"token":token})),
                Ok(None) => {}
                Err(_) => {
                    app.account_status = "Не удалось прочитать сохранённую сессию Windows".into()
                }
            }
        }
        app
    }

    fn send(&mut self, name: &str, args: Value) {
        match player::submit(&self.tx, name, args) {
            Ok(rx) => self.pending.push((name.to_owned(), rx)),
            Err(err) => self.status = err.to_string(),
        }
    }

    fn busy(&self) -> bool {
        self.pending.iter().any(|(name, _)| {
            matches!(
                name.as_str(),
                "search"
                    | "library"
                    | "playlists"
                    | "playlist_tracks"
                    | "play_playlist"
                    | "artist"
                    | "play_query"
                    | "play_track"
                    | "play_results"
                    | "wave_start"
                    | "queue_add"
                    | "login"
                    | "logout"
            )
        })
    }

    fn flush_volume(&mut self, s: &Snapshot) {
        let Some(target) = self.volume_target else {
            return;
        };
        if (s.volume - target).abs() <= 0.002 {
            self.volume_target = None;
            self.volume_sent = None;
            return;
        }
        if self
            .volume_sent
            .is_some_and(|sent| (sent - target).abs() < f32::EPSILON)
        {
            return;
        }
        if let Ok(reply) = player::submit(&self.tx, "volume", json!({"value":target})) {
            drop(reply);
            self.volume_sent = Some(target);
        }
    }

    fn poll(&mut self) {
        let mut pending = std::mem::take(&mut self.pending);
        for (name, rx) in pending.drain(..) {
            match rx.try_recv() {
                Ok(Ok(v)) => match name.as_str() {
                    "search" if self.query.trim() == self.submitted_query => match tracks_from(v) {
                        Ok(tracks) => self.results = tracks,
                        Err(error) => self.search_error = error,
                    },
                    "search" => {}
                    "library" => match tracks_from(v) {
                        Ok(tracks) => {
                            self.status = if tracks.is_empty() {
                                "Треки не найдены".into()
                            } else {
                                String::new()
                            };
                            self.library = tracks;
                        }
                        Err(error) => self.status = error,
                    },
                    "playlists" => match playlists_from(v) {
                        Ok(playlists) => {
                            self.status = if playlists.is_empty() {
                                "Плейлисты не найдены".into()
                            } else {
                                String::new()
                            };
                            self.playlists = playlists;
                        }
                        Err(error) => self.status = error,
                    },
                    "playlist_tracks" => match tracks_from(v) {
                        Ok(tracks) => {
                            self.playlist_error = if tracks.is_empty() {
                                "В плейлисте нет доступных треков".into()
                            } else {
                                String::new()
                            };
                            self.playlist_tracks = tracks;
                        }
                        Err(error) => {
                            self.playlist_error = error;
                            self.playlist_tracks.clear();
                        }
                    },
                    "play_playlist" => match tracks_from(v) {
                        Ok(tracks) if !tracks.is_empty() => {
                            self.status.clear();
                            self.send("play_results", json!({ "tracks": tracks, "index": 0 }));
                        }
                        Ok(_) => self.status = "В плейлисте нет доступных треков".into(),
                        Err(error) => self.status = error,
                    },
                    "artist" => match serde_json::from_value::<ArtistInfo>(v) {
                        Ok(info) => {
                            self.artist_error.clear();
                            self.artist_info = Some(info);
                        }
                        Err(_) => {
                            self.artist_error = "Не удалось прочитать данные исполнителя".into()
                        }
                    },
                    "login" => {
                        self.token.clear();
                        self.callback_url.clear();
                        self.browser_login = None;
                        self.account_status = if v["storage_warning"].is_string() {
                            "Вход выполнен. Windows не удалось обновить сохранённую сессию.".into()
                        } else {
                            "Вход выполнен".into()
                        };
                        if self.page == Page::Library {
                            self.send("library", json!({}));
                        }
                    }
                    "logout" => {
                        self.library.clear();
                        self.playlists.clear();
                        self.opened_playlist = None;
                        self.playlist_tracks.clear();
                        self.playlist_error.clear();
                        self.account_status = "Вы вышли из аккаунта".into();
                    }
                    "like" => self.status = "Добавлено в любимое".into(),
                    "unlike" => self.status = "Убрано из любимого".into(),
                    _ => self.status.clear(),
                },
                Ok(Err(e)) if matches!(name.as_str(), "login" | "logout") => {
                    self.account_status = e;
                }
                Ok(Err(e)) if name == "search" => {
                    if self.query.trim() == self.submitted_query {
                        self.search_error = e;
                    }
                }
                Ok(Err(e)) => self.status = e,
                Err(std::sync::mpsc::TryRecvError::Empty) => self.pending.push((name, rx)),
                Err(_) => self.status = "Плеер завершил работу".into(),
            }
        }
    }

    fn navigate(&mut self, page: Page, signed_in: bool) {
        if page == Page::Playlists {
            self.opened_playlist = None;
            self.playlist_tracks.clear();
            self.playlist_error.clear();
        }
        if self.page != page {
            self.page = page;
            if page == Page::Search {
                self.focus_search = true;
            }
            if page == Page::Library && signed_in {
                self.send("library", json!({}));
            }
            if page == Page::Playlists && signed_in {
                self.send("playlists", json!({}));
            }
        }
    }

    fn open_playlist(&mut self, playlist: &Playlist) {
        self.opened_playlist = Some(playlist.clone());
        self.playlist_tracks.clear();
        self.playlist_error.clear();
        self.send("playlist_tracks", json!({ "playlist_id": playlist.id }));
    }

    fn close_playlist(&mut self) {
        self.opened_playlist = None;
        self.playlist_tracks.clear();
        self.playlist_error.clear();
    }

    fn open_artist(&mut self, id: u64, name: &str) {
        if id == 0 {
            return;
        }
        if self.page != Page::Artist {
            self.previous_page = self.page;
        }
        self.page = Page::Artist;
        self.opened_artist = Some((id, name.to_owned()));
        self.artist_info = None;
        self.artist_error.clear();
        self.send("artist", json!({ "artist_id": id }));
    }

    fn shortcuts(&mut self, ctx: &Context, s: &Snapshot) {
        if self.text_focus {
            return;
        }
        let (ctrl, shift, pressed) = ctx.input(|input| {
            let pressed = [
                Key::ArrowRight,
                Key::ArrowLeft,
                Key::ArrowUp,
                Key::ArrowDown,
                Key::Space,
            ]
            .into_iter()
            .find(|&key| input.key_pressed(key));
            (input.modifiers.ctrl, input.modifiers.shift, pressed)
        });
        if let Some((name, args)) = shortcut_command(ctrl, shift, pressed, s) {
            self.send(name, args);
        }
    }

    fn nav(&mut self, ctx: &Context, s: &Snapshot) {
        egui::SidePanel::left("nav")
            .exact_width(190.0)
            .frame(
                Frame::new()
                    .fill(Color32::from_rgb(23, 23, 27))
                    .inner_margin(Margin::same(14)),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("♫").color(YELLOW).size(27.0).strong());
                    ui.label(RichText::new("Яндекс Музыка").size(17.0).strong());
                });
                ui.add_space(19.0);
                ui.label(
                    RichText::new("  ОБЗОР")
                        .size(10.5)
                        .color(Color32::from_gray(148))
                        .strong(),
                );
                for (page, title) in [(Page::Home, "Главная"), (Page::Search, "Поиск")]
                {
                    if nav_item(ui, title, Some(page), self.page == page) {
                        self.navigate(page, s.logged_in);
                    }
                }
                ui.add_space(13.0);
                ui.label(
                    RichText::new("  МОЯ МУЗЫКА")
                        .size(10.5)
                        .color(Color32::from_gray(148))
                        .strong(),
                );
                for (page, title) in [(Page::Library, "Любимое"), (Page::Playlists, "Плейлисты"), (Page::Queue, "Очередь")]
                {
                    if nav_item(ui, title, Some(page), self.page == page) {
                        self.navigate(page, s.logged_in);
                    }
                }
                ui.with_layout(Layout::bottom_up(Align::Min), |ui| {
                    let account = if s.logged_in {
                        s.account_name.as_str()
                    } else {
                        "Войти в Яндекс ID"
                    };
                    if nav_item(ui, account, None, false) {
                        self.navigate(Page::Settings, s.logged_in);
                    }
                    ui.separator();
                    if nav_item(
                        ui,
                        "Настройки",
                        Some(Page::Settings),
                        self.page == Page::Settings,
                    ) {
                        self.navigate(Page::Settings, s.logged_in);
                    }
                });
            });
    }

    fn start_browser_login(&mut self, ctx: &Context) {
        match BrowserLogin::new() {
            Ok(login) => {
                ctx.open_url(egui::OpenUrl::new_tab(login.url()));
                self.browser_login = Some(login);
                self.callback_url.clear();
                self.account_status.clear();
            }
            Err(error) => self.account_status = error.to_string(),
        }
    }

    fn account(&mut self, ui: &mut egui::Ui, s: &Snapshot) {
        ui.label(RichText::new("Аккаунт").size(18.0).strong());
        let busy = self
            .pending
            .iter()
            .any(|(name, _)| matches!(name.as_str(), "login" | "logout"));
        if s.logged_in {
            ui.label(RichText::new(&s.account_name).strong());
            if ui.add_enabled(!busy, egui::Button::new("Выйти")).clicked() {
                self.send("logout", json!({}));
            }
        } else {
            ui.label("Войдите, чтобы слушать треки и синхронизировать любимое.");
            ui.add_enabled_ui(!busy, |ui| {
                let browser_opened = ui.add(primary(if self.browser_login.is_some() { "Открыть Яндекс ID заново" } else { "Войти через Яндекс ID" })).clicked();
                if browser_opened {
                    self.start_browser_login(ui.ctx());
                }
                ui.checkbox(&mut self.remember, "Запомнить меня на этом ПК");
                if let Some(login) = &self.browser_login {
                    ui.small("Подтвердите вход в браузере. На открывшейся странице нажмите Ctrl+L и Ctrl+C, затем вставьте адрес сюда.");
                    let field = ui.add(TextEdit::singleline(&mut self.callback_url)
                        .password(true)
                        .desired_width(f32::INFINITY)
                        .hint_text("Вставьте адрес после входа (Ctrl+V)"));
                    if browser_opened {
                        field.request_focus();
                    }
                    if field.has_focus() {
                        self.text_focus = true;
                    }
                    let pasted = field.changed() && ui.input(|input| input.events.iter().any(|event| matches!(event, egui::Event::Paste(_))));
                    let finish = ui.button("Завершить вход").clicked();
                    if pasted || finish
                        || (field.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter))) {
                        match login.token(&self.callback_url) {
                            Ok(token) => {
                                self.callback_url.clear();
                                self.account_status.clear();
                                self.send("login", json!({"token":token, "remember":self.remember}));
                            }
                            Err(error) => self.account_status = error.to_string(),
                        }
                    }
                }
                ui.collapsing("Другой способ входа", |ui| {
                    ui.small("Если у вас уже есть OAuth-токен, его можно ввести вручную.");
                    let token_field =
                        ui.add(TextEdit::singleline(&mut self.token).password(true).hint_text("OAuth-токен"));
                    if token_field.has_focus() {
                        self.text_focus = true;
                    }
                    if ui.add_enabled(!self.token.trim().is_empty(), egui::Button::new("Войти с токеном")).clicked() {
                        let token = std::mem::take(&mut self.token);
                        self.send("login", json!({"token":token, "remember":self.remember}));
                    }
                });
            });
        }
        if busy {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.small("Проверяем сессию…");
            });
        }
        if !self.account_status.is_empty() {
            ui.label(RichText::new(&self.account_status).color(Color32::from_gray(183)));
        }
    }

    fn settings(&mut self, ui: &mut egui::Ui, s: &Snapshot) {
        ui.label(RichText::new("Настройки").size(30.0).strong());
        ui.label(
            RichText::new("Аккаунт, звук и подключённые приложения").color(Color32::from_gray(165)),
        );
        ui.add_space(10.0);
        ScrollArea::vertical().id_salt("settings").show(ui, |ui| {
            card().show(ui, |ui| {
                ui.set_width(ui.available_width());
                self.account(ui, s);
            });
            card().show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.label(RichText::new("Воспроизведение").size(18.0).strong());
                ui.small(
                    "Пробел — плей/пауза, Ctrl+←/→ — треки, Ctrl+Shift+←/→ — ±10 с, \
                     Ctrl+↑/↓ — громкость.",
                );
                if ui.button("Открыть эквалайзер").clicked() {
                    self.equalizer_open = true;
                }
                let mut lossless = s.lossless;
                if ui
                    .checkbox(&mut lossless, "Запрашивать lossless при загрузке трека")
                    .changed()
                {
                    self.send("lossless", json!({"enabled":lossless}));
                }
                ui.separator();
                let mut media = self.media.is_some();
                if ui
                    .checkbox(&mut media, "Мультимедийные клавиши клавиатуры")
                    .on_hover_text("Плей/пауза, следующий и предыдущий трек работают при свёрнутом окне")
                    .changed()
                {
                    if media {
                        match media::start(self.tx.clone(), self.state.clone()) {
                            Some(keys) => self.media = Some(keys),
                            None => {
                                self.media_error =
                                    "Не удалось перехватить клавиши".into();
                            }
                        }
                    } else {
                        self.media = None;
                        self.media_error.clear();
                    }
                }
                if !self.media_error.is_empty() {
                    ui.small(RichText::new(&self.media_error).color(Color32::from_gray(165)));
                }
            });
            card().show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.label(RichText::new("Интеграции").size(18.0).strong());
                let mut rpc = s.rpc;
                if ui.checkbox(&mut rpc, "Discord Rich Presence").changed() {
                    self.send("discord_rpc", json!({"enabled":rpc}));
                }
                ui.separator();
                let mut enabled = self.bridge.is_some();
                if ui.checkbox(&mut enabled, "MCP для ИИ-агентов").changed() {
                    if enabled {
                        match Bridge::start(self.tx.clone()) {
                            Ok(bridge) => self.bridge = Some(bridge),
                            Err(error) => self.status = error.to_string(),
                        }
                    } else {
                        self.bridge = None;
                    }
                }
                ui.small("Даёт локальным агентам доступ к плееру, очереди и любимым трекам.");
                if let Some(bridge) = &self.bridge {
                    if ui.button("Скопировать конфигурацию MCP").clicked() {
                        match bridge.config() {
                            Ok(config) => {
                                ui.ctx().copy_text(
                                    serde_json::to_string_pretty(&config).unwrap_or_default(),
                                );
                                self.status = "Конфигурация скопирована".into();
                            }
                            Err(error) => self.status = error.to_string(),
                        }
                    }
                    ui.small("После перезапуска клиента скопируйте новую конфигурацию.");
                }
            });
        });
    }

    fn playlist_page(&mut self, ui: &mut egui::Ui, s: &Snapshot, playlist: &Playlist) {
        if ui
            .add(egui::Button::new("← Плейлисты").frame(false))
            .clicked()
        {
            self.close_playlist();
            return;
        }
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            self.covers.draw(ui, playlist.cover_uri.as_deref(), 92.0);
            ui.add_space(8.0);
            ui.vertical(|ui| {
                ui.label(RichText::new(&playlist.title).size(26.0).strong());
                ui.label(
                    RichText::new(format!(
                        "{} • {} треков",
                        playlist.owner, playlist.track_count
                    ))
                    .color(Color32::from_gray(165)),
                );
                ui.add_space(8.0);
                if ui.add(primary("Играть")).clicked() {
                    self.send("play_playlist", json!({"playlist_id": playlist.id}));
                }
            });
        });
        ui.add_space(10.0);
        let loading = self
            .pending
            .iter()
            .any(|(name, _)| matches!(name.as_str(), "playlist_tracks" | "play_playlist"));
        if !self.playlist_error.is_empty() {
            ui.label(RichText::new(&self.playlist_error).color(Color32::from_rgb(255, 164, 164)));
            if ui.button("Повторить").clicked() {
                self.open_playlist(playlist);
            }
            return;
        }
        if self.playlist_tracks.is_empty() {
            if loading {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label("Загружаем треки…");
                });
            } else {
                ui.label("Список треков пока не загружен.");
                if ui.button("Загрузить").clicked() {
                    self.open_playlist(playlist);
                }
            }
            return;
        }
        let tracks = self.playlist_tracks.clone();
        let current = s
            .track
            .as_ref()
            .and_then(|track| tracks.iter().position(|item| item.id == track.id));
        self.rows(ui, &tracks, current, false);
    }

    fn artist_page(&mut self, ui: &mut egui::Ui, s: &Snapshot) {
        let Some((id, opened)) = self.opened_artist.clone() else {
            self.navigate(self.previous_page, s.logged_in);
            return;
        };
        if ui.add(egui::Button::new("← Назад").frame(false)).clicked() {
            let page = self.previous_page;
            self.opened_artist = None;
            self.artist_info = None;
            self.artist_error.clear();
            self.navigate(page, s.logged_in);
            return;
        }
        let info = self.artist_info.clone();
        let name = info
            .as_ref()
            .map_or_else(|| opened.clone(), |info| info.name.clone());
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            self.covers.draw(
                ui,
                info.as_ref().and_then(|info| info.cover_uri.as_deref()),
                92.0,
            );
            ui.add_space(8.0);
            ui.vertical(|ui| {
                ui.label(RichText::new(&name).size(26.0).strong());
                let Some(info) = &info else {
                    return;
                };
                let mut meta = Vec::new();
                if info.listeners > 0 {
                    meta.push(format!("{} слушателей за месяц", info.listeners));
                }
                if info.likes > 0 {
                    meta.push(format!("{} отметок «нравится»", info.likes));
                }
                if !meta.is_empty() {
                    ui.label(RichText::new(meta.join(" • ")).color(Color32::from_gray(165)));
                }
            });
        });
        ui.add_space(8.0);
        let loading = self.pending.iter().any(|(name, _)| name == "artist");
        if !self.artist_error.is_empty() {
            ui.label(RichText::new(&self.artist_error).color(Color32::from_rgb(255, 164, 164)));
            if ui.button("Повторить").clicked() {
                self.open_artist(id, &name);
            }
            return;
        }
        let Some(info) = info else {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label("Загружаем исполнителя…");
            });
            return;
        };
        if !info.description.is_empty() {
            egui::CollapsingHeader::new("Об исполнителе")
                .default_open(true)
                .show(ui, |ui| {
                    ScrollArea::vertical()
                        .id_salt("artist-about")
                        .max_height(150.0)
                        .show(ui, |ui| {
                            ui.label(
                                RichText::new(&info.description)
                                    .size(13.0)
                                    .color(Color32::from_gray(175)),
                            );
                        });
                });
            ui.add_space(6.0);
        }
        if info.tracks.is_empty() {
            if loading {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label("Загружаем треки…");
                });
            } else {
                ui.label("Треки не найдены.");
            }
            return;
        }
        let tracks = info.tracks.clone();
        let current = s
            .track
            .as_ref()
            .and_then(|track| tracks.iter().position(|item| item.id == track.id));
        ui.label(RichText::new("Популярные треки").size(18.0).strong());
        self.rows(ui, &tracks, current, false);
    }

    fn rows(&mut self, ui: &mut egui::Ui, tracks: &[Track], current: Option<usize>, queue: bool) {
        ScrollArea::vertical()
            .id_salt(if queue { "queue" } else { "tracks" })
            .auto_shrink([false, false])
            .show_rows(ui, 54.0, tracks.len(), |ui, range| {
                for index in range {
                    let track = &tracks[index];
                    let selected = current == Some(index);
                    ui.push_id(index, |ui| {
                        let width = ui.available_width();
                        let (rect, _) =
                            ui.allocate_exact_size(egui::vec2(width, 54.0), egui::Sense::hover());
                        if selected || ui.rect_contains_pointer(rect) {
                            ui.painter()
                                .rect_filled(rect, 10, Color32::from_rgb(35, 34, 43));
                        }
                        ui.scope_builder(
                            egui::UiBuilder::new()
                                .max_rect(rect.shrink2(egui::vec2(8.0, 4.0)))
                                .layout(Layout::left_to_right(Align::Center)),
                            |ui| {
                                self.covers.draw(ui, track.cover(), 46.0);
                                if ui
                                    .add(icon_button("▶", selected))
                                    .on_hover_text("Воспроизвести")
                                    .clicked()
                                {
                                    if queue {
                                        self.send("queue_play", json!({"index":index}));
                                    } else {
                                        self.send(
                                            "play_results",
                                            json!({"tracks":tracks,"index":index}),
                                        );
                                    }
                                }
                                let trailing_width = if width > 350.0 { 95.0 } else { 40.0 };
                                let title_width = (ui.available_width() - trailing_width).max(25.0);
                                ui.allocate_ui_with_layout(
                                    egui::vec2(title_width, 45.0),
                                    Layout::top_down(Align::Min).with_main_align(Align::Center),
                                    |ui| {
                                        ui.set_width(title_width);
                                        ui.spacing_mut().item_spacing.y = 2.0;
                                        let color = if selected { YELLOW } else { Color32::WHITE };
                                        ui.add(
                                            egui::Label::new(
                                                RichText::new(&track.title).strong().color(color),
                                            )
                                            .truncate(),
                                        );
                                        ui.horizontal(|ui| {
                                            ui.spacing_mut().item_spacing.x = 4.0;
                                            for (index, artist) in track.artists.iter().enumerate() {
                                                if index > 0 {
                                                    ui.label(
                                                        RichText::new("·")
                                                            .size(12.0)
                                                            .color(Color32::from_gray(120)),
                                                    );
                                                }
                                                let label = RichText::new(&artist.name)
                                                    .size(12.0)
                                                    .color(Color32::from_gray(163));
                                                if artist.id > 0 {
                                                    if ui
                                                        .add(egui::Button::new(label).frame(false))
                                                        .on_hover_text("Об исполнителе")
                                                        .clicked()
                                                    {
                                                        self.open_artist(artist.id, &artist.name);
                                                    }
                                                } else {
                                                    ui.add(egui::Label::new(label).truncate());
                                                }
                                            }
                                        });
                                    },
                                );
                                if width > 350.0 {
                                    ui.add_sized(
                                        [43.0, 24.0],
                                        egui::Label::new(
                                            RichText::new(time(track.duration_ms as f64 / 1000.0))
                                                .size(12.0)
                                                .color(Color32::from_gray(162)),
                                        ),
                                    );
                                }
                                let (symbol, hint) = if queue {
                                    ("×", "Убрать из очереди")
                                } else {
                                    ("+", "Добавить в очередь")
                                };
                                if ui
                                    .add_enabled(!queue || !selected, icon_button(symbol, false))
                                    .on_hover_text(hint)
                                    .clicked()
                                {
                                    if queue {
                                        self.send("queue_remove", json!({"index":index}));
                                    } else {
                                        self.send("queue_add", json!({"track_id":track.id}));
                                    }
                                }
                            },
                        );
                    });
                }
            });
    }

    fn search_page(&mut self, ui: &mut egui::Ui, state: &Snapshot) {
        ui.label(RichText::new("Поиск").size(32.0).strong());
        ui.add_space(5.0);
        let mut submit = false;
        card().inner_margin(Margin::same(9)).show(ui, |ui| {
            ui.horizontal(|ui| {
                let field = ui.add(
                    TextEdit::singleline(&mut self.query)
                        .desired_width((ui.available_width() - 142.0).max(65.0))
                        .hint_text("Название трека или имя исполнителя")
                        .char_limit(160)
                        .margin(egui::vec2(12.0, 10.0)),
                );
                if self.focus_search {
                    field.request_focus();
                    self.focus_search = false;
                }
                if field.has_focus() {
                    self.text_focus = true;
                }
                if field.changed() {
                    self.search_after = Some(Instant::now() + Duration::from_millis(320));
                    self.results.clear();
                    self.search_error.clear();
                }
                if ui
                    .add_enabled(!self.query.is_empty(), icon_button("×", false))
                    .on_hover_text("Очистить")
                    .clicked()
                {
                    self.query.clear();
                    self.submitted_query.clear();
                    self.search_after = None;
                    self.results.clear();
                    self.search_error.clear();
                    field.request_focus();
                }
                submit = ui.add(primary("Найти")).clicked()
                    || (field.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)));
            });
        });
        let pending = self.pending.iter().any(|(name, _)| name == "search");
        let query = self.query.trim();
        if query.is_empty() {
            ui.add_space(20.0);
            ui.label(RichText::new("Что хотите послушать?").size(21.0).strong());
            ui.label(
                RichText::new("Начните вводить название. Результаты появятся здесь.")
                    .color(Color32::from_gray(164)),
            );
            return;
        }
        if !pending {
            let due = self.search_after.is_some_and(|when| when <= Instant::now());
            if submit || due {
                match player::submit(&self.tx, "search", json!({"query":query})) {
                    Ok(reply) => {
                        self.submitted_query = query.to_owned();
                        self.pending.push(("search".into(), reply));
                        self.search_error.clear();
                    }
                    Err(error) => self.search_error = error.to_string(),
                }
                self.search_after = None;
            } else if let Some(when) = self.search_after {
                ui.ctx()
                    .request_repaint_after(when.saturating_duration_since(Instant::now()));
            }
        } else if submit {
            self.search_after = Some(Instant::now());
        }
        ui.add_space(6.0);
        if self.pending.iter().any(|(name, _)| name == "search") || self.search_after.is_some() {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label("Ищем треки…");
            });
        } else if !self.search_error.is_empty() {
            ui.label(RichText::new(&self.search_error).color(Color32::from_rgb(255, 164, 164)));
        } else if self.results.is_empty() {
            ui.label("Треки не найдены. Попробуйте другой запрос.");
        } else {
            ui.label(RichText::new("Треки").size(20.0).strong());
            let results = self.results.clone();
            let current = state
                .track
                .as_ref()
                .and_then(|current| results.iter().position(|track| track.id == current.id));
            self.rows(ui, &results, current, false);
        }
    }

    fn home(&mut self, ui: &mut egui::Ui, s: &Snapshot) {
        ui.label(RichText::new("Главная").size(32.0).strong());
        ui.add_space(8.0);
        let pending = self.pending.iter().any(|(name, _)| name == "wave_start");
        let (area, _) = ui.allocate_exact_size(
            egui::vec2(ui.available_width(), 292.0),
            egui::Sense::hover(),
        );
        draw_wave(ui, area, s.wave && s.playing);
        if s.wave && s.playing {
            ui.ctx().request_repaint_after(Duration::from_millis(33));
        }
        let center = area.center();
        let button_rect =
            egui::Rect::from_center_size(center - egui::vec2(0.0, 12.0), egui::vec2(197.0, 57.0));
        let label = if pending {
            "Запускаем…"
        } else if s.wave && s.playing {
            "Ⅱ   Моя волна"
        } else {
            "▶   Моя волна"
        };
        let button = egui::Button::new(
            RichText::new(label)
                .size(22.0)
                .strong()
                .color(Color32::WHITE),
        )
        .fill(Color32::from_rgb(42, 31, 68))
        .stroke(egui::Stroke::new(1.0_f32, Color32::from_rgb(128, 108, 177)))
        .corner_radius(29)
        .sense(if pending {
            egui::Sense::hover()
        } else {
            egui::Sense::click()
        });
        if ui.put(button_rect, button).clicked() && !pending {
            if !s.logged_in {
                self.navigate(Page::Settings, false);
                self.account_status = "Войдите, чтобы запустить Мою волну".into();
            } else {
                let command = if !s.wave {
                    "wave_start"
                } else if s.playing {
                    "pause"
                } else if !s.error.is_empty() {
                    "wave_start"
                } else {
                    "play"
                };
                self.send(command, json!({}));
            }
        }
        ui.painter().text(
            center + egui::vec2(0.0, 39.0),
            egui::Align2::CENTER_CENTER,
            if s.wave {
                "Ваш персональный поток"
            } else {
                "Музыка под ваше настроение"
            },
            egui::FontId::proportional(14.0),
            Color32::from_rgb(222, 213, 238),
        );
    }

    fn transport(&mut self, ui: &mut egui::Ui, s: &Snapshot) {
        ui.horizontal(|ui| {
            let position = self.seek.unwrap_or(s.position).clamp(0.0, s.duration);
            time_label(ui, position);
            ui.spacing_mut().slider_width = (ui.available_width() - 50.0).max(50.0);
            let mut target = position;
            let slider = ui.add_enabled(
                s.duration > 0.0 && !s.loading,
                Slider::new(&mut target, 0.0..=s.duration.max(1.0)).show_value(false),
            );
            if slider.dragged() {
                self.seek = Some(target);
            }
            if slider.drag_stopped() || (slider.changed() && !slider.dragged()) {
                self.send("seek", json!({"seconds":target}));
                self.seek = None;
            }
            time_label(ui, s.duration);
        });

        if ui.available_width() < 860.0 {
            ui.horizontal(|ui| {
                let info_width = (ui.available_width() - 165.0).max(115.0);
                self.track_info(ui, s, info_width);
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    self.volume(ui, s, 62.0);
                    self.likes(ui, s, true);
                });
            });
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                ui.add_space(((ui.available_width() - 300.0) / 2.0).max(0.0));
                self.playback_buttons(ui, s);
                ui.add_space(8.0);
                self.mode_buttons(ui, s);
            });
            return;
        }

        let width = ui.available_width();
        let (row, _) = ui.allocate_exact_size(egui::vec2(width, 48.0), egui::Sense::hover());
        let mut left = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(egui::Rect::from_min_max(
                    row.min,
                    egui::pos2(row.center().x - 86.0, row.bottom()),
                ))
                .layout(Layout::left_to_right(Align::Center)),
        );
        self.track_info(&mut left, s, (row.width() * 0.23).min(260.0));
        self.likes(&mut left, s, false);

        let center = egui::Rect::from_center_size(row.center(), egui::vec2(130.0, 48.0));
        let mut controls = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(center)
                .layout(Layout::left_to_right(Align::Center)),
        );
        controls.spacing_mut().item_spacing.x = 6.0;
        self.playback_buttons(&mut controls, s);

        let mut right = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(egui::Rect::from_min_max(
                    egui::pos2(row.center().x + 80.0, row.top()),
                    row.max,
                ))
                .layout(Layout::right_to_left(Align::Center)),
        );
        self.volume(&mut right, s, 90.0);
        right.add_space(10.0);
        right.with_layout(Layout::left_to_right(Align::Center), |ui| {
            self.mode_buttons(ui, s)
        });
    }

    fn track_info(&mut self, ui: &mut egui::Ui, s: &Snapshot, width: f32) {
        self.covers
            .draw(ui, s.track.as_ref().and_then(Track::cover), 42.0);
        let title = s
            .track
            .as_ref()
            .map_or("Выберите трек", |track| track.title.as_str());
        let subtitle = s.track.as_ref().map_or_else(
            || "Поиск или Моя волна".to_owned(),
            |track| {
                if s.wave {
                    format!("{}  ·  Моя волна", track.artist())
                } else {
                    track.artist()
                }
            },
        );
        ui.allocate_ui_with_layout(
            egui::vec2((width - 52.0).max(60.0), 42.0),
            Layout::top_down(Align::Min).with_main_align(Align::Center),
            |ui| {
                ui.set_width((width - 52.0).max(60.0));
                ui.spacing_mut().item_spacing.y = 2.0;
                ui.add(egui::Label::new(RichText::new(title).strong()).truncate());
                let artist = s
                    .track
                    .as_ref()
                    .and_then(|track| track.artists.iter().find(|artist| artist.id > 0))
                    .cloned();
                let subtitle = ui.add(
                    egui::Label::new(
                        RichText::new(subtitle)
                            .size(12.0)
                            .color(Color32::from_gray(161)),
                    )
                    .truncate()
                    .sense(egui::Sense::click()),
                );
                if let Some(artist) = artist {
                    if subtitle.on_hover_text("Об исполнителе").clicked() {
                        self.open_artist(artist.id, &artist.name);
                    }
                }
            },
        );
    }

    fn likes(&mut self, ui: &mut egui::Ui, s: &Snapshot, reversed: bool) {
        let choices = if reversed {
            [
                (ControlIcon::Unlike, "Убрать из любимого", "unlike"),
                (ControlIcon::Like, "Добавить в любимое", "like"),
            ]
        } else {
            [
                (ControlIcon::Like, "Добавить в любимое", "like"),
                (ControlIcon::Unlike, "Убрать из любимого", "unlike"),
            ]
        };
        ui.add_enabled_ui(s.track.is_some(), |ui| {
            for (icon, label, command) in choices {
                if control(ui, icon, label, false).clicked() {
                    self.send(command, json!({}));
                }
            }
        });
    }

    fn playback_buttons(&mut self, ui: &mut egui::Ui, s: &Snapshot) {
        if control(ui, ControlIcon::Previous, "Предыдущий трек", false).clicked() {
            self.send("previous", json!({}));
        }
        let (icon, label, action) = if s.playing {
            (ControlIcon::Pause, "Пауза", "pause")
        } else {
            (ControlIcon::Play, "Воспроизвести", "play")
        };
        if control(ui, icon, label, true).clicked() {
            self.send(action, json!({}));
        }
        if control(ui, ControlIcon::Next, "Следующий трек", false).clicked() {
            self.send("next", json!({}));
        }
    }

    fn mode_buttons(&mut self, ui: &mut egui::Ui, s: &Snapshot) {
        if control(
            ui,
            ControlIcon::Shuffle,
            if s.shuffle {
                "Перемешивание включено"
            } else {
                "Перемешивание выключено"
            },
            s.shuffle,
        )
        .clicked()
        {
            self.send("shuffle", json!({"enabled":!s.shuffle}));
        }
        let (repeat, next) = match s.repeat.as_str() {
            "track" => ("Повтор одного трека", "off"),
            "queue" => ("Повтор очереди", "track"),
            _ => ("Повтор выключен", "queue"),
        };
        if control(
            ui,
            ControlIcon::Repeat(s.repeat == "track"),
            repeat,
            s.repeat != "off",
        )
        .clicked()
        {
            self.send("repeat", json!({"mode":next}));
        }
        let (mini_icon, mini_label) = if self.mini {
            (ControlIcon::Expand, "Развернуть плеер")
        } else {
            (ControlIcon::Mini, "Открыть мини-плеер")
        };
        if control(ui, mini_icon, mini_label, self.mini).clicked() {
            self.mini = !self.mini;
            ui.ctx()
                .send_viewport_cmd(ViewportCommand::InnerSize(if self.mini {
                    egui::vec2(550.0, 190.0)
                } else {
                    egui::vec2(1060.0, 710.0)
                }));
            ui.ctx()
                .send_viewport_cmd(ViewportCommand::WindowLevel(if self.mini {
                    egui::WindowLevel::AlwaysOnTop
                } else {
                    egui::WindowLevel::Normal
                }));
        }
        if control(
            ui,
            ControlIcon::Equalizer,
            "Эквалайзер",
            self.equalizer_open || s.equalizer.enabled,
        )
        .clicked()
        {
            self.equalizer_open = !self.equalizer_open;
            self.eq_dragging = false;
        }
    }

    fn volume(&mut self, ui: &mut egui::Ui, s: &Snapshot, width: f32) {
        let current = self.volume_target.unwrap_or(s.volume);
        let mut volume_db = volume_to_db(current);
        ui.spacing_mut().slider_width = width;
        if ui
            .add(Slider::new(&mut volume_db, MIN_VOLUME_DB..=0.0).show_value(false))
            .changed()
        {
            self.volume_target = Some(db_to_volume(volume_db));
        }
        let percent = volume_percent(volume_db);
        ui.add_sized(
            [40.0, 18.0],
            egui::Label::new(
                RichText::new(format!("{percent:3}%"))
                    .monospace()
                    .size(11.0)
                    .color(Color32::from_gray(162)),
            )
            .truncate(),
        )
        .on_hover_text(format!("Громкость: {percent}% ({volume_db:.0} дБ)"));
    }

    fn equalizer_window(&mut self, ctx: &Context, current: EqualizerSettings) {
        if !self.equalizer_open {
            return;
        }
        let pending = self.pending.iter().any(|(name, _)| name == "equalizer");
        if !self.eq_dragging && !pending {
            self.eq_draft = current;
        }
        let mut open = self.equalizer_open;
        let mut commit = false;
        let mut dragging = false;
        egui::Window::new("Эквалайзер")
            .open(&mut open)
            .resizable(false)
            .collapsible(false)
            .anchor(
                egui::Align2::CENTER_BOTTOM,
                egui::vec2(0.0, if self.mini { -5.0 } else { -108.0 }),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    commit |= ui.checkbox(&mut self.eq_draft.enabled, "Включён").changed();
                    if ui
                        .button("Сбросить")
                        .on_hover_text("Установить все полосы на 0 дБ")
                        .clicked()
                    {
                        self.eq_draft.gains.fill(0);
                        commit = true;
                    }
                });
                ui.separator();
                ui.horizontal_top(|ui| {
                    ui.spacing_mut().item_spacing.x = 9.0;
                    for (index, (hz, gain)) in
                        FREQUENCIES.iter().zip(&mut self.eq_draft.gains).enumerate()
                    {
                        ui.push_id(index, |ui| {
                            ui.allocate_ui_with_layout(
                                egui::vec2(51.0, 0.0),
                                Layout::top_down(Align::Center),
                                |ui| {
                                    ui.set_width(51.0);
                                    ui.spacing_mut().slider_width =
                                        if self.mini { 68.0 } else { 114.0 };
                                    ui.add_sized(
                                        [48.0, 20.0],
                                        egui::Label::new(
                                            RichText::new(format!("{gain:+} дБ"))
                                                .monospace()
                                                .size(12.0),
                                        ),
                                    );
                                    let slider = ui.add(
                                        Slider::new(gain, -12..=12).vertical().show_value(false),
                                    );
                                    dragging |= slider.dragged();
                                    if slider.drag_stopped()
                                        || (slider.changed() && !slider.dragged())
                                    {
                                        self.eq_draft.enabled = true;
                                        commit = true;
                                    }
                                    ui.small(if *hz >= 1000.0 {
                                        format!("{} кГц", *hz as u32 / 1000)
                                    } else {
                                        format!("{} Гц", *hz as u32)
                                    });
                                },
                            );
                        });
                    }
                });
            });
        self.equalizer_open = open;
        self.eq_dragging = dragging;
        if commit {
            let settings = self.eq_draft;
            self.send(
                "equalizer",
                json!({"enabled":settings.enabled,"gains":settings.gains}),
            );
        }
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &Context, _frame: &mut eframe::Frame) {
        self.poll();
        self.covers.poll(ctx);
        let snapshot = self.state.lock().unwrap().clone();
        self.flush_volume(&snapshot);
        let search_shortcut = ctx.input(|input| {
            input.events.iter().any(|event| {
                matches!(event, egui::Event::Key { key: Key::K, pressed: true, modifiers, .. }
                | egui::Event::Key { physical_key: Some(Key::K), pressed: true, modifiers, .. }
                if modifiers.ctrl)
            })
        });
        if !self.mini && search_shortcut {
            self.navigate(Page::Search, snapshot.logged_in);
            self.focus_search = true;
        }
        self.shortcuts(ctx, &snapshot);
        self.text_focus = false;
        let zoom = ctx.input(|i| {
            if !i.modifiers.ctrl {
                return None;
            }
            [Key::Equals, Key::Minus, Key::Num0]
                .into_iter()
                .find(|&key| i.key_pressed(key))
        });
        match zoom {
            Some(Key::Equals) => ctx.set_zoom_factor((ctx.zoom_factor() + 0.1).min(2.0)),
            Some(Key::Minus) => ctx.set_zoom_factor((ctx.zoom_factor() - 0.1).max(0.75)),
            Some(Key::Num0) => ctx.set_zoom_factor(1.0),
            _ => {}
        }
        egui::TopBottomPanel::bottom("player")
            .frame(
                Frame::new()
                    .fill(Color32::from_rgb(24, 24, 28))
                    .inner_margin(Margin::symmetric(16, 9)),
            )
            .show(ctx, |ui| {
                self.transport(ui, &snapshot);
                let hint = if !self.status.is_empty() {
                    self.status.clone()
                } else if !snapshot.error.is_empty() {
                    snapshot.error.clone()
                } else if snapshot.loading {
                    "Загрузка трека…".to_owned()
                } else if self.busy() {
                    "Загрузка…".to_owned()
                } else {
                    String::new()
                };
                ui.add_sized(
                    [ui.available_width(), 16.0],
                    egui::Label::new(
                        RichText::new(hint)
                            .size(11.0)
                            .color(Color32::from_gray(163)),
                    )
                    .truncate(),
                );
            });
        if !self.mini {
            self.nav(ctx, &snapshot);
        }
        egui::CentralPanel::default()
            .frame(Frame::new().inner_margin(Margin::same(24)))
            .show(ctx, |ui| {
                if self.mini {
                    return;
                }
                match self.page {
                    Page::Home => self.home(ui, &snapshot),
                    Page::Search => self.search_page(ui, &snapshot),
                    Page::Library => {
                        ui.heading("Любимое");
                        if snapshot.logged_in {
                            if ui.button("Обновить").clicked() {
                                self.send("library", json!({}));
                            }
                            self.rows(ui, &self.library.clone(), None, false);
                        } else {
                            ui.label("Войдите, чтобы увидеть любимые треки.");
                            if ui.add(primary("Войти в аккаунт")).clicked() {
                                self.navigate(Page::Settings, false);
                            }
                        }
                    }
                    Page::Playlists => {
                        if let Some(playlist) = self.opened_playlist.clone() {
                            self.playlist_page(ui, &snapshot, &playlist);
                        } else if snapshot.logged_in {
                            ui.heading("Плейлисты");
                            if ui.button("Обновить").clicked() {
                                self.send("playlists", json!({}));
                            }
                            let loading =
                                self.pending.iter().any(|(name, _)| name == "playlists");
                            if loading {
                                ui.horizontal(|ui| {
                                    ui.spinner();
                                    ui.small("Загружаем плейлисты…");
                                });
                            }
                            if self.playlists.is_empty() {
                                if !loading {
                                    ui.label("Плейлисты не найдены. Нажмите «Обновить».");
                                }
                            } else {
                                let playlists = self.playlists.clone();
                                for playlist in &playlists {
                                    ui.horizontal(|ui| {
                                        self.covers
                                            .draw(ui, playlist.cover_uri.as_deref(), 40.0);
                                        ui.vertical(|ui| {
                                            if ui
                                                .add(
                                                    egui::Button::new(
                                                        RichText::new(&playlist.title)
                                                            .strong()
                                                            .color(Color32::WHITE),
                                                    )
                                                    .frame(false),
                                                )
                                                .on_hover_text("Открыть плейлист")
                                                .clicked()
                                            {
                                                self.open_playlist(playlist);
                                            }
                                            ui.label(
                                                RichText::new(format!(
                                                    "{} • {} треков",
                                                    playlist.owner, playlist.track_count
                                                ))
                                                .size(12.0)
                                                .color(Color32::from_gray(163)),
                                            );
                                        });
                                        if ui
                                            .button("Открыть")
                                            .on_hover_text("Посмотреть треки плейлиста")
                                            .clicked()
                                        {
                                            self.open_playlist(playlist);
                                        }
                                        if ui
                                            .button("Играть")
                                            .on_hover_text("Воспроизвести плейлист")
                                            .clicked()
                                        {
                                            self.send(
                                                "play_playlist",
                                                json!({"playlist_id":playlist.id}),
                                            );
                                        }
                                    });
                                }
                            }
                        } else {
                            ui.label("Войдите, чтобы увидеть плейлисты.");
                            if ui.add(primary("Войти в аккаунт")).clicked() {
                                self.navigate(Page::Settings, false);
                            }
                        }
                    }
                    Page::Artist => self.artist_page(ui, &snapshot),
                    Page::Queue => {
                        ui.heading("Очередь");
                        if snapshot.queue.is_empty() {
                            ui.label("Добавьте трек из поиска");
                        }
                        self.rows(ui, &snapshot.queue, snapshot.index, true);
                    }
                    Page::Settings => {
                        self.settings(ui, &snapshot);
                    }
                }
            });
        self.equalizer_window(ctx, snapshot.equalizer);
    }
}

fn shortcut_command(
    ctrl: bool,
    shift: bool,
    key: Option<Key>,
    s: &Snapshot,
) -> Option<(&'static str, Value)> {
    match key? {
        Key::ArrowRight if ctrl && shift => seek_command(s, 10.0),
        Key::ArrowLeft if ctrl && shift => seek_command(s, -10.0),
        Key::ArrowRight if ctrl => Some(("next", json!({}))),
        Key::ArrowLeft if ctrl => Some(("previous", json!({}))),
        Key::ArrowUp if ctrl => Some(("volume", json!({"value":(s.volume + 0.05).min(1.0)}))),
        Key::ArrowDown if ctrl => Some(("volume", json!({"value":(s.volume - 0.05).max(0.0)}))),
        Key::Space if !ctrl => Some((if s.playing { "pause" } else { "play" }, json!({}))),
        _ => None,
    }
}

fn seek_command(s: &Snapshot, delta: f64) -> Option<(&'static str, Value)> {
    if s.duration <= 0.0 || s.loading {
        return None;
    }
    let seconds = (s.position + delta).clamp(0.0, s.duration);
    Some(("seek", json!({ "seconds": seconds })))
}

fn volume_to_db(linear: f32) -> f32 {
    (20.0_f32 * linear.max(0.0001).log10()).clamp(MIN_VOLUME_DB, 0.0)
}

fn db_to_volume(db: f32) -> f32 {
    if db <= MIN_VOLUME_DB {
        0.0
    } else {
        10.0_f32.powf(db / 20.0).clamp(0.0, 1.0)
    }
}

fn volume_percent(db: f32) -> i32 {
    (((db - MIN_VOLUME_DB) / -MIN_VOLUME_DB) * 100.0).round() as i32
}

fn tracks_from(value: Value) -> Result<Vec<Track>, String> {
    if value.is_null() {
        return Ok(Vec::new());
    }
    serde_json::from_value(value).map_err(|error| format!("Не удалось прочитать треки: {error}"))
}

fn playlists_from(value: Value) -> Result<Vec<Playlist>, String> {
    if value.is_null() {
        return Ok(Vec::new());
    }
    serde_json::from_value(value)
        .map_err(|error| format!("Не удалось прочитать плейлисты: {error}"))
}

fn nav_item(ui: &mut egui::Ui, label: &str, icon: Option<Page>, active: bool) -> bool {
    let color = if active {
        YELLOW
    } else {
        Color32::from_gray(221)
    };
    let response = ui
        .scope(|ui| {
            if icon.is_some() {
                ui.spacing_mut().button_padding.x = 30.0;
            }
            ui.add_sized(
                [ui.available_width(), 39.0],
                egui::Button::new(RichText::new(label).size(15.0).color(color))
                    .selected(active)
                    .frame_when_inactive(active)
                    .corner_radius(10)
                    .right_text("")
                    .truncate(),
            )
        })
        .inner;
    if let Some(icon) = icon {
        let origin = egui::pos2(response.rect.left() + 15.0, response.rect.center().y);
        let point = |x, y| origin + egui::vec2(x, y);
        let painter = ui.painter();
        let stroke = egui::Stroke::new(1.4_f32, color);
        let line = |points: &[(f32, f32)]| {
            painter.add(egui::Shape::line(
                points.iter().map(|&(x, y)| point(x, y)).collect(),
                stroke,
            ));
        };
        match icon {
            Page::Home => line(&[
                (-6.0, -1.0),
                (0.0, -6.0),
                (6.0, -1.0),
                (5.0, -1.0),
                (5.0, 5.0),
                (-5.0, 5.0),
                (-5.0, -1.0),
            ]),
            Page::Search => {
                painter.circle_stroke(point(-1.0, -1.0), 4.0, stroke);
                line(&[(2.0, 2.0), (6.0, 6.0)]);
            }
            Page::Library => line(&[
                (0.0, 5.0),
                (-6.0, -1.0),
                (-5.0, -4.0),
                (-2.0, -5.0),
                (0.0, -3.0),
                (2.0, -5.0),
                (5.0, -4.0),
                (6.0, -1.0),
                (0.0, 5.0),
            ]),
            Page::Queue => {
                for y in [-4.0, 0.0, 4.0] {
                    line(&[(-6.0, y), (6.0, y)]);
                }
            }
            Page::Playlists => {
                painter.circle_stroke(origin, 3.0, stroke);
                painter.circle_stroke(origin, 1.0, stroke);
            }
            Page::Artist => {
                painter.circle_stroke(point(0.0, -3.0), 3.0, stroke);
                line(&[(-5.5, 6.0), (-3.0, 2.0), (3.0, 2.0), (5.5, 6.0)]);
            }
            Page::Settings => {
                painter.circle_stroke(origin, 5.0, stroke);
                painter.circle_stroke(origin, 1.8, stroke);
            }
        }
    }
    response.clicked()
}

fn primary(label: &str) -> egui::Button<'_> {
    egui::Button::new(RichText::new(label).color(Color32::from_gray(22)).strong())
        .fill(YELLOW)
        .corner_radius(20)
}

fn icon_button(symbol: &str, active: bool) -> egui::Button<'_> {
    let color = if active {
        Color32::from_rgb(31, 27, 16)
    } else {
        Color32::from_gray(231)
    };
    let mut button = egui::Button::new(RichText::new(symbol).size(15.0).color(color))
        .min_size(egui::vec2(32.0, 32.0))
        .corner_radius(22);
    if active {
        button = button.fill(YELLOW);
    }
    button
}

#[derive(Clone, Copy)]
enum ControlIcon {
    Play,
    Pause,
    Previous,
    Next,
    Shuffle,
    Repeat(bool),
    Mini,
    Expand,
    Equalizer,
    Like,
    Unlike,
}

fn time_label(ui: &mut egui::Ui, seconds: f64) {
    ui.add_sized(
        [39.0, 18.0],
        egui::Label::new(
            RichText::new(time(seconds))
                .monospace()
                .size(11.0)
                .color(Color32::from_gray(162)),
        ),
    );
}

fn control(ui: &mut egui::Ui, icon: ControlIcon, label: &str, active: bool) -> egui::Response {
    let primary = matches!(icon, ControlIcon::Play | ControlIcon::Pause);
    let side = if primary { 44.0 } else { 36.0 };
    let response = ui.add_sized(egui::Vec2::splat(side), egui::Button::new("").frame(false));
    response.widget_info(|| {
        egui::WidgetInfo::labeled(egui::WidgetType::Button, ui.is_enabled(), label)
    });
    let center = response.rect.center();
    let painter = ui.painter();
    let muted = !ui.is_enabled();
    let ink = if muted {
        Color32::from_gray(102)
    } else if primary {
        Color32::from_rgb(34, 28, 11)
    } else if active {
        YELLOW
    } else {
        Color32::from_gray(224)
    };
    let fill = if primary {
        if response.hovered() {
            Color32::from_rgb(255, 232, 117)
        } else {
            YELLOW
        }
    } else if response.hovered() || response.is_pointer_button_down_on() {
        Color32::from_rgb(56, 54, 65)
    } else if active {
        Color32::from_rgb(62, 54, 35)
    } else {
        Color32::TRANSPARENT
    };
    if fill != Color32::TRANSPARENT {
        painter.circle_filled(center, side * 0.5, fill);
    }
    if response.has_focus() {
        painter.circle_stroke(center, side * 0.5 - 1.0, egui::Stroke::new(1.0_f32, ink));
    }
    paint_control(painter, center, ink, icon);
    response.on_hover_text(label)
}

fn paint_control(painter: &egui::Painter, center: egui::Pos2, color: Color32, icon: ControlIcon) {
    let p = |x, y| center + egui::vec2(x, y);
    let stroke = egui::Stroke::new(1.65_f32, color);
    let line = |points: &[(f32, f32)]| {
        painter.add(egui::Shape::line(
            points.iter().map(|&(x, y)| p(x, y)).collect(),
            stroke,
        ));
    };
    let rect = |x1, y1, x2, y2, radius| {
        painter.rect_filled(
            egui::Rect::from_min_max(p(x1, y1), p(x2, y2)),
            radius,
            color,
        );
    };
    let triangle = |points: [(f32, f32); 3]| {
        painter.add(egui::Shape::convex_polygon(
            points.into_iter().map(|(x, y)| p(x, y)).collect(),
            color,
            egui::Stroke::NONE,
        ));
    };
    match icon {
        ControlIcon::Play => triangle([(-5.0, -8.0), (8.0, 0.0), (-5.0, 8.0)]),
        ControlIcon::Pause => {
            rect(-6.0, -8.0, -2.0, 8.0, 1);
            rect(2.0, -8.0, 6.0, 8.0, 1);
        }
        ControlIcon::Previous => {
            triangle([(6.0, -6.5), (-4.0, 0.0), (6.0, 6.5)]);
            rect(-7.0, -6.5, -5.0, 6.5, 1);
        }
        ControlIcon::Next => {
            triangle([(-6.0, -6.5), (4.0, 0.0), (-6.0, 6.5)]);
            rect(5.0, -6.5, 7.0, 6.5, 1);
        }
        ControlIcon::Shuffle => {
            line(&[(-8.0, -5.0), (-4.5, -5.0), (4.0, 5.0), (8.0, 5.0)]);
            line(&[(5.0, 2.0), (8.0, 5.0), (5.0, 8.0)]);
            line(&[(-8.0, 5.0), (-4.5, 5.0), (-2.5, 2.5)]);
            line(&[(1.0, -1.5), (4.0, -5.0), (8.0, -5.0)]);
            line(&[(5.0, -8.0), (8.0, -5.0), (5.0, -2.0)]);
        }
        ControlIcon::Repeat(one) => {
            line(&[(-8.0, -0.5), (-8.0, -4.0), (-6.0, -6.0), (8.0, -6.0)]);
            line(&[(5.0, -9.0), (8.0, -6.0), (5.0, -3.0)]);
            line(&[(8.0, 0.5), (8.0, 4.0), (6.0, 6.0), (-8.0, 6.0)]);
            line(&[(-5.0, 3.0), (-8.0, 6.0), (-5.0, 9.0)]);
            if one {
                line(&[(-1.4, -0.9), (0.6, -2.0), (0.6, 2.7)]);
            }
        }
        ControlIcon::Mini => {
            line(&[(-8.0, 2.0), (-8.0, -6.0), (8.0, -6.0), (8.0, -2.0)]);
            line(&[(-8.0, 3.0), (-2.0, 3.0)]);
            rect(-0.5, 0.0, 9.0, 6.0, 1);
        }
        ControlIcon::Expand => {
            line(&[(-8.0, -2.5), (-8.0, -7.0), (-3.5, -7.0)]);
            line(&[(3.5, -7.0), (8.0, -7.0), (8.0, -2.5)]);
            line(&[(8.0, 2.5), (8.0, 7.0), (3.5, 7.0)]);
            line(&[(-3.5, 7.0), (-8.0, 7.0), (-8.0, 2.5)]);
        }
        ControlIcon::Equalizer => {
            for (x, y) in [(-6.0, 1.0), (0.0, -3.0), (6.0, 3.0)] {
                line(&[(x, -7.5), (x, y - 1.9)]);
                line(&[(x, y + 1.9), (x, 7.5)]);
                painter.circle_stroke(p(x, y), 1.8, stroke);
            }
        }
        ControlIcon::Like | ControlIcon::Unlike => {
            line(&[
                (0.0, 7.0),
                (-6.9, 0.5),
                (-8.0, -2.1),
                (-7.0, -5.0),
                (-4.8, -6.4),
                (-2.6, -6.0),
                (0.0, -3.9),
                (2.6, -6.0),
                (4.8, -6.4),
                (7.0, -5.0),
                (8.0, -2.1),
                (6.9, 0.5),
                (0.0, 7.0),
            ]);
            if matches!(icon, ControlIcon::Unlike) {
                line(&[(-2.9, -0.5), (2.9, -0.5)]);
            }
        }
    }
}

fn draw_wave(ui: &egui::Ui, rect: egui::Rect, playing: bool) {
    let painter = ui.painter().with_clip_rect(rect);
    let center = rect.center();
    painter.rect_filled(rect, 20, Color32::from_rgb(23, 17, 40));
    let rx = (rect.width() * 0.42).min(340.0);
    let ry = rect.height() * 0.43;
    for layer in (1..=18).rev() {
        let t = layer as f32 / 18.0;
        let points = (0..64)
            .map(|i| {
                let a = i as f32 * std::f32::consts::TAU / 64.0;
                center + egui::vec2(a.cos() * rx * t, a.sin() * ry * t)
            })
            .collect();
        painter.add(egui::Shape::convex_polygon(
            points,
            Color32::from_rgba_unmultiplied(81, 45, 157, 11),
            egui::Stroke::NONE,
        ));
    }
    let phase = if playing {
        ui.input(|input| input.time) as f32 * 1.15
    } else {
        0.0
    };
    for strand in 0..19 {
        let offset = strand as f32 * 0.19;
        let points = (0..=150)
            .map(|i| {
                let a = i as f32 * std::f32::consts::TAU / 150.0;
                let ripple = (a * 3.0 + offset + phase).sin() * 0.095
                    + (a * 6.0 - phase * 0.6 + offset * 1.7).sin() * 0.045;
                let radius = 0.58 + strand as f32 * 0.014 + ripple;
                center + egui::vec2(a.cos() * rx * radius, a.sin() * ry * radius)
            })
            .collect();
        let color = if strand % 4 == 0 {
            (108, 182, 255)
        } else {
            (171, 122, 255)
        };
        painter.add(egui::Shape::line(
            points,
            egui::Stroke::new(
                1.1_f32,
                Color32::from_rgba_unmultiplied(color.0, color.1, color.2, 90),
            ),
        ));
    }
}

fn card() -> Frame {
    Frame::new()
        .fill(Color32::from_rgb(28, 28, 33))
        .corner_radius(14)
        .inner_margin(Margin::same(17))
}

fn time(seconds: f64) -> String {
    let seconds = seconds as u64;
    format!("{}:{:02}", seconds / 60, seconds % 60)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn volume_label_matches_the_handle() {
        assert_eq!(volume_percent(MIN_VOLUME_DB), 0, "левый край — 0 %");
        assert_eq!(volume_percent(-45.0), 25, "четверть хода — 25 %");
        assert_eq!(volume_percent(0.0), 100, "правый край — 100 %");
    }

    #[test]
    fn left_edge_is_silence() {
        assert_eq!(db_to_volume(MIN_VOLUME_DB), 0.0);
        assert_eq!(db_to_volume(0.0), 1.0);
        assert!(db_to_volume(-59.0) > 0.0, "чуть правее края звук уже есть");
    }

    #[test]
    fn volume_round_trip_keeps_the_handle_in_place() {
        for db in [-60.0, -45.0, -30.0, -12.0, -2.5, 0.0] {
            let applied = db_to_volume(db);
            assert!((volume_to_db(applied) - db).abs() < 0.01, "{db}");
        }
        assert!((volume_percent(volume_to_db(0.75)) - 96).abs() <= 1);
    }

    fn snapshot(position: f64, duration: f64) -> Snapshot {
        Snapshot {
            position,
            duration,
            volume: 0.5,
            ..Snapshot::default()
        }
    }

    #[test]
    fn shortcuts_map_to_player_commands() {
        let state = snapshot(50.0, 100.0);

        assert_eq!(
            shortcut_command(true, false, Some(Key::ArrowRight), &state)
                .unwrap()
                .0,
            "next"
        );
        assert_eq!(
            shortcut_command(true, false, Some(Key::ArrowLeft), &state)
                .unwrap()
                .0,
            "previous"
        );
        assert!(
            shortcut_command(false, false, Some(Key::ArrowRight), &state).is_none(),
            "стрелки без Ctrl оставлены интерфейсу"
        );
        assert!(shortcut_command(true, false, None, &state).is_none());

        let (name, args) = shortcut_command(true, true, Some(Key::ArrowRight), &state).unwrap();
        assert_eq!(name, "seek");
        assert_eq!(args["seconds"].as_f64().unwrap(), 60.0);

        let (name, args) = shortcut_command(true, false, Some(Key::ArrowUp), &state).unwrap();
        assert_eq!(name, "volume");
        assert!((args["value"].as_f64().unwrap() - 0.55).abs() < 0.001);

        assert_eq!(
            shortcut_command(false, false, Some(Key::Space), &state)
                .unwrap()
                .0,
            "play"
        );
        let playing = Snapshot {
            playing: true,
            ..state
        };
        assert_eq!(
            shortcut_command(false, false, Some(Key::Space), &playing)
                .unwrap()
                .0,
            "pause"
        );
        assert!(
            shortcut_command(true, false, Some(Key::Space), &playing).is_none(),
            "Ctrl+пробел не переключает воспроизведение"
        );
    }

    #[test]
    fn seek_stays_inside_the_track_and_waits_for_loading() {
        let state = snapshot(3.0, 100.0);
        let (_, args) = shortcut_command(true, true, Some(Key::ArrowLeft), &state).unwrap();
        assert_eq!(args["seconds"].as_f64().unwrap(), 0.0);
        let (_, args) = shortcut_command(true, true, Some(Key::ArrowRight), &state).unwrap();
        assert_eq!(args["seconds"].as_f64().unwrap(), 13.0);

        let loading = Snapshot {
            loading: true,
            ..state
        };
        assert!(
            shortcut_command(true, true, Some(Key::ArrowRight), &loading).is_none(),
            "пока грузится трек, перемотка недоступна"
        );
    }
}

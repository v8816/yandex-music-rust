use eframe::egui::{self, Color32, ColorImage, Context, TextureHandle, TextureOptions};
use reqwest::{Url, blocking::Client};
use std::{
    collections::HashMap,
    io::{Cursor, Read},
    sync::{
        Arc, Mutex,
        mpsc::{self, Receiver, SyncSender},
    },
    thread,
    time::{Duration, Instant},
};

const MAX_COVERS: usize = 40;
const MAX_BYTES: u64 = 1024 * 1024;
const UNAVAILABLE_RETRY: Duration = Duration::from_secs(5);
type LoadedCover = (String, Result<ColorImage, String>);

pub struct Covers {
    entries: HashMap<String, (u64, Cover)>,
    requests: SyncSender<String>,
    results: Receiver<LoadedCover>,
}

enum Cover {
    Loading,
    Ready(TextureHandle, Color32),
    Unavailable { retry_at: Instant, reason: String },
}

impl Covers {
    pub fn new(ctx: &Context) -> Self {
        let (requests, incoming) = mpsc::sync_channel::<String>(32);
        let incoming = Arc::new(Mutex::new(incoming));
        let (loaded, results) = mpsc::sync_channel(4);
        let (client, client_error) = match cover_client() {
            Ok(client) => (Some(client), None),
            Err(_) => (None, Some("Не удалось создать HTTP-клиент обложек")),
        };
        for _ in 0..2 {
            let (incoming, loaded, ctx, client, client_error) = (
                incoming.clone(),
                loaded.clone(),
                ctx.clone(),
                client.clone(),
                client_error,
            );
            thread::spawn(move || {
                loop {
                    let request = incoming.lock().unwrap().recv();
                    let Ok(uri) = request else { break };
                    let cover = match client.as_ref() {
                        Some(client) => download(client, &uri),
                        None => Err(client_error
                            .unwrap_or("Загрузчик обложек недоступен")
                            .into()),
                    };
                    if loaded.send((uri, cover)).is_err() {
                        break;
                    }
                    ctx.request_repaint();
                }
            });
        }
        Self {
            entries: HashMap::new(),
            requests,
            results,
        }
    }

    pub fn poll(&mut self, ctx: &Context) {
        for (uri, image) in self.results.try_iter() {
            if let Some((_, cover)) = self.entries.get_mut(&uri) {
                let retry_at = Instant::now() + UNAVAILABLE_RETRY;
                *cover = match image {
                    Err(reason) => {
                        ctx.request_repaint_after(
                            retry_at.saturating_duration_since(Instant::now()),
                        );
                        Cover::Unavailable { retry_at, reason }
                    }
                    Ok(image) => {
                        let mut rgb = [0u64; 3];
                        for pixel in &image.pixels {
                            rgb[0] += pixel.r() as u64;
                            rgb[1] += pixel.g() as u64;
                            rgb[2] += pixel.b() as u64;
                        }
                        let n = image.pixels.len().max(1) as u64;
                        let color = Color32::from_rgb(
                            (rgb[0] / n) as u8,
                            (rgb[1] / n) as u8,
                            (rgb[2] / n) as u8,
                        );
                        Cover::Ready(
                            ctx.load_texture("track-cover", image, TextureOptions::LINEAR),
                            color,
                        )
                    }
                };
            }
        }
    }

    pub fn palette(&self, uri: Option<&str>) -> Color32 {
        let ready_color = |uri: &str| match self.entries.get(uri) {
            Some((_, Cover::Ready(_, color))) => Some(*color),
            _ => None,
        };
        uri.and_then(|uri| ready_color(uri).or_else(|| ready_color(&uri.replace("%%", "400x400"))))
            .unwrap_or(Color32::from_rgb(65, 45, 90))
    }

    pub fn draw(&mut self, ui: &mut egui::Ui, uri: Option<&str>, size: f32) -> egui::Response {
        let (rect, response) =
            ui.allocate_exact_size(egui::Vec2::splat(size), egui::Sense::click());
        if !ui.is_rect_visible(rect) {
            return response;
        }
        let large_uri = if size >= 200.0 {
            uri.map(|uri| uri.replace("%%", "400x400"))
        } else {
            None
        };
        let texture = large_uri
            .as_deref()
            .or(uri)
            .and_then(|uri| self.texture(ui.ctx(), uri));
        if let Some(id) = texture {
            let image = egui::Image::new((id, rect.size())).corner_radius(8);
            image.paint_at(ui, rect);
        } else {
            ui.painter()
                .rect_filled(rect, 8, Color32::from_rgb(43, 42, 50));
            let center = rect.center();
            let scale = size * 0.22;
            let color = Color32::from_gray(156);
            ui.painter().line_segment(
                [
                    center + egui::vec2(-scale * 0.4, scale * 0.5),
                    center + egui::vec2(-scale * 0.4, -scale),
                ],
                egui::Stroke::new((size * 0.035).max(1.5), color),
            );
            ui.painter().line_segment(
                [
                    center + egui::vec2(-scale * 0.4, -scale),
                    center + egui::vec2(scale * 0.7, -scale * 0.65),
                ],
                egui::Stroke::new((size * 0.05).max(2.0), color),
            );
            ui.painter().circle_filled(
                center + egui::vec2(-scale * 0.65, scale * 0.5),
                scale * 0.45,
                color,
            );
        }
        if texture.is_none()
            && let Some(uri) = uri
        {
            if let Some((_, Cover::Unavailable { reason, .. })) = self.entries.get(uri) {
                response.clone().on_hover_text(reason);
            }
        } else if texture.is_none() && uri.is_none() {
            response
                .clone()
                .on_hover_text("Яндекс Музыка не передала ссылку на обложку");
        }
        response
    }

    fn texture(&mut self, ctx: &Context, uri: &str) -> Option<egui::TextureId> {
        let now = Instant::now();
        let frame = ctx.cumulative_frame_nr();
        if let Some((seen, entry)) = self.entries.get_mut(uri) {
            *seen = frame;
            match entry {
                Cover::Ready(texture, _) => return Some(texture.id()),
                Cover::Loading => return None,
                Cover::Unavailable { retry_at, .. } if now < *retry_at => return None,
                Cover::Unavailable { .. } => {}
            }
        }
        if self
            .entries
            .get(uri)
            .is_some_and(|(_, entry)| matches!(entry, Cover::Unavailable { .. }))
        {
            self.entries.remove(uri);
        }
        if uri.contains("400x400") {
            self.entries
                .retain(|key, _| key == uri || !key.contains("400x400"));
        }
        if self.entries.len() >= MAX_COVERS {
            let oldest = self
                .entries
                .iter()
                .filter(|(_, (_, cover))| !matches!(cover, Cover::Loading))
                .min_by_key(|(_, (seen, _))| *seen)
                .map(|(uri, _)| uri.clone());
            let oldest = oldest?;
            self.entries.remove(&oldest);
        }
        if self.requests.try_send(uri.to_owned()).is_ok() {
            self.entries.insert(uri.to_owned(), (frame, Cover::Loading));
        }
        None
    }
}

pub(crate) fn cover_url(uri: &str) -> Option<Url> {
    let uri = uri.replace("%%", "100x100");
    let mut url = Url::parse(&uri)
        .ok()
        .filter(|url| url.has_host())
        .or_else(|| Url::parse(&format!("https://{}", uri.trim_start_matches('/'))).ok())?;
    if !cover_host_allowed(&url) {
        return None;
    }
    url.set_fragment(None);
    Some(url)
}

fn cover_host_allowed(url: &Url) -> bool {
    url.scheme() == "https"
        && url.port().is_none()
        && url.username().is_empty()
        && url.password().is_none()
        && url.host_str().is_some_and(|host| {
            matches!(
                host,
                "avatars.yandex.net" | "avatars.mds.yandex.net" | "avatars.yandex.ru"
            )
        })
}

fn cover_client() -> reqwest::Result<Client> {
    Client::builder()
        .connect_timeout(Duration::from_secs(5))
        .pool_max_idle_per_host(2)
        .redirect(reqwest::redirect::Policy::custom(|attempt| {
            if attempt.previous().len() >= 5 {
                attempt.stop()
            } else if cover_host_allowed(attempt.url()) {
                attempt.follow()
            } else {
                attempt.stop()
            }
        }))
        .build()
}

fn download(client: &Client, uri: &str) -> Result<ColorImage, String> {
    let url = cover_url(uri).ok_or_else(|| "Некорректный адрес обложки".to_owned())?;
    let response = client
        .get(url)
        .timeout(Duration::from_secs(8))
        .send()
        .map_err(|error| {
            if error.is_timeout() {
                "Истекло время ожидания сервера обложек".to_owned()
            } else {
                "Не удалось получить обложку с сервера Яндекса".to_owned()
            }
        })?;
    if !response.status().is_success() {
        return Err(format!(
            "Сервер обложек ответил HTTP {}",
            response.status().as_u16()
        ));
    }
    if response.content_length().unwrap_or(0) > MAX_BYTES {
        return Err("Файл обложки превышает 1 МБ".into());
    }
    let mut bytes = Vec::new();
    response
        .take(MAX_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "Не удалось прочитать файл обложки".to_owned())?;
    if bytes.len() as u64 > MAX_BYTES {
        return Err("Файл обложки превышает 1 МБ".into());
    }
    let mut reader = image::ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|_| "Сервер прислал неизвестный формат обложки".to_owned())?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(512);
    limits.max_image_height = Some(512);
    limits.max_alloc = Some(8 * 1024 * 1024);
    reader.limits(limits);
    let pixels = reader
        .decode()
        .map_err(|_| "Не удалось декодировать изображение обложки".to_owned())?
        .into_rgba8();
    Ok(ColorImage::from_rgba_unmultiplied(
        [pixels.width() as usize, pixels.height() as usize],
        pixels.as_raw(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    #[test]
    fn cover_urls_normalize_yandex_image_uris_and_reject_external_hosts() {
        assert_eq!(
            cover_url("avatars.yandex.net/get-music-content/1/%%")
                .unwrap()
                .as_str(),
            "https://avatars.yandex.net/get-music-content/1/100x100"
        );
        assert_eq!(
            cover_url("https://avatars.mds.yandex.net/get-music-content/1/400x400")
                .unwrap()
                .host_str(),
            Some("avatars.mds.yandex.net")
        );
        assert!(cover_url("https://example.com/cover.png").is_none());
        assert!(cover_url("http://avatars.yandex.net/cover.png").is_none());
    }

    #[test]
    fn cover_redirects_stay_on_https_yandex_artwork_hosts() {
        assert!(cover_host_allowed(
            &Url::parse("https://avatars.mds.yandex.net/cover.png").unwrap()
        ));
        assert!(!cover_host_allowed(
            &Url::parse("http://avatars.yandex.net/cover.png").unwrap()
        ));
        assert!(!cover_host_allowed(
            &Url::parse("https://example.com/cover.png").unwrap()
        ));
        assert!(!cover_host_allowed(
            &Url::parse("https://avatars.yandex.net.evil.example/cover.png").unwrap()
        ));
        assert!(!cover_host_allowed(
            &Url::parse("https://user@avatars.yandex.net/cover.png").unwrap()
        ));
    }

    #[test]
    fn failed_cover_schedules_a_repaint_when_retry_is_due() {
        let ctx = Context::default();
        let delay_ms = Arc::new(AtomicU64::new(0));
        let observed_delay_ms = delay_ms.clone();
        ctx.set_request_repaint_callback(move |info| {
            observed_delay_ms.store(info.delay.as_millis() as u64, Ordering::Relaxed);
        });

        let (requests, _incoming) = mpsc::sync_channel(1);
        let (loaded, results) = mpsc::sync_channel(1);
        let uri = "https://avatars.yandex.net/cover.png".to_owned();
        loaded
            .send((uri.clone(), Err("Сетевая ошибка".into())))
            .unwrap();
        let mut covers = Covers {
            entries: HashMap::from([(uri.clone(), (0, Cover::Loading))]),
            requests,
            results,
        };

        covers.poll(&ctx);

        let scheduled_delay_ms = delay_ms.load(Ordering::Relaxed);
        assert!(
            (4_900..=UNAVAILABLE_RETRY.as_millis() as u64).contains(&scheduled_delay_ms),
            "scheduled repaint after {scheduled_delay_ms} ms"
        );
        assert!(matches!(
            covers.entries.get(&uri).map(|(_, cover)| cover),
            Some(Cover::Unavailable { .. })
        ));
    }
}

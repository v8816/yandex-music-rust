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
    time::Duration,
};

const MAX_COVERS: usize = 40;
const MAX_BYTES: u64 = 1024 * 1024;
type LoadedCover = (String, Option<ColorImage>);

pub struct Covers {
    entries: HashMap<String, (u64, Cover)>,
    requests: SyncSender<String>,
    results: Receiver<LoadedCover>,
}

enum Cover {
    Loading,
    Ready(TextureHandle, Color32),
    Unavailable,
}

impl Covers {
    pub fn new(ctx: &Context) -> Self {
        let (requests, incoming) = mpsc::sync_channel::<String>(32);
        let incoming = Arc::new(Mutex::new(incoming));
        let (loaded, results) = mpsc::sync_channel(32);
        let client = Client::builder()
            .timeout(Duration::from_secs(8))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .ok();
        for _ in 0..2 {
            let (incoming, loaded, ctx, client) = (
                incoming.clone(),
                loaded.clone(),
                ctx.clone(),
                client.clone(),
            );
            thread::spawn(move || {
                loop {
                    let request = incoming.lock().unwrap().recv();
                    let Ok(uri) = request else { break };
                    let cover = client.as_ref().and_then(|client| download(client, &uri));
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
                *cover = image.map_or(Cover::Unavailable, |image| {
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
                });
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
        let large_uri = if size >= 200.0 {
            uri.map(|uri| uri.replace("%%", "400x400"))
        } else {
            None
        };
        let texture = large_uri
            .as_deref()
            .or(uri)
            .and_then(|uri| self.texture(ui.ctx(), uri));
        let (rect, response) =
            ui.allocate_exact_size(egui::Vec2::splat(size), egui::Sense::click());
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
        response
    }

    fn texture(&mut self, ctx: &Context, uri: &str) -> Option<egui::TextureId> {
        let frame = ctx.cumulative_frame_nr();
        if let Some((seen, entry)) = self.entries.get_mut(uri) {
            *seen = frame;
            return match entry {
                Cover::Ready(texture, _) => Some(texture.id()),
                _ => None,
            };
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
    if url.scheme() != "https"
        || url.port().is_some()
        || !url.username().is_empty()
        || url.password().is_some()
        || !url.host_str().is_some_and(|host| {
            matches!(
                host,
                "avatars.yandex.net" | "avatars.mds.yandex.net" | "avatars.yandex.ru"
            )
        })
    {
        return None;
    }
    url.set_fragment(None);
    Some(url)
}

fn download(client: &Client, uri: &str) -> Option<ColorImage> {
    let response = client
        .get(cover_url(uri)?)
        .send()
        .ok()?
        .error_for_status()
        .ok()?;
    if response.content_length().unwrap_or(0) > MAX_BYTES {
        return None;
    }
    let mut bytes = Vec::new();
    response.take(MAX_BYTES + 1).read_to_end(&mut bytes).ok()?;
    if bytes.len() as u64 > MAX_BYTES {
        return None;
    }
    let mut reader = image::ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .ok()?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(512);
    limits.max_image_height = Some(512);
    limits.max_alloc = Some(8 * 1024 * 1024);
    reader.limits(limits);
    let pixels = reader.decode().ok()?.into_rgba8();
    Some(ColorImage::from_rgba_unmultiplied(
        [pixels.width() as usize, pixels.height() as usize],
        pixels.as_raw(),
    ))
}

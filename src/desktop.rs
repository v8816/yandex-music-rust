use eframe::egui;

pub fn icon() -> egui::IconData {
    let size = 64usize;
    let mut rgba = vec![0; size * size * 4];
    for y in 0..size {
        for x in 0..size {
            let dx = x as f32 - 31.5;
            let dy = y as f32 - 31.5;
            if dx * dx + dy * dy > 31.0 * 31.0 {
                continue;
            }
            let wave = [(18i32, 23i32), (27, 13), (36, 19), (45, 26)]
                .iter()
                .any(|&(cx, top)| {
                    (x as i32 - cx).abs() <= 2 && y as i32 >= top && y as i32 <= 63 - top
                });
            let color = if wave {
                [24, 22, 30, 255]
            } else {
                [255, 211 + (y / 4) as u8, 64, 255]
            };
            rgba[(y * size + x) * 4..(y * size + x) * 4 + 4].copy_from_slice(&color);
        }
    }
    egui::IconData {
        rgba,
        width: 64,
        height: 64,
    }
}

#[cfg(windows)]
pub struct Tray {
    _icon: tray_icon::TrayIcon,
    pub show: tray_icon::menu::MenuId,
    pub play: tray_icon::menu::MenuId,
    pub next: tray_icon::menu::MenuId,
    pub quit: tray_icon::menu::MenuId,
}

#[cfg(windows)]
impl Tray {
    pub fn new(_ctx: &egui::Context) -> Option<Self> {
        use tray_icon::{
            Icon, TrayIconBuilder,
            menu::{Menu, MenuItem},
        };
        let menu = Menu::new();
        let show = MenuItem::new("Открыть плеер", true, None);
        let play = MenuItem::new("Играть / пауза", true, None);
        let next = MenuItem::new("Следующий трек", true, None);
        let quit = MenuItem::new("Выйти", true, None);
        menu.append_items(&[&show, &play, &next, &quit]).ok()?;
        let data = icon();
        let icon = TrayIconBuilder::new()
            .with_tooltip("Яндекс Музыка · Rust")
            .with_icon(Icon::from_rgba(data.rgba, data.width, data.height).ok()?)
            .with_menu(Box::new(menu))
            .build()
            .ok()?;
        Some(Self {
            _icon: icon,
            show: show.id().clone(),
            play: play.id().clone(),
            next: next.id().clone(),
            quit: quit.id().clone(),
        })
    }
}

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
}

#[cfg(windows)]
fn show_player(ctx: &egui::Context) {
    use windows::Win32::{
        Foundation::{BOOL, HWND, LPARAM},
        UI::WindowsAndMessaging::{
            EnumWindows, GetWindowTextLengthW, GetWindowThreadProcessId, SW_RESTORE,
            SetForegroundWindow, ShowWindow,
        },
    };
    unsafe extern "system" fn find_own_window(window: HWND, data: LPARAM) -> BOOL {
        let mut pid = 0;
        unsafe {
            GetWindowThreadProcessId(window, Some(&mut pid));
        }
        if pid == std::process::id() && unsafe { GetWindowTextLengthW(window) } > 0 {
            unsafe {
                *(data.0 as *mut HWND) = window;
            }
            return BOOL(0);
        }
        BOOL(1)
    }
    unsafe {
        let mut window = HWND::default();
        let _ = EnumWindows(
            Some(find_own_window),
            LPARAM(&mut window as *mut HWND as isize),
        );
        if window.0 != 0 {
            let _ = ShowWindow(window, SW_RESTORE);
            let _ = SetForegroundWindow(window);
        }
    }
    ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
    ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
    ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
    ctx.request_repaint();
}

#[cfg(windows)]
impl Tray {
    pub fn new(
        ctx: &egui::Context,
        tx: std::sync::mpsc::SyncSender<crate::player::Job>,
        state: std::sync::Arc<std::sync::Mutex<crate::player::Snapshot>>,
    ) -> Option<Self> {
        use tray_icon::{
            Icon, TrayIconBuilder, TrayIconEvent,
            menu::{Menu, MenuEvent, MenuItem},
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
        let (show, play, next, quit) = (
            show.id().clone(),
            play.id().clone(),
            next.id().clone(),
            quit.id().clone(),
        );
        let context = ctx.clone();
        MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
            if event.id == show {
                show_player(&context);
            } else if event.id == play {
                let playing = state.lock().map(|s| s.playing).unwrap_or(false);
                let _ = crate::player::submit(
                    &tx,
                    if playing { "pause" } else { "play" },
                    serde_json::json!({}),
                );
            } else if event.id == next {
                let _ = crate::player::submit(&tx, "next", serde_json::json!({}));
            } else if event.id == quit {
                if let Ok(reply) = crate::player::submit(&tx, "stop", serde_json::json!({})) {
                    let _ = reply.recv_timeout(std::time::Duration::from_secs(1));
                }
                std::process::exit(0);
            }
            context.request_repaint();
        }));
        let context = ctx.clone();
        TrayIconEvent::set_event_handler(Some(move |event| {
            if matches!(event, TrayIconEvent::DoubleClick { .. }) {
                show_player(&context);
            }
        }));
        Some(Self { _icon: icon })
    }
}

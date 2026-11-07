
use crate::player::{Job, Snapshot};
use serde_json::json;
use std::sync::{Arc, mpsc::SyncSender};

#[cfg(windows)]
pub use platform::{MediaKeys, start};

#[cfg(windows)]
mod platform {
    use super::*;
    use crate::player;
    use std::{
        sync::{
            atomic::{AtomicBool, Ordering},
            mpsc, Mutex,
        },
        thread,
    };
    use windows::{
        Win32::{
            Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, WPARAM},
            System::Threading::GetCurrentThreadId,
            UI::{
                Input::KeyboardAndMouse::{
                    MOD_NOREPEAT, RegisterHotKey, UnregisterHotKey, VK_MEDIA_NEXT_TRACK,
                    VK_MEDIA_PLAY_PAUSE, VK_MEDIA_PREV_TRACK,
                },
                WindowsAndMessaging::{
                    CallNextHookEx, GetMessageW, HC_ACTION, HHOOK, KBDLLHOOKSTRUCT, MSG,
                    PostThreadMessageW, SetWindowsHookExW, UnhookWindowsHookEx, WH_KEYBOARD_LL,
                    WM_HOTKEY, WM_KEYDOWN, WM_QUIT, WM_SYSKEYDOWN,
                },
            },
        },
        core::Error,
    };

    const PLAY_PAUSE: i32 = 1;
    const NEXT: i32 = 2;
    const PREVIOUS: i32 = 3;

    struct HookState {
        tx: SyncSender<Job>,
        state: Arc<Mutex<Snapshot>>,
    }

    static HOOK: Mutex<Option<HookState>> = Mutex::new(None);

    pub struct MediaKeys {
        thread: u32,
        running: Arc<AtomicBool>,
        hook: Option<HHOOK>,
    }

    impl MediaKeys {
        pub fn start(tx: SyncSender<Job>, state: Arc<Mutex<Snapshot>>) -> Option<Self> {
            let (report, ready) = mpsc::channel::<Result<(u32, Option<HHOOK>), Error>>();
            let running = Arc::new(AtomicBool::new(true));
            let alive = running.clone();
            thread::spawn(move || {
                let mut registered = Vec::new();
                let mut hook = None;
                if register(&mut registered).is_err() {
                    if let Ok(mut slot) = HOOK.lock() {
                        *slot = Some(HookState {
                            tx: tx.clone(),
                            state: state.clone(),
                        });
                    }
                    let installed = unsafe {
                        SetWindowsHookExW(
                            WH_KEYBOARD_LL,
                            Some(keyboard_hook),
                            HINSTANCE::default(),
                            0,
                        )
                    };
                    match installed {
                        Ok(hook_handle) => hook = Some(hook_handle),
                        Err(error) => {
                            clear_hook();
                            let _ = report.send(Err(error));
                            return;
                        }
                    }
                }
                let thread = unsafe { GetCurrentThreadId() };
                if report.send(Ok((thread, hook))).is_err() {
                    release(&registered);
                    clear_hook();
                    return;
                }
                let mut message = MSG::default();
                while alive.load(Ordering::Acquire) {
                    if !unsafe { GetMessageW(&mut message, HWND::default(), 0, 0) }.as_bool() {
                        break;
                    }
                    if message.message != WM_HOTKEY {
                        continue;
                    }
                    if let Some(command) = hotkey_command(message.wParam.0 as i32, &state) {
                        let _ = player::submit(&tx, command, json!({}));
                    }
                }
                release(&registered);
                if let Some(hook) = hook {
                    unsafe {
                        let _ = UnhookWindowsHookEx(hook);
                    }
                }
                clear_hook();
            });
            match ready.recv().ok()? {
                Ok((thread, hook)) => Some(Self {
                    thread,
                    running,
                    hook,
                }),
                Err(_) => None,
            }
        }

        #[cfg(test)]
        pub fn uses_hook(&self) -> bool {
            self.hook.is_some()
        }
    }

    unsafe extern "system" fn keyboard_hook(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
        if code == HC_ACTION as i32 {
            let message = wparam.0;
            if message == WM_KEYDOWN as usize || message == WM_SYSKEYDOWN as usize {
                let info = unsafe { &*(lparam.0 as *const KBDLLHOOKSTRUCT) };
                if let Ok(guard) = HOOK.lock() {
                    if let Some(shared) = guard.as_ref() {
                        if let Some(command) = media_key_command(info.vkCode, &shared.state) {
                            let _ = player::submit(&shared.tx, command, json!({}));
                            return LRESULT(1);
                        }
                    }
                }
            }
        }
        unsafe { CallNextHookEx(HHOOK::default(), code, wparam, lparam) }
    }

    fn media_key_command(vk: u32, state: &Arc<Mutex<Snapshot>>) -> Option<&'static str> {
        if vk == u32::from(VK_MEDIA_NEXT_TRACK.0) {
            Some("next")
        } else if vk == u32::from(VK_MEDIA_PREV_TRACK.0) {
            Some("previous")
        } else if vk == u32::from(VK_MEDIA_PLAY_PAUSE.0) {
            let playing = state.lock().map(|s| s.playing).unwrap_or(false);
            Some(if playing { "pause" } else { "play" })
        } else {
            None
        }
    }

    fn hotkey_command(id: i32, state: &Arc<Mutex<Snapshot>>) -> Option<&'static str> {
        match id {
            NEXT => Some("next"),
            PREVIOUS => Some("previous"),
            PLAY_PAUSE => {
                let playing = state.lock().map(|s| s.playing).unwrap_or(false);
                Some(if playing { "pause" } else { "play" })
            }
            _ => None,
        }
    }

    fn register(registered: &mut Vec<i32>) -> Result<(), Error> {
        for (id, key) in [
            (PLAY_PAUSE, VK_MEDIA_PLAY_PAUSE),
            (NEXT, VK_MEDIA_NEXT_TRACK),
            (PREVIOUS, VK_MEDIA_PREV_TRACK),
        ] {
            let result =
                unsafe { RegisterHotKey(HWND::default(), id, MOD_NOREPEAT, u32::from(key.0)) };
            if let Err(error) = result {
                release(registered);
                return Err(error);
            }
            registered.push(id);
        }
        Ok(())
    }

    fn release(registered: &[i32]) {
        for id in registered {
            let _ = unsafe { UnregisterHotKey(HWND::default(), *id) };
        }
    }

    fn clear_hook() {
        if let Ok(mut slot) = HOOK.lock() {
            *slot = None;
        }
    }

    pub fn start(tx: SyncSender<Job>, state: Arc<Mutex<Snapshot>>) -> Option<MediaKeys> {
        MediaKeys::start(tx, state)
    }

    impl Drop for MediaKeys {
        fn drop(&mut self) {
            self.running.store(false, Ordering::Release);
            unsafe {
                let _ = PostThreadMessageW(self.thread, WM_QUIT, WPARAM(0), LPARAM(0));
            }
            if let Some(hook) = self.hook {
                unsafe {
                    let _ = UnhookWindowsHookEx(hook);
                }
            }
            clear_hook();
        }
    }
}

#[cfg(not(windows))]
pub struct MediaKeys;

#[cfg(not(windows))]
pub fn start(_: SyncSender<Job>, _: Arc<std::sync::Mutex<Snapshot>>) -> Option<MediaKeys> {
    None
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[test]
    #[ignore = "registers real system-wide media hotkeys while running"]
    fn hotkeys_fall_back_to_a_hook_and_are_released_on_drop() {
        let (tx, state) = crate::player::start(|| {});
        let first = start(tx.clone(), state.clone()).expect("клавиши должны захватываться");
        assert!(
            !first.uses_hook(),
            "свободные клавиши берутся горячими клавишами"
        );
        let second = start(tx.clone(), state.clone()).expect("занятые клавиши ловятся хуком");
        assert!(second.uses_hook(), "второй экземпляр переходит на хук");
        drop(second);
        drop(first);
        let mut again = None;
        for _ in 0..40 {
            if let Some(keys) = start(tx.clone(), state.clone()) {
                again = Some(keys);
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        let again = again.expect("после освобождения клавиши снова доступны");
        assert!(!again.uses_hook(), "клавиши снова берутся горячими клавишами");
    }

    #[test]
    #[ignore = "presses synthetic media keys and drives the real player thread"]
    fn next_media_key_switches_the_track() {
        let (tx, state) = crate::player::start(|| {});
        let _keys = start(tx.clone(), state.clone()).expect("клавиши должны захватываться");
        play_first_track(&tx, &state);
        press_next_media_key();
        assert!(
            wait_for_track(&state, 2),
            "нажатие мультимедийной клавиши переключает трек"
        );
    }

    #[test]
    #[ignore = "registers real hotkeys and a keyboard hook while running"]
    fn hook_mode_handles_media_keys_when_hotkeys_are_busy() {
        let (tx, state) = crate::player::start(|| {});
        let _hotkeys = start(tx.clone(), state.clone()).expect("первый экземпляр");
        let hook = start(tx.clone(), state.clone()).expect("второй экземпляр на хуке");
        assert!(hook.uses_hook(), "второй экземпляр переходит на хук");
        play_first_track(&tx, &state);
        press_next_media_key();
        assert!(
            wait_for_track(&state, 2),
            "хук тоже переключает трек, даже когда клавиша занята"
        );
    }

    fn play_first_track(tx: &SyncSender<Job>, state: &Arc<Mutex<Snapshot>>) {
        let tracks = json!([
            {"id": 1, "title": "Первая", "artists": [], "albums": [], "durationMs": 1000},
            {"id": 2, "title": "Вторая", "artists": [], "albums": [], "durationMs": 1000}
        ]);
        crate::player::submit(tx, "play_results", json!({"tracks":tracks, "index":0}))
            .unwrap()
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap()
            .unwrap();
        assert_eq!(current(state), Some(1));
    }

    fn press_next_media_key() {
        use windows::Win32::UI::Input::KeyboardAndMouse::{
            KEYBD_EVENT_FLAGS, KEYEVENTF_KEYUP, VK_MEDIA_NEXT_TRACK, keybd_event,
        };

        unsafe {
            keybd_event(VK_MEDIA_NEXT_TRACK.0 as u8, 0, KEYBD_EVENT_FLAGS(0), 0);
            keybd_event(VK_MEDIA_NEXT_TRACK.0 as u8, 0, KEYEVENTF_KEYUP, 0);
        }
    }

    fn wait_for_track(state: &Arc<Mutex<Snapshot>>, id: u64) -> bool {
        (0..60).any(|_| {
            std::thread::sleep(std::time::Duration::from_millis(50));
            current(state) == Some(id)
        })
    }

    fn current(state: &Arc<Mutex<Snapshot>>) -> Option<u64> {
        state
            .lock()
            .unwrap()
            .track
            .as_ref()
            .map(|track| track.id)
    }
}

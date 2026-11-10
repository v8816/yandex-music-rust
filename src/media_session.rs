use crate::player::{Job, Snapshot, submit};
use serde_json::json;
use std::sync::mpsc::SyncSender;
use windows::{
    Foundation::{EventRegistrationToken, TimeSpan, TypedEventHandler, Uri},
    Media::{
        MediaPlaybackStatus, MediaPlaybackType, SystemMediaTransportControls,
        SystemMediaTransportControlsButton, SystemMediaTransportControlsButtonPressedEventArgs,
        SystemMediaTransportControlsTimelineProperties,
    },
    Storage::Streams::RandomAccessStreamReference,
    Win32::{
        Foundation::HWND,
        System::WinRT::{
            ISystemMediaTransportControlsInterop, RO_INIT_MULTITHREADED, RoInitialize,
            RoUninitialize,
        },
    },
    core::{HSTRING, Result, factory},
};

struct Apartment;

impl Drop for Apartment {
    fn drop(&mut self) {
        unsafe { RoUninitialize() };
    }
}

pub struct Session {
    controls: SystemMediaTransportControls,
    button_event: EventRegistrationToken,
    track_id: Option<u64>,
    status: MediaPlaybackStatus,
    position_second: u64,
    _apartment: Apartment,
}

impl Session {
    pub fn new(hwnd: isize, tx: SyncSender<Job>) -> Result<Self> {
        unsafe { RoInitialize(RO_INIT_MULTITHREADED)? };
        let apartment = Apartment;
        let interop =
            factory::<SystemMediaTransportControls, ISystemMediaTransportControlsInterop>()?;
        let controls: SystemMediaTransportControls = unsafe { interop.GetForWindow(HWND(hwnd))? };
        controls.SetIsEnabled(false)?;
        controls.SetIsPlayEnabled(true)?;
        controls.SetIsPauseEnabled(true)?;
        controls.SetIsNextEnabled(true)?;
        controls.SetIsPreviousEnabled(true)?;
        let button_event = controls.ButtonPressed(&TypedEventHandler::<
            SystemMediaTransportControls,
            SystemMediaTransportControlsButtonPressedEventArgs,
        >::new(move |_, args| {
            let Some(args) = args else {
                return Ok(());
            };
            let command = match args.Button()? {
                SystemMediaTransportControlsButton::Play => "play",
                SystemMediaTransportControlsButton::Pause => "pause",
                SystemMediaTransportControlsButton::Next => "next",
                SystemMediaTransportControlsButton::Previous => "previous",
                _ => return Ok(()),
            };
            let _ = submit(&tx, command, json!({}));
            Ok(())
        }))?;
        Ok(Self {
            controls,
            button_event,
            track_id: None,
            status: MediaPlaybackStatus::Closed,
            position_second: 0,
            _apartment: apartment,
        })
    }

    pub fn update(&mut self, snapshot: &Snapshot) {
        let _ = self.publish(snapshot);
    }

    fn publish(&mut self, snapshot: &Snapshot) -> Result<()> {
        let Some(track) = &snapshot.track else {
            if self.track_id.is_some() {
                self.controls
                    .SetPlaybackStatus(MediaPlaybackStatus::Closed)?;
                self.controls.SetIsEnabled(false)?;
                let display = self.controls.DisplayUpdater()?;
                display.ClearAll()?;
                display.Update()?;
                self.track_id = None;
                self.status = MediaPlaybackStatus::Closed;
            }
            return Ok(());
        };
        let changed_track = self.track_id != Some(track.id);
        if changed_track {
            let display = self.controls.DisplayUpdater()?;
            display.ClearAll()?;
            display.SetType(MediaPlaybackType::Music)?;
            let music = display.MusicProperties()?;
            music.SetTitle(&HSTRING::from(track.title.as_str()))?;
            music.SetArtist(&HSTRING::from(track.artist()))?;
            if let Some(url) = track.cover().and_then(crate::covers::cover_url)
                && let Ok(uri) = Uri::CreateUri(&HSTRING::from(url.as_str()))
                && let Ok(art) = RandomAccessStreamReference::CreateFromUri(&uri)
            {
                let _ = display.SetThumbnail(&art);
            }
            display.Update()?;
            self.controls.SetIsEnabled(true)?;
            self.track_id = Some(track.id);
        }
        let status = if snapshot.loading {
            MediaPlaybackStatus::Changing
        } else if snapshot.playing {
            MediaPlaybackStatus::Playing
        } else {
            MediaPlaybackStatus::Paused
        };
        let changed_status = self.status != status;
        if changed_status {
            self.controls.SetPlaybackStatus(status)?;
            self.status = status;
        }
        let position = snapshot.position.clamp(0.0, snapshot.duration);
        let position_second = position as u64;
        if changed_track || changed_status || position_second != self.position_second {
            let timeline = SystemMediaTransportControlsTimelineProperties::new()?;
            timeline.SetEndTime(span(snapshot.duration))?;
            timeline.SetPosition(span(position))?;
            self.controls.UpdateTimelineProperties(&timeline)?;
            self.position_second = position_second;
        }
        Ok(())
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.controls.RemoveButtonPressed(self.button_event);
        let _ = self.controls.SetIsEnabled(false);
    }
}

fn span(seconds: f64) -> TimeSpan {
    TimeSpan {
        Duration: (seconds * 10_000_000.0) as i64,
    }
}

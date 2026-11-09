use crate::api::{Api, Track};
use std::{
    collections::VecDeque,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
        mpsc::{self, Receiver, Sender},
    },
    thread,
};

const CACHE_TRACKS: usize = 1;

pub enum Loaded {
    Stream {
        generation: u64,
        audio: crate::api::AudioStream,
    },
    Failed {
        generation: u64,
        error: String,
    },
}

impl Loaded {
    pub fn generation(&self) -> u64 {
        match self {
            Self::Stream { generation, .. } | Self::Failed { generation, .. } => *generation,
        }
    }
}

enum Request {
    Play {
        api: Api,
        track: Track,
        lossless: bool,
        generation: u64,
    },
    Prefetch {
        api: Api,
        track: Track,
        lossless: bool,
    },
}

struct Cached {
    track: u64,
    lossless: bool,
    audio: crate::api::AudioStream,
}

pub struct Loader {
    requests: Sender<Request>,
    prefetches: mpsc::SyncSender<Request>,
    loaded: Receiver<Loaded>,
    latest: Arc<AtomicU64>,
    generation: u64,
}

impl Loader {
    pub fn new() -> Self {
        let (requests, incoming) = mpsc::channel::<Request>();
        let (results, loaded) = mpsc::channel::<Loaded>();
        let latest = Arc::new(AtomicU64::new(0));
        let cache = Arc::new(std::sync::Mutex::new(VecDeque::new()));
        let cancelled = latest.clone();
        let foreground_cache = cache.clone();
        thread::spawn(move || run(incoming, results, cancelled, foreground_cache));
        let (prefetches, pending) = mpsc::sync_channel(1);
        let cancelled = latest.clone();
        thread::spawn(move || prefetch_worker(pending, cancelled, cache));
        Self {
            requests,
            prefetches,
            loaded,
            latest,
            generation: 0,
        }
    }

    pub fn play(&mut self, api: &Api, track: &Track, lossless: bool) -> u64 {
        self.generation += 1;
        self.latest.store(self.generation, Ordering::Relaxed);
        let _ = self.requests.send(Request::Play {
            api: api.clone(),
            track: track.clone(),
            lossless,
            generation: self.generation,
        });
        self.generation
    }

    pub fn prefetch(&self, api: &Api, track: &Track, lossless: bool) {
        let _ = self.prefetches.try_send(Request::Prefetch {
            api: api.clone(),
            track: track.clone(),
            lossless,
        });
    }

    pub fn cancel(&mut self) -> u64 {
        self.generation += 1;
        self.latest.store(self.generation, Ordering::Relaxed);
        self.generation
    }

    pub fn poll(&self) -> Vec<Loaded> {
        self.loaded.try_iter().collect()
    }
}

type Cache = Arc<std::sync::Mutex<VecDeque<Cached>>>;

fn run(incoming: Receiver<Request>, loaded: Sender<Loaded>, latest: Arc<AtomicU64>, cache: Cache) {
    while let Ok(first) = incoming.recv() {
        let mut play = None;
        let mut prefetches = Vec::new();
        for request in std::iter::once(first).chain(incoming.try_iter()) {
            match request {
                Request::Play {
                    api,
                    track,
                    lossless,
                    generation,
                } => {
                    if generation == latest.load(Ordering::Relaxed) {
                        play = Some((api, track, lossless, generation));
                    }
                }
                Request::Prefetch {
                    api,
                    track,
                    lossless,
                } => prefetches.push((api, track, lossless)),
            }
        }
        if let Some((api, track, lossless, generation)) = play {
            if let Some(cached) = take(
                &mut cache.lock().unwrap_or_else(|e| e.into_inner()),
                track.id,
                lossless,
            ) {
                let _ = loaded.send(Loaded::Stream {
                    generation,
                    audio: cached.audio,
                });
            } else {
                let cancelled = latest.clone();
                let keep = move || cancelled.load(Ordering::Relaxed) == generation;
                match api.stream_audio(track.id, lossless, keep) {
                    Ok(audio) => {
                        let _ = loaded.send(Loaded::Stream { generation, audio });
                    }
                    Err(error) => {
                        let _ = loaded.send(Loaded::Failed {
                            generation,
                            error: error.to_string(),
                        });
                    }
                }
            }
        }
    }
}

fn prefetch_worker(incoming: Receiver<Request>, latest: Arc<AtomicU64>, cache: Cache) {
    while let Ok(Request::Prefetch {
        api,
        track,
        lossless,
    }) = incoming.recv()
    {
        let mark = latest.load(Ordering::Relaxed);
        let cancelled = latest.clone();
        if let Ok(audio) = api.stream_audio(track.id, lossless, move || {
            cancelled.load(Ordering::Relaxed) == mark
        }) {
            audio.buffer.wait_finished();
            if latest.load(Ordering::Relaxed) != mark || !audio.buffer.is_complete() {
                continue;
            }
            let mut cache = cache.lock().unwrap_or_else(|e| e.into_inner());
            while cache.len() >= CACHE_TRACKS {
                cache.pop_front();
            }
            cache.push_back(Cached {
                track: track.id,
                lossless,
                audio,
            });
        }
    }
}

fn take(cache: &mut VecDeque<Cached>, track: u64, lossless: bool) -> Option<Cached> {
    let index = cache
        .iter()
        .position(|cached| cached.track == track && cached.lossless == lossless)?;
    cache.remove(index)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cached(track: u64, lossless: bool) -> Cached {
        Cached {
            track,
            lossless,
            audio: {
                let buffer = crate::stream::SharedBuffer::new(1024, None).unwrap();
                buffer.finish();
                crate::api::AudioStream {
                    reader: buffer.reader(),
                    buffer,
                    codec: "mp3".into(),
                }
            },
        }
    }

    #[test]
    fn prefetch_cache_matches_track_and_quality() {
        let mut cache = VecDeque::from([cached(1, false), cached(2, false)]);
        assert!(
            take(&mut cache, 2, true).is_none(),
            "РґСЂСѓРіРѕРµ РєР°С‡РµСЃС‚РІРѕ РЅРµ РїРѕРґС…РѕРґРёС‚"
        );
        let hit = take(&mut cache, 2, false)
            .expect("РїСЂРµРґР·Р°РіСЂСѓР¶РµРЅРЅС‹Р№ С‚СЂРµРє РЅР°Р№РґРµРЅ");
        assert_eq!(hit.track, 2);
        assert_eq!(
            cache.len(),
            1,
            "РёСЃРїРѕР»СЊР·РѕРІР°РЅРЅР°СЏ Р·Р°РїРёСЃСЊ СѓРґР°Р»СЏРµС‚СЃСЏ РёР· РєСЌС€Р°"
        );
        assert!(
            take(&mut cache, 2, false).is_none(),
            "РїРѕРІС‚РѕСЂРЅРѕ Р·Р°РїРёСЃСЊ РЅРµ РІС‹РґР°С‘С‚СЃСЏ"
        );
    }
}

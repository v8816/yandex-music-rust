
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

const CACHE_TRACKS: usize = 2;
const CACHE_BYTES: usize = 64 * 1024 * 1024;

pub enum Loaded {
    Audio {
        generation: u64,
        bytes: Vec<u8>,
        codec: String,
    },
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
            Self::Audio { generation, .. }
            | Self::Stream { generation, .. }
            | Self::Failed { generation, .. } => *generation,
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
    bytes: Vec<u8>,
    codec: String,
}

pub struct Loader {
    requests: Sender<Request>,
    loaded: Receiver<Loaded>,
    latest: Arc<AtomicU64>,
    generation: u64,
}

impl Loader {
    pub fn new() -> Self {
        let (requests, incoming) = mpsc::channel::<Request>();
        let (results, loaded) = mpsc::channel::<Loaded>();
        let latest = Arc::new(AtomicU64::new(0));
        let cancelled = latest.clone();
        thread::spawn(move || run(incoming, results, cancelled));
        Self {
            requests,
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
        let _ = self.requests.send(Request::Prefetch {
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

fn run(incoming: Receiver<Request>, loaded: Sender<Loaded>, latest: Arc<AtomicU64>) {
    let mut cache: VecDeque<Cached> = VecDeque::new();
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
            if let Some(cached) = take(&mut cache, track.id, lossless) {
                let _ = loaded.send(Loaded::Audio {
                    generation,
                    bytes: cached.bytes,
                    codec: cached.codec,
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
        for (api, track, lossless) in prefetches {
            if cache
                .iter()
                .any(|cached| cached.track == track.id && cached.lossless == lossless)
            {
                continue;
            }
            let mark = latest.load(Ordering::Relaxed);
            let cancelled = latest.clone();
            let keep = move || cancelled.load(Ordering::Relaxed) == mark;
            if let Ok(Some((bytes, codec))) = api.audio_while(track.id, lossless, keep) {
                cache.push_back(Cached {
                    track: track.id,
                    lossless,
                    bytes,
                    codec,
                });
                while cache.len() > CACHE_TRACKS
                    || (cache.len() > 1
                        && cache.iter().map(|cached| cached.bytes.len()).sum::<usize>()
                            > CACHE_BYTES)
                {
                    cache.pop_front();
                }
            }
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
            bytes: vec![0; 4],
            codec: "mp3".into(),
        }
    }

    #[test]
    fn prefetch_cache_matches_track_and_quality() {
        let mut cache = VecDeque::from([cached(1, false), cached(2, false)]);
        assert!(take(&mut cache, 2, true).is_none(), "другое качество не подходит");
        let hit = take(&mut cache, 2, false).expect("предзагруженный трек найден");
        assert_eq!(hit.track, 2);
        assert_eq!(cache.len(), 1, "использованная запись удаляется из кэша");
        assert!(take(&mut cache, 2, false).is_none(), "повторно запись не выдаётся");
    }
}

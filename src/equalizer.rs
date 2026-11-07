use rodio::{ChannelCount, SampleRate, Source, source::SeekError};
use serde::{Deserialize, Serialize};
use std::{
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

pub const FREQUENCIES: [f32; 5] = [60.0, 250.0, 1000.0, 4000.0, 12000.0];

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Settings {
    pub enabled: bool,
    pub gains: [i8; FREQUENCIES.len()],
}

#[derive(Clone, Default)]
pub struct Control(Arc<AtomicU64>);

impl Control {
    pub fn set(&self, settings: Settings) {
        let [bass, low_mid, mid, high_mid, treble] = settings.gains.map(|gain| gain as u8);
        self.0.store(
            u64::from_le_bytes([
                bass,
                low_mid,
                mid,
                high_mid,
                treble,
                settings.enabled.into(),
                0,
                0,
            ]),
            Ordering::Relaxed,
        );
    }
}

#[derive(Clone, Copy)]
struct Filter {
    b0: f32,
    b1: f32,
    b2: f32,
    a1: f32,
    a2: f32,
}

impl Filter {
    fn peaking(hz: f32, gain: i8, rate: SampleRate) -> Self {
        let angle = std::f32::consts::TAU * hz / rate as f32;
        let (sin, cos) = angle.sin_cos();
        let alpha = sin / (2.0 * 0.85);
        let amplitude = 10.0_f32.powf(f32::from(gain) / 40.0);
        let denominator = 1.0 + alpha / amplitude;
        Self {
            b0: (1.0 + alpha * amplitude) / denominator,
            b1: -2.0 * cos / denominator,
            b2: (1.0 - alpha * amplitude) / denominator,
            a1: -2.0 * cos / denominator,
            a2: (1.0 - alpha / amplitude) / denominator,
        }
    }

    fn apply(self, input: f32, delay: &mut [f32; 2]) -> f32 {
        let out = self.b0 * input + delay[0];
        delay[0] = self.b1 * input - self.a1 * out + delay[1];
        delay[1] = self.b2 * input - self.a2 * out;
        out
    }
}

pub struct Equalized<S> {
    source: S,
    control: Control,
    applied: Option<u64>,
    rate: SampleRate,
    channel: usize,
    until_refresh: usize,
    filters: [Option<Filter>; FREQUENCIES.len()],
    delays: Vec<[[f32; 2]; FREQUENCIES.len()]>,
    headroom: f32,
    active: bool,
}

impl<S: Source> Equalized<S> {
    pub fn new(source: S, control: Control) -> Self {
        let rate = source.sample_rate();
        let delays = vec![[[0.0; 2]; FREQUENCIES.len()]; usize::from(source.channels())];
        Self {
            source,
            control,
            applied: None,
            rate,
            channel: 0,
            until_refresh: 0,
            filters: [None; FREQUENCIES.len()],
            delays,
            headroom: 1.0,
            active: false,
        }
    }

    fn refresh(&mut self) {
        let settings = self.control.0.load(Ordering::Relaxed);
        let rate = self.source.sample_rate();
        let channels = usize::from(self.source.channels());
        if self.applied == Some(settings) && self.rate == rate && self.delays.len() == channels {
            return;
        }
        self.applied = Some(settings);
        if self.delays.len() != channels {
            self.channel = 0;
        }
        self.rate = rate;
        self.delays.resize(channels, [[0.0; 2]; FREQUENCIES.len()]);
        let bytes = settings.to_le_bytes();
        let enabled = bytes[5] != 0;
        let mut boost_db = 0.0_f32;
        for (index, &hz) in FREQUENCIES.iter().enumerate() {
            let gain = bytes[index] as i8;
            let filter = if enabled && gain != 0 && hz < rate as f32 * 0.48 {
                boost_db = boost_db.max(f32::from(gain));
                Some(Filter::peaking(hz, gain, rate))
            } else {
                None
            };
            self.filters[index] = filter;
        }
        self.delays.fill([[0.0; 2]; FREQUENCIES.len()]);
        self.active = self.filters.iter().any(Option::is_some);
        self.headroom = 10.0_f32.powf(-boost_db / 20.0);
    }
}

impl<S: Source> Iterator for Equalized<S> {
    type Item = f32;

    fn next(&mut self) -> Option<Self::Item> {
        if self.until_refresh == 0 {
            self.refresh();
            self.until_refresh = 128 * self.delays.len().max(1);
        }
        let sample = self.source.next()?;
        self.until_refresh -= 1;
        let channel = self.channel;
        self.channel = if self.channel + 1 < self.delays.len() {
            self.channel + 1
        } else {
            0
        };
        if !self.active {
            return Some(sample);
        }
        let mut out = sample * self.headroom;
        for (filter, delay) in self.filters.iter().zip(&mut self.delays[channel]) {
            if let Some(filter) = filter {
                out = filter.apply(out, delay);
            }
        }
        Some(out.clamp(-1.0, 1.0))
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.source.size_hint()
    }
}

impl<S: Source> Source for Equalized<S> {
    fn current_span_len(&self) -> Option<usize> {
        self.source.current_span_len()
    }
    fn channels(&self) -> ChannelCount {
        self.source.channels()
    }
    fn sample_rate(&self) -> SampleRate {
        self.source.sample_rate()
    }
    fn total_duration(&self) -> Option<Duration> {
        self.source.total_duration()
    }
    fn try_seek(&mut self, pos: Duration) -> Result<(), SeekError> {
        self.source.try_seek(pos)?;
        self.channel = 0;
        self.until_refresh = 0;
        self.delays.fill([[0.0; 2]; FREQUENCIES.len()]);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rodio::buffer::SamplesBuffer;

    #[test]
    fn boosts_the_selected_band_relative_to_others_without_channel_bleed() {
        fn rms_at(hz: f32, settings: Settings) -> f32 {
            let samples = (0..4800)
                .flat_map(|frame| {
                    [
                        0.2 * (std::f32::consts::TAU * hz * frame as f32 / 48000.0).sin(),
                        0.0,
                    ]
                })
                .collect::<Vec<_>>();
            let control = Control::default();
            control.set(settings);
            let output = Equalized::new(SamplesBuffer::new(2, 48000, samples), control).skip(2400);
            let mut energy = 0.0;
            for (index, sample) in output.enumerate() {
                if index % 2 == 1 {
                    assert_eq!(sample, 0.0);
                } else {
                    energy += sample * sample;
                }
            }
            (energy / 3600.0).sqrt()
        }
        let boost = Settings {
            enabled: true,
            gains: [0, 0, 10, 0, 0],
        };
        assert!(rms_at(1000.0, boost) > 2.5 * rms_at(100.0, boost));
        let bypass = Settings {
            enabled: false,
            ..boost
        };
        assert!((rms_at(1000.0, bypass) - rms_at(100.0, bypass)).abs() < 0.001);
    }
}

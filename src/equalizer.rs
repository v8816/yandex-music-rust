use rodio::{ChannelCount, SampleRate, Source, source::SeekError};
use serde::{Deserialize, Serialize};
use std::{
    sync::{
        Arc,
        atomic::{AtomicU32, AtomicU64, Ordering},
    },
    time::Duration,
};

pub const FREQUENCIES: [f32; 5] = [60.0, 250.0, 1000.0, 4000.0, 12000.0];
pub const PRESETS: [(&str, [i8; FREQUENCIES.len()]); 7] = [
    ("Ровный", [0, 0, 0, 0, 0]),
    ("Больше баса", [5, 2, 0, 0, 0]),
    ("Вокал", [-2, -2, 2, 4, 0]),
    ("Рок", [3, 1, -1, 2, 3]),
    ("Электроника", [4, 1, -2, 1, 4]),
    ("Джаз", [2, 0, 1, 2, 2]),
    ("Больше высоких", [0, 0, 0, 2, 5]),
];

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Settings {
    pub enabled: bool,
    pub gains: [i8; FREQUENCIES.len()],
}

#[derive(Clone)]
pub struct Control {
    settings: Arc<AtomicU64>,
    volume: Arc<AtomicU32>,
}

impl Default for Control {
    fn default() -> Self {
        Self {
            settings: Arc::new(AtomicU64::new(0)),
            volume: Arc::new(AtomicU32::new(1.0_f32.to_bits())),
        }
    }
}

impl Control {
    pub fn set(&self, settings: Settings) {
        let [bass, low_mid, mid, high_mid, treble] = settings.gains.map(|gain| gain as u8);
        self.settings.store(
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

    pub fn set_volume(&self, volume: f32) {
        self.volume.store(volume.to_bits(), Ordering::Relaxed);
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

    fn power_response(self, cos: f64, sin: f64) -> f64 {
        // Evaluate the biquad at a point on the unit circle.
        let power = |a: f32, b: f32, c: f32| {
            let (a, b, c) = (f64::from(a), f64::from(b), f64::from(c));
            ((a + c) * cos + b).powi(2) + ((a - c) * sin).powi(2)
        };
        power(self.b0, self.b1, self.b2) / power(1.0, self.a1, self.a2)
    }
}

fn headroom(filters: &[Option<Filter>], rate: SampleRate) -> f32 {
    let nyquist = f64::from(rate) * 0.5;
    let mut peak_power = 1.0_f64;
    let frequencies = FREQUENCIES
        .into_iter()
        .map(f64::from)
        .chain((0..=256).map(|step| 10.0 * (nyquist / 10.0).powf(f64::from(step) / 256.0)));
    for hz in frequencies.filter(|hz| *hz <= nyquist) {
        let (sin, cos) = (std::f64::consts::PI * hz / nyquist).sin_cos();
        let power = filters
            .iter()
            .flatten()
            .map(|filter| filter.power_response(cos, sin))
            .product::<f64>();
        peak_power = peak_power.max(power);
    }
    peak_power.sqrt().recip() as f32
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
    volume: f32,
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
            volume: 1.0,
            active: false,
        }
    }

    fn refresh(&mut self) {
        self.volume = f32::from_bits(self.control.volume.load(Ordering::Relaxed));
        let settings = self.control.settings.load(Ordering::Relaxed);
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
        for (index, &hz) in FREQUENCIES.iter().enumerate() {
            let gain = bytes[index] as i8;
            let filter = if enabled && gain != 0 && hz < rate as f32 * 0.48 {
                Some(Filter::peaking(hz, gain, rate))
            } else {
                None
            };
            self.filters[index] = filter;
        }
        self.delays.fill([[0.0; 2]; FREQUENCIES.len()]);
        self.active = self.filters.iter().any(Option::is_some);
        self.headroom = if enabled
            && bytes[..FREQUENCIES.len()]
                .iter()
                .any(|&gain| gain as i8 > 0)
        {
            headroom(&self.filters, rate)
        } else {
            1.0
        };
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
            return Some(sample * self.volume);
        }
        let mut out = sample;
        for (filter, delay) in self.filters.iter().zip(&mut self.delays[channel]) {
            if let Some(filter) = filter {
                out = filter.apply(out, delay);
            }
        }
        Some((out * self.headroom * self.volume).clamp(-1.0, 1.0))
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
    fn changing_volume_preserves_the_equalized_waveform_with_overlapping_boosts() {
        let rate = 48_000;
        let input = (0..rate / 2)
            .map(|frame| (std::f32::consts::TAU * 250.0 * frame as f32 / rate as f32).sin())
            .collect::<Vec<_>>();
        let render = |volume| {
            let control = Control::default();
            control.set(Settings {
                enabled: true,
                gains: [12; FREQUENCIES.len()],
            });
            control.set_volume(volume);
            Equalized::new(SamplesBuffer::new(1, rate, input.clone()), control)
                .skip(rate as usize / 4)
                .collect::<Vec<_>>()
        };
        let full = render(1.0);
        let half = render(0.5);
        let peak = full.iter().map(|sample| sample.abs()).fold(0.0, f32::max);
        assert!((0.7..1.0).contains(&peak), "peak: {peak}");
        assert!(
            full.iter()
                .zip(half)
                .all(|(full, half)| (full * 0.5 - half).abs() < 1e-5)
        );
    }

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

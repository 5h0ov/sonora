use std::num::NonZero;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex, PoisonError, Weak};
use std::time::Duration;

use anyhow::{Context as _, Result};
use cpal::traits::{DeviceTrait, HostTrait};
use rodio::source::SeekError;
use rodio::{DeviceSinkBuilder, MixerDeviceSink, Source};

use crate::equalizer::{Equalized, Equalizer};
use crate::spectrum::{Spectrum, Tap};

pub const RAMP: Duration = Duration::from_millis(25);
const BUFFER: Duration = Duration::from_millis(50);

/// The device stream every output in the process mixes into, so the sound server sees Sonora as
/// one client however many engines are running. It closes when the last output on it drops.
static SHARED: Mutex<Weak<Device>> = Mutex::new(Weak::new());

#[derive(Clone)]
pub struct Volume(Arc<AtomicU32>);

impl Volume {
    pub fn new(gain: f32) -> Self {
        Self(Arc::new(AtomicU32::new(gain.to_bits())))
    }

    pub fn set(&self, gain: f32) {
        self.0.store(gain.to_bits(), Ordering::Relaxed);
    }

    fn get(&self) -> f32 {
        f32::from_bits(self.0.load(Ordering::Relaxed))
    }
}

/// What sits between the queue and the device: the equalizer, then the volume ramp, with the
/// spectrum tap listening at the end. Every engine builds one and hands it to the output.
pub struct Chain {
    pub volume: Volume,
    pub equalizer: Equalizer,
    pub spectrum: Spectrum,
}

/// One open stream on an output device, with the mixer the engines' chains play into.
struct Device {
    id: String,
    failed: Arc<AtomicBool>,
    stream: MixerDeviceSink,
}

impl Device {
    /// The shared stream on the default device. It opens a new one when no output holds a
    /// healthy stream there, and a stream left on a device that is no longer the default stays
    /// with the outputs still using it until they reopen.
    fn shared() -> Result<Arc<Self>> {
        let device = cpal::default_host()
            .default_output_device()
            .context("no audio output device")?;
        let id = ident(&device);

        let mut shared = SHARED.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(open) = shared.upgrade()
            && open.id == id
            && !open.failed.load(Ordering::Acquire)
        {
            return Ok(open);
        }

        let open = Arc::new(Self::open(device, id)?);
        *shared = Arc::downgrade(&open);
        Ok(open)
    }

    fn open(device: cpal::Device, id: String) -> Result<Self> {
        let default = device
            .default_output_config()
            .map_err(|error| anyhow::anyhow!("cannot read the output config: {error}"))?;

        log::info!(
            "sink: using {} at {} Hz, {} channels, {}",
            id,
            default.sample_rate(),
            default.channels(),
            default.sample_format()
        );

        let format = default.sample_format();
        let frames = (BUFFER.as_secs_f64() * default.sample_rate() as f64).round() as u32;
        let failed = Arc::new(AtomicBool::new(false));
        let stream_failed = failed.clone();
        let builder = DeviceSinkBuilder::default()
            .with_device(device)
            .with_config(&default.config())
            .with_buffer_size(cpal::BufferSize::Fixed(frames))
            .with_sample_format(format)
            .with_error_callback(move |error| match error {
                cpal::StreamError::BufferUnderrun => log::debug!("sink: buffer underrun"),
                error => {
                    log::warn!("sink: audio output failed: {error}");
                    stream_failed.store(true, Ordering::Release);
                }
            });
        let mut stream = builder
            .open_stream()
            .map_err(|error| anyhow::anyhow!("cannot open the audio output: {error}"))?;
        stream.log_on_drop(false);

        Ok(Self { id, failed, stream })
    }
}

/// One engine's player on the shared device stream. Dropping it ends the player's source in the
/// mixer, and the stream closes once no output is left on it.
pub struct Output {
    sink: Arc<rodio::Player>,
    volume: Volume,
    device: Arc<Device>,
}

impl Output {
    /// Joins the stream on the default output device and runs every sample through the
    /// equalizer and then the volume ramp before it reaches the mixer.
    pub fn open(chain: Chain) -> Result<Self> {
        let Chain {
            volume,
            equalizer,
            spectrum,
        } = chain;
        let device = Device::shared()?;

        let applied = volume.get();
        let tap = spectrum.attach();
        let (sink, source) = rodio::Player::new();
        let equalized = Equalized::new(source, equalizer);
        device
            .stream
            .mixer()
            .add(SmoothGain::new(equalized, volume.clone(), applied, RAMP).with_tap(tap));

        Ok(Self {
            sink: Arc::new(sink),
            volume,
            device,
        })
    }

    pub fn sink(&self) -> &Arc<rodio::Player> {
        &self.sink
    }

    pub fn set_volume(&self, gain: f32) {
        self.volume.set(gain);
    }

    /// Whether the device stream reported an error. Every output on the stream sees it.
    pub fn failed(&self) -> bool {
        self.device.failed.load(Ordering::Acquire)
    }

    /// Whether the system's default device is no longer the one this output plays on.
    pub fn changed(&self) -> bool {
        cpal::default_host()
            .default_output_device()
            .map(|device| ident(&device))
            .is_some_and(|device| device != "unknown" && device != self.device.id)
    }
}

pub struct SmoothGain<I> {
    input: I,
    volume: Volume,
    tap: Option<Tap>,

    current: f32,
    target: f32,
    step: f32,

    ramp: Duration,
    frames_left: u32,
    ramp_frames: u32,

    channel: u16,
    channels: u16,
    rate: u32,
}

impl<I: Source> SmoothGain<I> {
    pub fn new(input: I, volume: Volume, initial: f32, ramp: Duration) -> Self {
        Self {
            input,
            volume,
            tap: None,
            current: initial,
            target: initial,
            step: 0.0,
            ramp,
            frames_left: 0,
            ramp_frames: 1,
            channel: 0,
            channels: 0,
            rate: 0,
        }
    }

    pub fn with_tap(mut self, tap: Tap) -> Self {
        self.tap = Some(tap);
        self
    }

    fn resync(&mut self) {
        let channels = self.input.channels().get();
        let rate = self.input.sample_rate().get();
        if channels == self.channels && rate == self.rate {
            return;
        }

        self.channels = channels;
        self.rate = rate;
        if let Some(tap) = &self.tap {
            tap.format(rate, channels);
        }
        self.ramp_frames = (self.ramp.as_secs_f64() * rate as f64).round().max(1.0) as u32;
        self.frames_left = self.frames_left.min(self.ramp_frames);
    }
}

impl<I: Source> Iterator for SmoothGain<I> {
    type Item = f32;

    fn next(&mut self) -> Option<Self::Item> {
        let sample = self.input.next()?;

        if self.channel == 0 {
            self.resync();
            let requested = self.volume.get().max(0.0);

            if requested.to_bits() != self.target.to_bits() {
                self.target = requested;
                self.frames_left = self.ramp_frames;
                self.step = (self.target - self.current) / self.ramp_frames as f32;
            }

            if self.frames_left > 0 {
                self.current += self.step;
                self.frames_left -= 1;

                if self.frames_left == 0 {
                    self.current = self.target;
                }
            }
        }

        let output = sample * self.current;
        if let Some(tap) = self.tap.as_mut() {
            tap.push(output);
        }

        self.channel += 1;
        if self.channel >= self.channels {
            self.channel = 0;
        }

        Some(output)
    }
}

impl<I: Source> Source for SmoothGain<I> {
    fn current_span_len(&self) -> Option<usize> {
        self.input.current_span_len()
    }

    fn channels(&self) -> NonZero<u16> {
        self.input.channels()
    }

    fn sample_rate(&self) -> NonZero<u32> {
        self.input.sample_rate()
    }

    fn total_duration(&self) -> Option<Duration> {
        self.input.total_duration()
    }

    fn try_seek(&mut self, position: Duration) -> Result<(), SeekError> {
        self.input.try_seek(position)
    }
}

pub struct Trimmed<I> {
    input: I,
    head: u64,
    body: Option<u64>,
    emitted: u64,
    primed: bool,
    lane: u64,
}

impl<I: Source> Trimmed<I> {
    pub fn new(input: I, skip: Duration, take: Option<Duration>) -> Self {
        let lane = (input.sample_rate().get() as u64) * (input.channels().get() as u64);
        let samples = |span: Duration| (span.as_secs_f64() * lane as f64).round() as u64;

        Self {
            head: samples(skip),
            body: take.map(samples),
            emitted: 0,
            primed: false,
            lane,
            input,
        }
    }

    fn offset(&self) -> Duration {
        Duration::from_secs_f64(self.head as f64 / self.lane as f64)
    }
}

impl<I: Source> Iterator for Trimmed<I> {
    type Item = f32;

    fn next(&mut self) -> Option<Self::Item> {
        if !self.primed {
            self.primed = true;
            for _ in 0..self.head {
                self.input.next()?;
            }
        }
        if self.body.is_some_and(|body| self.emitted >= body) {
            return None;
        }

        let sample = self.input.next()?;
        self.emitted += 1;
        Some(sample)
    }
}

impl<I: Source> Source for Trimmed<I> {
    fn current_span_len(&self) -> Option<usize> {
        self.input.current_span_len()
    }

    fn channels(&self) -> NonZero<u16> {
        self.input.channels()
    }

    fn sample_rate(&self) -> NonZero<u32> {
        self.input.sample_rate()
    }

    fn total_duration(&self) -> Option<Duration> {
        match self.body {
            Some(body) => Some(Duration::from_secs_f64(body as f64 / self.lane as f64)),
            None => self
                .input
                .total_duration()
                .map(|whole| whole.saturating_sub(self.offset())),
        }
    }

    fn try_seek(&mut self, position: Duration) -> Result<(), SeekError> {
        self.input.try_seek(position + self.offset())?;
        self.primed = true;
        self.emitted = (position.as_secs_f64() * self.lane as f64).round() as u64;
        Ok(())
    }
}

fn ident(device: &cpal::Device) -> String {
    device
        .id()
        .map(|id| id.to_string())
        .unwrap_or_else(|_| "unknown".to_owned())
}

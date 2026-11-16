//! The real backend, on cpal: CoreAudio on macOS, WASAPI on Windows. It only lists and opens
//! output devices.

use super::backend::{AudioSlot, OutputBackend};
use super::{OutputInfo, classify, lock};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{
    BufferSize, SampleFormat, StreamConfig, SupportedBufferSize, SupportedStreamConfigRange,
};
use std::collections::HashMap;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::{Arc, Mutex};

#[cfg(target_os = "macos")]
use super::macos::transport;

/// The rate asked for wherever a device lists it.
const PREFERRED_RATE: u32 = 48_000;
/// Frames per callback asked for wherever a device allows it.
const PREFERRED_BUFFER: u32 = 256;

/// The system's audio host (CoreAudio on macOS, WASAPI on Windows). Creating it touches no
/// device.
pub fn cpal_backend() -> Box<dyn OutputBackend> {
    #[cfg(target_os = "macos")]
    super::macos::notify_on_own_thread();
    Box::new(CpalBackend {
        host: cpal::default_host(),
        stream: None,
        last: Mutex::default(),
        stuck: Mutex::default(),
    })
}

struct CpalBackend {
    host: cpal::Host,
    stream: Option<cpal::Stream>,
    /// The last list `outputs` made, kept for a look whose enumeration panics.
    last: Mutex<Vec<OutputInfo>>,
    /// Devices that list 48 kHz but would not switch to it, with the rate they run at instead.
    /// Forgotten when the device goes away, so it is asked for 48 kHz again once it is back.
    stuck: Mutex<HashMap<String, u32>>,
}

// cpal's streams are `Send` (cpal asserts it for every host), so the backend lives behind
// `AudioOutput`'s mutex and no stream has to stay on one thread.
const _: fn() = || {
    fn send<T: Send>() {}
    send::<CpalBackend>();
};

// cpal can panic where it does not expect the system to change under it (its Windows
// enumeration while devices come and go): every method keeps such a panic inside.
impl OutputBackend for CpalBackend {
    fn outputs(&self) -> Vec<OutputInfo> {
        match catch_unwind(AssertUnwindSafe(|| self.list())) {
            Ok(list) => {
                lock(&self.last).clone_from(&list);
                list
            }
            // Keep the last list for this look rather than report every device gone.
            Err(_) => lock(&self.last).clone(),
        }
    }

    fn open(&mut self, info: &OutputInfo, slot: Arc<AudioSlot>) -> Result<u32, String> {
        self.close();
        catch_unwind(AssertUnwindSafe(|| self.start(info, &slot)))
            .unwrap_or_else(|_| Err("the audio system failed while opening the device".into()))
    }

    fn close(&mut self) {
        // Dropping the stream stops it; its callback does not run again.
        let stream = self.stream.take();
        let _ = catch_unwind(AssertUnwindSafe(move || drop(stream)));
    }
}

impl CpalBackend {
    fn list(&self) -> Vec<OutputInfo> {
        // Only for its id. A stream opened on this handle would follow the system default by
        // itself (the macOS DefaultOutput unit, the WASAPI virtual endpoint), around the guard.
        let default = self.host.default_output_device().and_then(|d| d.id().ok());
        let Ok(devices) = self.host.output_devices() else {
            return Vec::new();
        };
        let mut stuck = lock(&self.stuck);
        let list: Vec<OutputInfo> = devices
            .filter_map(|device| {
                let id = device.id().ok()?;
                let name = device.description().ok()?.name().to_owned();
                // A device it cannot pick a config for is one it could not open.
                let config = stream_config(&device, stuck.get(id.id()).copied()).ok()?;
                Some(OutputInfo {
                    kind: classify(&name, transport(id.id())),
                    is_default: default.as_ref() == Some(&id),
                    id: id.id().to_owned(),
                    name,
                    sample_rate: config.sample_rate,
                })
            })
            .collect();
        stuck.retain(|id, _| list.iter().any(|output| output.id == *id));
        list
    }

    fn start(&mut self, info: &OutputInfo, slot: &Arc<AudioSlot>) -> Result<u32, String> {
        let device = self
            .host
            .output_devices()
            .map_err(|e| e.to_string())?
            .find(|d| d.id().is_ok_and(|id| id.id() == info.id))
            .ok_or("the device is gone")?;
        let stuck = lock(&self.stuck).get(&info.id).copied();
        let config = stream_config(&device, stuck)?;
        let (stream, rate) = match play(&device, config, slot) {
            Ok(stream) => (stream, config.sample_rate),
            // It lists 48 kHz but would not switch (an interface locked to an external clock,
            // say; cpal waits up to a second for that). Run it at its own rate, and go straight
            // there from now on.
            Err(error) if stuck.is_none() && config.sample_rate == PREFERRED_RATE => {
                let own = config_in(&float_ranges(&device)?, own_rate(&device)?)?;
                if own.sample_rate == PREFERRED_RATE {
                    return Err(error);
                }
                let stream = play(&device, own, slot)?;
                lock(&self.stuck).insert(info.id.clone(), own.sample_rate);
                (stream, own.sample_rate)
            }
            Err(error) => return Err(error),
        };
        self.stream = Some(stream);
        Ok(rate)
    }
}

/// The config `open` tries for `device`: 48 kHz if the device lists it (and did not refuse it
/// before: `stuck` is the rate it runs at instead), otherwise its own rate.
fn stream_config(device: &cpal::Device, stuck: Option<u32>) -> Result<StreamConfig, String> {
    let ranges = float_ranges(device)?;
    let rate = match stuck {
        Some(rate) => rate,
        None if ranges.iter().any(|r| r.contains_rate(PREFERRED_RATE)) => PREFERRED_RATE,
        None => own_rate(device)?,
    };
    config_in(&ranges, rate)
}

/// The device's 32-bit float configurations.
fn float_ranges(device: &cpal::Device) -> Result<Vec<SupportedStreamConfigRange>, String> {
    Ok(device
        .supported_output_configs()
        .map_err(|e| e.to_string())?
        .filter(|range| range.sample_format() == SampleFormat::F32)
        .collect())
}

/// The rate the device runs at now (its default config).
fn own_rate(device: &cpal::Device) -> Result<u32, String> {
    Ok(device
        .default_output_config()
        .map_err(|e| e.to_string())?
        .sample_rate())
}

/// 32-bit float at `rate`, with 256-frame callbacks where the device allows them.
fn config_in(ranges: &[SupportedStreamConfigRange], rate: u32) -> Result<StreamConfig, String> {
    let range = ranges
        .iter()
        .find(|r| r.contains_rate(rate))
        .ok_or_else(|| format!("the device has no 32-bit float output at {rate} Hz"))?;
    let buffer_size = match *range.buffer_size() {
        SupportedBufferSize::Range { min, max } if (min..=max).contains(&PREFERRED_BUFFER) => {
            BufferSize::Fixed(PREFERRED_BUFFER)
        }
        _ => BufferSize::Default,
    };
    Ok(StreamConfig {
        channels: range.channels(),
        sample_rate: rate,
        buffer_size,
    })
}

/// Builds and starts a stream, with the device's own buffer size if the fixed one fails.
fn play(
    device: &cpal::Device,
    config: StreamConfig,
    slot: &Arc<AudioSlot>,
) -> Result<cpal::Stream, String> {
    let stream = match build(device, config, slot) {
        Ok(stream) => stream,
        Err(_) if config.buffer_size != BufferSize::Default => {
            let config = StreamConfig {
                buffer_size: BufferSize::Default,
                ..config
            };
            build(device, config, slot)?
        }
        Err(error) => return Err(error),
    };
    stream.play().map_err(|e| e.to_string())?;
    Ok(stream)
}

/// A stream (not started yet) that pulls audio from `slot` and reports its latency and its
/// failures there.
fn build(
    device: &cpal::Device,
    config: StreamConfig,
    slot: &Arc<AudioSlot>,
) -> Result<cpal::Stream, String> {
    let channels = usize::from(config.channels);
    let rate = config.sample_rate;
    let audio = Arc::clone(slot);
    let errors = Arc::clone(slot);
    device
        .build_output_stream(
            config,
            // The audio thread: only arithmetic, an atomic store and the slot's try_lock.
            move |out: &mut [f32], info: &cpal::OutputCallbackInfo| {
                audio.set_output_latency_frames(latency_frames(info.timestamp(), rate));
                audio.process(out, channels);
            },
            // An xrun comes from the audio thread and is only a glitch. Anything else (device
            // gone, rate changed) means the stream is dead: look at the devices now.
            move |error: cpal::Error| {
                if error.kind() != cpal::ErrorKind::Xrun {
                    errors.stream_failed();
                }
            },
            None,
        )
        .map_err(|e| e.to_string())
}

/// Frames from the callback until its first frame is heard (cpal's timestamps include the
/// device's buffer, latency and safety offset). Real-time safe.
fn latency_frames(at: cpal::OutputStreamTimestamp, rate: u32) -> u32 {
    let latency = at.playback.duration_since(at.callback);
    (latency.as_secs_f64() * f64::from(rate)).round() as u32
}

/// Only macOS reports how an output is connected.
#[cfg(not(target_os = "macos"))]
fn transport(_id: &str) -> Option<super::Transport> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use cpal::{OutputStreamTimestamp, StreamInstant};

    #[test]
    fn latency_is_the_time_to_playback_in_frames() {
        let at = |playback_ns| OutputStreamTimestamp {
            callback: StreamInstant::new(7, 0),
            playback: StreamInstant::new(7, playback_ns),
        };
        assert_eq!(latency_frames(at(10_000_000), 48_000), 480);
        assert_eq!(latency_frames(at(5_333_333), 48_000), 256);
        assert_eq!(latency_frames(at(0), 44_100), 0);
        // A playback time before the callback (never expected) reads as no latency.
        let backwards = OutputStreamTimestamp {
            callback: StreamInstant::new(7, 1_000),
            playback: StreamInstant::new(7, 0),
        };
        assert_eq!(latency_frames(backwards, 48_000), 0);
    }
}

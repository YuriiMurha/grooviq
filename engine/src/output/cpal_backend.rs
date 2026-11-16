//! The real backend, on cpal: CoreAudio on macOS, WASAPI on Windows. It only lists and opens
//! output devices.

use super::backend::{AudioSlot, OutputBackend};
use super::{OutputInfo, classify};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{BufferSize, SampleFormat, StreamConfig, SupportedBufferSize};
use std::sync::Arc;

#[cfg(target_os = "macos")]
use super::macos::transport;

/// The rate asked for wherever a device supports it.
const PREFERRED_RATE: u32 = 48_000;
/// Frames per callback asked for wherever a device allows it.
const PREFERRED_BUFFER: u32 = 256;

/// The system's audio host (CoreAudio on macOS, WASAPI on Windows). Creating it touches no
/// device.
pub fn cpal_backend() -> Box<dyn OutputBackend> {
    Box::new(CpalBackend {
        host: cpal::default_host(),
        stream: None,
    })
}

struct CpalBackend {
    host: cpal::Host,
    stream: Option<cpal::Stream>,
}

// cpal's streams are `Send` (cpal asserts it for every host), so the backend lives behind
// `AudioOutput`'s mutex and no stream has to stay on one thread.
const _: fn() = || {
    fn send<T: Send>() {}
    send::<CpalBackend>();
};

impl OutputBackend for CpalBackend {
    fn outputs(&self) -> Vec<OutputInfo> {
        // Only for its id. A stream opened on this handle would follow the system default by
        // itself (the macOS DefaultOutput unit, the WASAPI virtual endpoint), around the guard.
        let default = self.host.default_output_device().and_then(|d| d.id().ok());
        let Ok(devices) = self.host.output_devices() else {
            return Vec::new();
        };
        devices
            .filter_map(|device| {
                let id = device.id().ok()?;
                let name = device.description().ok()?.name().to_owned();
                // A device it cannot pick a config for is one it could not open.
                let config = stream_config(&device).ok()?;
                Some(OutputInfo {
                    kind: classify(&name, transport(id.id())),
                    is_default: default.as_ref() == Some(&id),
                    id: id.id().to_owned(),
                    name,
                    sample_rate: config.sample_rate,
                })
            })
            .collect()
    }

    fn open(&mut self, info: &OutputInfo, slot: Arc<AudioSlot>) -> Result<(), String> {
        self.close();
        let device = self
            .host
            .output_devices()
            .map_err(|e| e.to_string())?
            .find(|d| d.id().is_ok_and(|id| id.id() == info.id))
            .ok_or("the device is gone")?;
        let config = stream_config(&device)?;
        // The engine was set to `info`'s rate; it must not run at another one.
        if config.sample_rate != info.sample_rate {
            return Err(format!(
                "the device now runs at {} Hz, not {} Hz",
                config.sample_rate, info.sample_rate
            ));
        }
        let stream = match build(&device, config, &slot) {
            Ok(stream) => stream,
            // The fixed buffer is a wish: fall back to the device's own.
            Err(_) if config.buffer_size != BufferSize::Default => {
                let config = StreamConfig {
                    buffer_size: BufferSize::Default,
                    ..config
                };
                build(&device, config, &slot)?
            }
            Err(e) => return Err(e),
        };
        stream.play().map_err(|e| e.to_string())?;
        self.stream = Some(stream);
        Ok(())
    }

    fn close(&mut self) {
        // Dropping the stream stops it; its callback does not run again.
        self.stream = None;
    }
}

/// The config `open` uses for `device`: 32-bit float at 48 kHz with 256-frame callbacks, or the
/// device's own rate and buffer size where it does not support those.
fn stream_config(device: &cpal::Device) -> Result<StreamConfig, String> {
    let float: Vec<_> = device
        .supported_output_configs()
        .map_err(|e| e.to_string())?
        .filter(|range| range.sample_format() == SampleFormat::F32)
        .collect();
    let supported = match float.iter().find(|r| r.contains_rate(PREFERRED_RATE)) {
        Some(range) => range.with_sample_rate(PREFERRED_RATE),
        None => {
            let default = device.default_output_config().map_err(|e| e.to_string())?;
            let rate = default.sample_rate();
            if default.sample_format() == SampleFormat::F32 {
                default
            } else {
                float
                    .iter()
                    .find(|r| r.contains_rate(rate))
                    .ok_or("the device has no 32-bit float output")?
                    .with_sample_rate(rate)
            }
        }
    };
    let buffer_size = match *supported.buffer_size() {
        SupportedBufferSize::Range { min, max } if (min..=max).contains(&PREFERRED_BUFFER) => {
            BufferSize::Fixed(PREFERRED_BUFFER)
        }
        _ => BufferSize::Default,
    };
    Ok(StreamConfig {
        channels: supported.channels(),
        sample_rate: supported.sample_rate(),
        buffer_size,
    })
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

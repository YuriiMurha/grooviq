use rtrb::{Consumer, Producer, RingBuffer};
use std::io;
use std::sync::atomic::{Ordering, fence};
use std::thread;
use std::time::{Duration, Instant};
use tracy_client::{Client, GpuContext, GpuContextType, frame_name, plot_name, span_location};

const CAPACITY: usize = 2048;
const CALLBACK_BEGIN: u16 = 0;
const CALLBACK_END: u16 = 1;
const BLOCK_BEGIN: u16 = 2;
const BLOCK_END: u16 = 3;

enum Sample {
    CallbackBegin(Instant),
    Block {
        begin: Instant,
        end: Instant,
        sample_rate: f32,
    },
    CallbackEnd(Instant),
}

pub(crate) struct Profile {
    samples: Producer<Sample>,
    recording: bool,
}

impl Profile {
    pub(crate) fn start(client: Client) -> io::Result<Self> {
        // Tracy's CPU zones can allocate on first use and on queue growth. Its GPU timing API
        // accepts explicit timestamps, preserving callback timing while keeping Tracy off the
        // audio thread. Anchor that clock before the processor can render its first block.
        let epoch = Instant::now();
        let timeline = client
            .clone()
            .new_gpu_context(
                Some("Grooviq audio callback (CPU)"),
                // OpenGL is Tracy's single-thread context type. Its reader rejects Invalid.
                GpuContextType::OpenGL,
                0,
                1.0,
            )
            .map_err(io::Error::other)?;
        let (samples, drain) = RingBuffer::new(CAPACITY);
        thread::Builder::new()
            .name("grooviq-tracy".into())
            .spawn(move || report(client, timeline, epoch, drain))?;
        Ok(Self {
            samples,
            recording: false,
        })
    }

    pub(crate) fn begin_callback(&mut self, blocks: usize) {
        // Reserve the entire callback. Only this producer can consume slots, so the following
        // pushes cannot fail, even if the reporter stalls. Drop whole callbacks under pressure.
        self.recording = self.samples.slots() >= blocks + 2;
        if self.recording {
            let _ = self.samples.push(Sample::CallbackBegin(Instant::now()));
        }
    }

    pub(crate) fn recording(&self) -> bool {
        self.recording
    }

    pub(crate) fn block(&mut self, begin: Instant, sample_rate: f32) {
        let _ = self.samples.push(Sample::Block {
            begin,
            end: Instant::now(),
            sample_rate,
        });
    }

    pub(crate) fn end_callback(&mut self) {
        if self.recording {
            let _ = self.samples.push(Sample::CallbackEnd(Instant::now()));
            self.recording = false;
        }
    }
}

fn report(client: Client, timeline: GpuContext, epoch: Instant, mut samples: Consumer<Sample>) {
    let nanos = |time: Instant| time.saturating_duration_since(epoch).as_nanos() as i64;
    let mut connected = false;
    loop {
        match samples.pop() {
            Ok(Sample::CallbackBegin(begin)) => {
                connected = Client::is_connected();
                if connected {
                    client.secondary_frame_mark(frame_name!("Grooviq audio callback"));
                    timeline.begin_span(span_location!("AudioProcessor::process"), CALLBACK_BEGIN);
                    timeline.upload_gpu_timestamp(CALLBACK_BEGIN, nanos(begin));
                }
            }
            Ok(Sample::Block {
                begin,
                end,
                sample_rate,
            }) if connected => {
                timeline.begin_span(span_location!("Engine block (64 frames)"), BLOCK_BEGIN);
                timeline.end_span(BLOCK_END);
                timeline.upload_gpu_timestamp(BLOCK_BEGIN, nanos(begin));
                timeline.upload_gpu_timestamp(BLOCK_END, nanos(end));
                client.plot(
                    plot_name!("Engine block time (% of deadline)"),
                    (end - begin).as_secs_f64() * f64::from(sample_rate) * 100.0
                        / crate::BLOCK as f64,
                );
            }
            Ok(Sample::CallbackEnd(end)) if connected => {
                timeline.end_span(CALLBACK_END);
                timeline.upload_gpu_timestamp(CALLBACK_END, nanos(end));
            }
            Ok(_) => {}
            Err(_) => {
                if samples.is_abandoned() {
                    fence(Ordering::Acquire);
                    if samples.is_empty() {
                        break;
                    }
                }
                thread::sleep(Duration::from_millis(1));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{BLOCK, EngineConfig, engine};

    #[test]
    fn a_full_queue_drops_whole_callbacks_and_can_resume() {
        let (samples, mut drain) = RingBuffer::new(3);
        let (_, mut processor) = engine(EngineConfig {
            sample_rate: 48_000.0,
        });
        processor.profile = Some(Profile {
            samples,
            recording: false,
        });

        // Two blocks cannot fit. The processor still renders them and consumes one extra
        // frame, leaving the rest of that block for future calls.
        processor.process(&mut [0.0; BLOCK + 1], 1);
        assert!(drain.is_empty());

        processor.process(&mut [0.0; BLOCK], 1);
        // Another callback while full must not leave unmatched start or end markers.
        processor.process(&mut [], 0);

        let Sample::CallbackBegin(callback_start) = drain.pop().unwrap() else {
            panic!("missing callback start");
        };
        let Sample::Block {
            begin,
            end,
            sample_rate,
        } = drain.pop().unwrap()
        else {
            panic!("missing block");
        };
        let Sample::CallbackEnd(callback_end) = drain.pop().unwrap() else {
            panic!("missing callback end");
        };
        assert!(callback_start <= begin && begin <= end && end <= callback_end);
        assert_eq!(sample_rate, 48_000.0);

        // A callback served entirely from the leftover block still has its own zone.
        processor.process(&mut [0.0; BLOCK - 1], 1);
        assert!(matches!(drain.pop(), Ok(Sample::CallbackBegin(_))));
        assert!(matches!(drain.pop(), Ok(Sample::CallbackEnd(_))));
        assert!(drain.is_empty());
    }
}

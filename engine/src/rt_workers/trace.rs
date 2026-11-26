use rtrb::{Consumer, Producer, RingBuffer};
use std::io;
use std::sync::atomic::{Ordering, fence};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};
use tracy_client::{Client, GpuContext, GpuContextType, span_location};

type Span = (Instant, Instant);

pub(super) struct WorkerTrace {
    samples: Producer<Span>,
    reporter: JoinHandle<()>,
}

impl WorkerTrace {
    pub(super) fn start(client: Client, name: &str) -> io::Result<Self> {
        let epoch = Instant::now();
        let timeline = client
            .new_gpu_context(Some(name), GpuContextType::OpenGL, 0, 1.0)
            .map_err(io::Error::other)?;
        let (samples, drain) = RingBuffer::new(2048);
        let reporter = thread::Builder::new()
            .name(format!("{name}-tracy"))
            .spawn(move || report(timeline, epoch, drain))?;
        Ok(Self { samples, reporter })
    }

    pub(super) fn job(&mut self, run: impl FnOnce()) {
        // Like the callback profiler, only send bounded timestamps from the real-time
        // path. Tracy's CPU zone queues can allocate even after the first zone is warmed.
        let begin = Instant::now();
        run();
        let _ = self.samples.push((begin, Instant::now()));
    }

    pub(super) fn finish(self) {
        drop(self.samples);
        let _ = self.reporter.join();
    }
}

fn report(timeline: GpuContext, epoch: Instant, mut drain: Consumer<Span>) {
    let nanos = |time: Instant| time.saturating_duration_since(epoch).as_nanos() as i64;
    loop {
        match drain.pop() {
            Ok((begin, end)) if Client::is_connected() => {
                timeline.begin_span(span_location!("Audio worker job"), 0);
                timeline.end_span(1);
                timeline.upload_gpu_timestamp(0, nanos(begin));
                timeline.upload_gpu_timestamp(1, nanos(end));
            }
            Ok(_) => {}
            Err(_) => {
                if drain.is_abandoned() {
                    fence(Ordering::Acquire);
                    if drain.is_empty() {
                        break;
                    }
                }
                thread::sleep(Duration::from_millis(5));
            }
        }
    }
}

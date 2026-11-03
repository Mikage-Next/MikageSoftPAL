//! Sound for hosts that pull it from a real-time audio callback.
//!
//! The engine mixes with Kira into a sink whose renderer sits behind a lock.
//! A real-time callback must not take it, so a feeder thread renders ahead
//! into a lock-free ring and the callback only copies out of it. While the
//! host's audio is stopped the ring stays full and the feeder waits.
use pal_vm::audio::host_backend::AudioSink;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

pub const SAMPLE_RATE: u32 = 48_000;
const CHANNELS: usize = 2;
/// Frames rendered per pass of the feeder.
const CHUNK: usize = 256;
/// Frames held ahead of the callback: about 85 ms at 48 kHz.
const AHEAD: usize = 4096;

pub struct AudioOutput {
    ring: rtrb::Consumer<f32>,
    stop: Arc<AtomicBool>,
    feeder: Option<JoinHandle<()>>,
}

impl AudioOutput {
    pub fn new(sink: AudioSink) -> std::io::Result<Self> {
        let (mut producer, ring) = rtrb::RingBuffer::<f32>::new(CHANNELS * AHEAD);
        let stop = Arc::new(AtomicBool::new(false));
        let stopping = stop.clone();
        let feeder = std::thread::Builder::new()
            .name("sena-audio".into())
            .spawn(move || {
                let mut buf = vec![0.0f32; CHANNELS * CHUNK];
                while !stopping.load(Ordering::Relaxed) {
                    if producer.slots() < buf.len() {
                        std::thread::sleep(Duration::from_millis(3));
                        continue;
                    }
                    sink.render_f32(&mut buf);
                    if let Ok(chunk) = producer.write_chunk_uninit(buf.len()) {
                        chunk.fill_from_iter(buf.iter().copied());
                    }
                }
            })?;
        Ok(Self {
            ring,
            stop,
            feeder: Some(feeder),
        })
    }

    /// Fills `out` (interleaved stereo) from the ring; what the feeder has not
    /// rendered yet is silence. Never blocks or allocates.
    pub fn render(&mut self, out: &mut [f32]) {
        let n = self.ring.slots().min(out.len()) & !1;
        let mut done = 0;
        if let Ok(chunk) = self.ring.read_chunk(n) {
            let (a, b) = chunk.as_slices();
            out[..a.len()].copy_from_slice(a);
            out[a.len()..a.len() + b.len()].copy_from_slice(b);
            done = a.len() + b.len();
            chunk.commit_all();
        }
        out[done..].fill(0.0);
    }
}

impl Drop for AudioOutput {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(feeder) = self.feeder.take() {
            let _ = feeder.join();
        }
    }
}

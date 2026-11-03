//! Native PCM pull backend, following siglus_rs's console mixer boundary.
//! Each engine owns its sink; an audio thread never borrows the VM or FFI host.
use kira::backend::{Backend, Renderer};
use std::sync::{Arc, Mutex};

#[derive(Clone, Default)]
pub struct AudioSink(Arc<Mutex<Option<Renderer>>>);

impl AudioSink {
    pub fn render_i16(&self, output: &mut [i16]) {
        output.fill(0);
        let Ok(mut slot) = self.0.lock() else {
            return;
        };
        let Some(renderer) = slot.as_mut() else {
            return;
        };
        renderer.on_start_processing();
        let mut samples = [0.0f32; 1024];
        for chunk in output.chunks_mut(samples.len()) {
            let len = chunk.len() & !1;
            renderer.process(&mut samples[..len], 2);
            for (dest, sample) in chunk[..len].iter_mut().zip(&samples[..len]) {
                *dest = (sample.clamp(-1.0, 1.0) * i16::MAX as f32) as i16;
            }
        }
    }
}

impl AudioSink {
    /// Interleaved stereo float samples at 48 kHz (`output.len()` even).
    pub fn render_f32(&self, output: &mut [f32]) {
        output.fill(0.0);
        let Ok(mut slot) = self.0.lock() else {
            return;
        };
        let Some(renderer) = slot.as_mut() else {
            return;
        };
        renderer.on_start_processing();
        let len = output.len() & !1;
        renderer.process(&mut output[..len], 2);
    }
}

pub struct HostBackend(AudioSink);

impl Backend for HostBackend {
    type Settings = AudioSink;
    type Error = &'static str;
    fn setup(sink: Self::Settings, _: usize) -> Result<(Self, u32), Self::Error> {
        Ok((Self(sink), 48_000))
    }
    fn start(&mut self, renderer: Renderer) -> Result<(), Self::Error> {
        *self.0 .0.lock().map_err(|_| "audio sink mutex poisoned")? = Some(renderer);
        Ok(())
    }
}

impl Drop for HostBackend {
    fn drop(&mut self) {
        if let Ok(mut slot) = self.0 .0.lock() {
            *slot = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kira::sound::static_sound::{StaticSoundData, StaticSoundSettings};
    use kira::{AudioManager, AudioManagerSettings, Frame};
    #[test]
    fn sink_pulls_stereo_and_survives_manager_teardown() {
        let sink = AudioSink::default();
        let other = AudioSink::default();
        let mut manager = AudioManager::<HostBackend>::new(AudioManagerSettings {
            backend_settings: sink.clone(),
            ..Default::default()
        })
        .unwrap();
        let sound = StaticSoundData {
            sample_rate: 48_000,
            frames: vec![
                Frame {
                    left: 0.25,
                    right: -0.5
                };
                4096
            ]
            .into(),
            settings: StaticSoundSettings::default(),
            slice: None,
        };
        let _handle = manager.play(sound).unwrap();
        let mut out = [0i16; 1024];
        sink.render_i16(&mut out);
        assert!(out.chunks_exact(2).any(|s| s[0] > 1000 && s[1] < -1000));
        other.render_i16(&mut out);
        assert!(out.iter().all(|&s| s == 0));
        drop(manager);
        sink.render_i16(&mut out);
        assert!(out.iter().all(|&s| s == 0));
    }
}

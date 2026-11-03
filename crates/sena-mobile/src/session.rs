//! One running game: the PAL engine, its input and the wgpu presentation into
//! the host's view.
use std::path::PathBuf;
use std::time::Duration;

use pal_asset::Nls;
use pal_vm::config::TextLang;
use pal_vm::event::{InputEvent, MouseButton, PalEvent};
use pal_vm::renderer::WgpuBackend;
use pal_vm::runtime::RuntimeStatus;
use pal_vm::{Engine, EngineConfig};

use crate::log;

pub struct Config {
    pub game_dir: PathBuf,
    pub nls: Nls,
    pub lang: TextLang,
}

pub enum Status {
    Running,
    Ended,
    Failed(String),
}

struct Presenter {
    _instance: wgpu::Instance,
    backend: WgpuBackend,
    surface: [u32; 2],
}

pub struct Session {
    engine: Engine,
    presenter: Option<Presenter>,
    /// The game's screen in PAL logical pixels (SYSTEM.INI).
    logical: [u32; 2],
    /// Where that screen sits in the surface: x, y, width, height.
    viewport: [u32; 4],
    status: Status,
    /// The last frame, for snapshots.
    last_scene: Option<pal_vm::scene::FrameScene>,
    snapshot: Vec<u8>,
}

impl Session {
    pub fn open(config: Config) -> anyhow::Result<Self> {
        log::emit(
            log::INFO,
            &format!(
                "open {} nls={} lang={:?}",
                config.game_dir.display(),
                config.nls.name(),
                config.lang
            ),
        );
        let mut engine = Engine::new(EngineConfig {
            game_root: Some(config.game_dir),
            nls: config.nls,
            lang: config.lang,
            ..Default::default()
        })?;
        let (width, height) = engine.logical_size_from_config(1280, 720);
        // Pointer positions are passed in logical pixels, so the engine's
        // window is the logical screen and its mapping is the identity.
        engine.handle_event(PalEvent::Resized { width, height });
        Ok(Self {
            engine,
            presenter: None,
            logical: [width.max(1), height.max(1)],
            viewport: [0, 0, width.max(1), height.max(1)],
            status: Status::Running,
            last_scene: None,
            snapshot: Vec::new(),
        })
    }

    pub fn screen_size(&self) -> [u32; 2] {
        self.logical
    }

    pub fn status(&self) -> &Status {
        &self.status
    }

    /// Presents into a surface made by `make` from a fresh instance, or
    /// resizes the current one.
    pub fn attach(
        &mut self,
        size: [u32; 2],
        make: impl FnOnce(&wgpu::Instance) -> anyhow::Result<wgpu::Surface<'static>>,
    ) -> anyhow::Result<()> {
        let size = [size[0].max(1), size[1].max(1)];
        if let Some(presenter) = self.presenter.as_mut() {
            presenter.backend.resize_to(size[0], size[1]);
            presenter.surface = size;
        } else {
            let instance = wgpu::Instance::new(
                wgpu::InstanceDescriptor::new_without_display_handle_from_env(),
            );
            let surface = make(&instance)?;
            let backend =
                pollster::block_on(WgpuBackend::with_surface(&instance, surface, size[0], size[1]))?;
            self.presenter = Some(Presenter {
                _instance: instance,
                backend,
                surface: size,
            });
        }
        self.fit();
        Ok(())
    }

    pub fn detach(&mut self) {
        self.presenter = None;
    }

    /// Letterboxes the logical screen into the surface.
    fn fit(&mut self) {
        let Some(presenter) = self.presenter.as_mut() else {
            return;
        };
        let [sw, sh] = presenter.surface;
        let [lw, lh] = self.logical;
        let scale = (sw as f64 / lw as f64).min(sh as f64 / lh as f64);
        let w = ((lw as f64 * scale).round() as u32).clamp(1, sw);
        let h = ((lh as f64 * scale).round() as u32).clamp(1, sh);
        self.viewport = [(sw - w) / 2, (sh - h) / 2, w, h];
        presenter.backend.set_viewport(Some(self.viewport));
    }

    /// Runs one frame of `dt_ms` and presents it.
    pub fn step(&mut self, dt_ms: u32) -> &Status {
        if !matches!(self.status, Status::Running) {
            return &self.status;
        }
        // The script's window requests (mode, size, position) have nothing to
        // act on in a phone view.
        let _ = self.engine.take_window_requests();
        self.engine.handle_event(PalEvent::RedrawRequested);
        let frame = match self
            .engine
            .update_with_delta(Duration::from_millis(u64::from(dt_ms.clamp(1, 250))))
        {
            Ok(frame) => frame,
            Err(err) => {
                self.engine.input_begin_frame();
                self.status = Status::Failed(format!("{err:#}"));
                return &self.status;
            }
        };
        let logical = [frame.scene.logical_width.max(1), frame.scene.logical_height.max(1)];
        if logical != self.logical {
            self.logical = logical;
            self.engine.handle_event(PalEvent::Resized {
                width: logical[0],
                height: logical[1],
            });
            self.fit();
        }
        if let Some(presenter) = self.presenter.as_mut() {
            if let Err(err) = presenter.backend.render(&frame.scene, wgpu::Color::BLACK) {
                log::emit(log::WARN, &format!("present failed: {err:#}"));
            }
        }
        self.engine.input_begin_frame();
        let runtime_status = frame.runtime_status;
        self.last_scene = Some(frame.scene);
        match &runtime_status {
            RuntimeStatus::Halted { .. } => self.status = Status::Ended,
            RuntimeStatus::UnsupportedCommand { .. }
            | RuntimeStatus::UnsupportedExtCall { .. }
            | RuntimeStatus::Faulted { .. } => {
                self.status = Status::Failed(runtime_status.to_string())
            }
            _ => {}
        }
        &self.status
    }

    /// Surface pixels to the game's logical pixels.
    fn to_logical(&self, x: f32, y: f32) -> (f64, f64) {
        let [vx, vy, vw, vh] = self.viewport.map(f64::from);
        let lx = ((f64::from(x) - vx) * f64::from(self.logical[0]) / vw)
            .clamp(0.0, f64::from(self.logical[0] - 1));
        let ly = ((f64::from(y) - vy) * f64::from(self.logical[1]) / vh)
            .clamp(0.0, f64::from(self.logical[1] - 1));
        (lx, ly)
    }

    pub fn pointer(&mut self, x: f32, y: f32, button: Option<(MouseButton, bool)>) {
        let (x, y) = self.to_logical(x, y);
        self.engine.set_host_touch_available(true);
        self.engine
            .handle_event(PalEvent::Input(InputEvent::CursorMoved { x, y }));
        if let Some((button, pressed)) = button {
            self.engine
                .handle_event(PalEvent::Input(InputEvent::MouseInput { button, pressed }));
        }
    }

    pub fn wheel(&mut self, notches: i32) {
        self.engine.handle_event(PalEvent::Input(InputEvent::MouseWheel {
            delta_x: 0.0,
            delta_y: notches as f32,
        }));
    }

    pub fn key(&mut self, vk: i32, down: bool) {
        if let Some(key_name) = key_name(vk) {
            self.engine.handle_event(PalEvent::Input(InputEvent::Keyboard {
                key_name,
                pressed: down,
            }));
        }
    }

    pub fn set_focused(&mut self, focused: bool) {
        self.engine.handle_event(PalEvent::Focused { focused });
    }

    /// The last frame drawn on the CPU at the game's logical size (RGBA).
    pub fn snapshot(&mut self) -> Option<([u32; 2], &[u8])> {
        let scene = self.last_scene.as_ref()?;
        self.snapshot = pal_vm::scene::rasterize_scene_rgba(scene);
        let size = [scene.logical_width.max(1), scene.logical_height.max(1)];
        Some((size, &self.snapshot))
    }

    pub fn audio_sink(&self) -> pal_vm::audio::host_backend::AudioSink {
        self.engine.audio().host_sink()
    }
}

/// Windows virtual-key codes to the key names the engine's input reads.
fn key_name(vk: i32) -> Option<String> {
    let name = match vk {
        0x0D => "Enter",
        0x1B => "Escape",
        0x20 => "Space",
        0x11 | 0xA2 | 0xA3 => "Control",
        0x10 | 0xA0 | 0xA1 => "Shift",
        0x12 => "Alt",
        0x09 => "Tab",
        0x25 => "ArrowLeft",
        0x26 => "ArrowUp",
        0x27 => "ArrowRight",
        0x28 => "ArrowDown",
        0x70..=0x7B => return Some(format!("F{}", vk - 0x6F)),
        0x30..=0x39 | 0x41..=0x5A => return Some(char::from(vk as u8).to_string()),
        _ => return None,
    };
    Some(name.to_owned())
}

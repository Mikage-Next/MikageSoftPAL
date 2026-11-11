use super::*;
use crate::fx::{RasterSource, RasterUniforms};
use crate::scene::{rasterize_scene_rgba, DrawCommand, FrameScene, SceneTexture, SceneTextureId};
use crate::sprite::{PalVec3, SpriteSystem};

#[test]
#[ignore = "requires a working graphics adapter; performs real GPU readback"]
fn raster_gpu_readback_matches_cpu_reference() {
    pollster::block_on(async {
        let instance =
            wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions::default())
            .await
            .unwrap();
        eprintln!("raster GPU adapter: {:?}", adapter.get_info());
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor::default())
            .await
            .unwrap();
        for program in [
            ShaderProgram::DrawPrimitiveUp2d,
            ShaderProgram::Bicubic,
            ShaderProgram::RadialBlur,
            ShaderProgram::Raster,
            ShaderProgram::RasterSw,
        ] {
            let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
            let _module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some(program.label()),
                source: wgpu::ShaderSource::Wgsl(shader_source(program).into()),
            });
            assert!(
                scope.pop().await.is_none(),
                "shader validation failed: {program:?}"
            );
        }
        let texture_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: None,
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let fx_layout = raster_bind_layout(&device);
        let rgba: Vec<_> = (0..64 * 64)
            .flat_map(|i| [(i % 64 * 3) as u8, (i / 64 * 3) as u8, 80, 255])
            .collect();
        let extent = wgpu::Extent3d {
            width: 64,
            height: 64,
            depth_or_array_layers: 1,
        };
        let source = device.create_texture(&wgpu::TextureDescriptor {
            label: None,
            size: extent,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        queue.write_texture(
            source.as_image_copy(),
            &rgba,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(256),
                rows_per_image: Some(64),
            },
            extent,
        );
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor::default());
        let source_view = source.create_view(&Default::default());
        let texture_bind =
            WgpuBackend::bind_texture(&device, &texture_layout, &source_view, &sampler);
        let output = device.create_texture(&wgpu::TextureDescriptor {
            label: None,
            size: extent,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let output_view = output.create_view(&Default::default());
        let readback = device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: 64 * 256,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let mut sprites = SpriteSystem::new();
        let handle = sprites
            .create_rgba_sprite(
                64,
                64,
                rgba.clone(),
                PalVec3 {
                    x: 8.0,
                    y: 8.0,
                    z: 0.0,
                },
                0,
                "test",
            )
            .unwrap();
        sprites.set_sprite_fx_effect_with_source(
            handle,
            0,
            1,
            0,
            RasterSource {
                period_ms: 1000,
                undulate_x: 0.46,
                undulate_y: 0.18,
            },
        );
        sprites.tick_fx_effects(0);
        sprites.tick_fx_effects(270);
        let DrawCommand::Sprite(mut draw) = sprites.commands()[0] else {
            panic!()
        };
        draw.dst.w = 48.0;
        draw.dst.h = 48.0;
        draw.color = [0.8, 0.9, 1.0, 0.7];
        for blend_disabled in [false, true] {
            for texture_rgb in [false, true] {
                for mode in 1..=11 {
                    draw.render_mode = mode;
                    draw.blend_state = crate::scene::blend::NativeDrawState {
                        blend_disabled,
                        texture_rgb,
                    };
                    let state = draw.effective_blend_state();
                    let pipeline = raster_pipeline(
                        &device,
                        wgpu::TextureFormat::Rgba8Unorm,
                        &texture_layout,
                        &fx_layout,
                        state.pipeline_index(mode) as u32,
                    );
                    for (phase, rotation) in [(0.0, 0.0), (0.27, 0.0), (0.27, 64.0), (0.9, 128.0)] {
                        draw.raster_fx = Some(RasterUniforms {
                            offset: phase,
                            undulate_x: 0.46,
                            undulate_y: 0.18,
                        });
                        draw.rotation[2] = rotation;
                        let quad = draw
                            .quad(false)
                            .map(|p| [p[0] / 32.0 - 1.0, 1.0 - p[1] / 32.0]);
                        let fx_bind = raster_bind_group(
                            &device,
                            &fx_layout,
                            RasterGpuUniforms::new(draw.raster_fx.unwrap(), quad),
                        );
                        let local = [[-1.0, 1.0], [1.0, 1.0], [1.0, -1.0], [-1.0, -1.0]];
                        let uvs = [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]];
                        let vertices: Vec<_> = [0, 1, 2, 0, 2, 3]
                            .map(|i| SpriteVertex {
                                position: local[i],
                                uv: uvs[i],
                                color: state.modulation(draw.color),
                            })
                            .to_vec();
                        let vertex_buffer =
                            device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                                label: None,
                                contents: bytemuck::cast_slice(&vertices),
                                usage: wgpu::BufferUsages::VERTEX,
                            });
                        let mut encoder = device.create_command_encoder(&Default::default());
                        {
                            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                                label: None,
                                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                                    view: &output_view,
                                    depth_slice: None,
                                    resolve_target: None,
                                    ops: wgpu::Operations {
                                        load: wgpu::LoadOp::Clear(wgpu::Color {
                                            r: 80.0 / 255.0,
                                            g: 40.0 / 255.0,
                                            b: 160.0 / 255.0,
                                            a: 1.0,
                                        }),
                                        store: wgpu::StoreOp::Store,
                                    },
                                })],
                                depth_stencil_attachment: None,
                                timestamp_writes: None,
                                occlusion_query_set: None,
                                multiview_mask: None,
                            });
                            pass.set_pipeline(&pipeline);
                            pass.set_bind_group(0, &texture_bind, &[]);
                            pass.set_bind_group(1, &fx_bind, &[]);
                            pass.set_vertex_buffer(0, vertex_buffer.slice(..));
                            pass.draw(0..6, 0..1);
                        }
                        encoder.copy_texture_to_buffer(
                            output.as_image_copy(),
                            wgpu::TexelCopyBufferInfo {
                                buffer: &readback,
                                layout: wgpu::TexelCopyBufferLayout {
                                    offset: 0,
                                    bytes_per_row: Some(256),
                                    rows_per_image: Some(64),
                                },
                            },
                            extent,
                        );
                        queue.submit([encoder.finish()]);
                        let (tx, rx) = std::sync::mpsc::channel();
                        readback
                            .slice(..)
                            .map_async(wgpu::MapMode::Read, move |result| {
                                tx.send(result).unwrap();
                            });
                        device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
                        rx.recv().unwrap().unwrap();
                        let actual = readback.slice(..).get_mapped_range().expect("mapped readback").to_vec();
                        readback.unmap();
                        let mut scene = FrameScene::boot().with_logical_size(64, 64);
                        scene.clear_color = [80.0 / 255.0, 40.0 / 255.0, 160.0 / 255.0, 1.0];
                        draw.texture_id = SceneTextureId(1);
                        scene.textures.push(SceneTexture::rgba8(
                            SceneTextureId(1),
                            1,
                            64,
                            64,
                            rgba.clone(),
                        ));
                        scene.commands.push(DrawCommand::Sprite(draw));
                        let expected = rasterize_scene_rgba(&scene);
                        let error: usize = actual
                            .iter()
                            .zip(&expected)
                            .enumerate()
                            .filter(|(index, _)| index % 4 != 3)
                            .map(|(_, (a, b))| a.abs_diff(*b) as usize)
                            .sum();
                        let mean = error as f64 / (actual.len() * 3 / 4) as f64;
                        eprintln!("mode={mode} blend_disabled={blend_disabled} texture_rgb={texture_rgb} phase={phase} rotation={rotation}: GPU/CPU mean RGB byte error={mean}");
                        assert!(mean < 0.7, "GPU and CPU diverged: {mean}");
                    }
                }
            }
        }
    });
}

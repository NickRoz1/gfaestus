use vulkano::buffer::{BufferUsage, CpuAccessibleBuffer, CpuBufferPool};
use vulkano::command_buffer::{AutoCommandBufferBuilder, DynamicState, SubpassContents};
use vulkano::descriptor::{descriptor_set::PersistentDescriptorSet, PipelineLayoutAbstract};
use vulkano::device::{Device, DeviceExtensions};
use vulkano::framebuffer::{Framebuffer, FramebufferAbstract, RenderPassAbstract, Subpass};
use vulkano::image::{ImageUsage, SwapchainImage};
use vulkano::instance::{Instance, PhysicalDevice};

use vulkano::pipeline::{viewport::Viewport, GraphicsPipeline};

use vulkano::swapchain::{
    self, AcquireError, ColorSpace, FullscreenExclusive, PresentMode, SurfaceTransform, Swapchain,
    SwapchainCreationError,
};
use vulkano::sync::{self, FlushError, GpuFuture};

use vulkano_win::VkSurfaceBuild;

use winit::event::{Event, WindowEvent};
use winit::event_loop::{ControlFlow, EventLoop};
use winit::window::{Window, WindowBuilder};

use std::sync::Arc;

use std::time::Instant;

use vk_gfa::geometry::*;
use vk_gfa::gfa::*;
use vk_gfa::view;

use vk_gfa::ui::{UICmd, UIState, UIThread};

use nalgebra_glm as glm;

// pub struct ViewOffset {
// }

fn main() {
    let required_extensions = vulkano_win::required_extensions();
    let instance = Instance::new(None, &required_extensions, None).unwrap();
    let physical = PhysicalDevice::enumerate(&instance).next().unwrap();

    let event_loop = EventLoop::new();
    let surface = WindowBuilder::new()
        .build_vk_surface(&event_loop, instance.clone())
        .unwrap();

    let queue_family = physical
        .queue_families()
        .find(|&q| q.supports_graphics() && surface.is_supported(q).unwrap_or(false))
        .unwrap();

    let device_ext = DeviceExtensions {
        khr_swapchain: true,
        ..DeviceExtensions::none()
    };
    let (device, mut queues) = Device::new(
        physical,
        physical.supported_features(),
        &device_ext,
        [(queue_family, 0.5)].iter().cloned(),
    )
    .unwrap();

    let queue = queues.next().unwrap();

    let (mut swapchain, images) = {
        let caps = surface.capabilities(physical).unwrap();
        let alpha = caps.supported_composite_alpha.iter().next().unwrap();
        let format = caps.supported_formats[0].0;
        let dimensions: [u32; 2] = surface.window().inner_size().into();

        Swapchain::new(
            device.clone(),
            surface.clone(),
            caps.min_image_count,
            format,
            dimensions,
            1,
            ImageUsage::color_attachment(),
            &queue,
            SurfaceTransform::Identity,
            alpha,
            PresentMode::Fifo,
            FullscreenExclusive::Default,
            true,
            ColorSpace::SrgbNonLinear,
        )
        .unwrap()
    };

    let vertex_buffer_pool: CpuBufferPool<Vertex> = CpuBufferPool::vertex_buffer(device.clone());
    let color_buffer_pool: CpuBufferPool<Color> = CpuBufferPool::vertex_buffer(device.clone());

    // fn _dumb() {
    let _ = include_str!("../shaders/point.vert");
    let _ = include_str!("../shaders/point.frag");
    let _ = include_str!("../shaders/fragment.frag");
    let _ = include_str!("../shaders/vertex.vert");
    let _ = include_str!("../shaders/geometry.geom");
    // }

    mod point_vert {
        vulkano_shaders::shader! {
            ty: "vertex",
            path: "shaders/point.vert",
        }
    }

    mod simple_vert {
        vulkano_shaders::shader! {
            ty: "vertex",
            path: "shaders/vertex.vert",
        }
    }

    mod simple_frag {
        vulkano_shaders::shader! {
            ty: "fragment",
            path: "shaders/fragment.frag",
        }
    }

    mod point_frag {
        vulkano_shaders::shader! {
            ty: "fragment",
            path: "shaders/point.frag",
        }
    }

    mod rect_geom {
        vulkano_shaders::shader! {
            ty: "geometry",
            path: "shaders/geometry.geom",
        }
    }

    // let point_vert = point_vert::Shader::load(device.clone()).unwrap();
    // let point_frag = point_frag::Shader::load(device.clone()).unwrap();
    let simple_vert = simple_vert::Shader::load(device.clone()).unwrap();
    let simple_frag = simple_frag::Shader::load(device.clone()).unwrap();
    let rect_geom = rect_geom::Shader::load(device.clone()).unwrap();

    let uniform_buffer =
        CpuBufferPool::<simple_vert::ty::View>::new(device.clone(), BufferUsage::uniform_buffer());

    let render_pass = Arc::new(
        vulkano::single_pass_renderpass!(
            device.clone(),
            attachments: {
                color: {
                    load: Clear,
                    store: Store,
                    format: swapchain.format(),
                    samples: 1,
                }
            },
            pass: {
                color: [color],
                depth_stencil: {}
            }
        )
        .unwrap(),
    );

    let pipeline = Arc::new(
        GraphicsPipeline::start()
            .vertex_input_single_buffer::<Vertex>()
            .vertex_shader(simple_vert.main_entry_point(), ())
            // .vertex_shader(point_vert.main_entry_point(), ())
            .triangle_list()
            // .triangle_strip()
            // .point_list()
            // .line_list()
            // .geometry_shader(rect_geom.main_entry_point(), ())
            .viewports_dynamic_scissors_irrelevant(1)
            // .fragment_shader(point_frag.main_entry_point(), ())
            .fragment_shader(simple_frag.main_entry_point(), ())
            .render_pass(Subpass::from(render_pass.clone(), 0).unwrap())
            .blend_alpha_blending()
            .build(device.clone())
            .unwrap(),
    );

    let mut dynamic_state = DynamicState {
        line_width: None,
        viewports: None,
        scissors: None,
        compare_mask: None,
        write_mask: None,
        reference: None,
    };

    // let mut view: (f32, f32) = (0.0, 0.0);

    let segments = Segment::from_path(
        Point {
            x: -200.0,
            y: -15.0,
        },
        &[10, 12, 15, 50, 30, 10, 30],
    );

    use vk_gfa::view::View;

    let mut view: View = View::default();

    let mut framebuffers = window_size_update(&images, render_pass.clone(), &mut dynamic_state);

    let mut width = 100.0;
    let mut height = 100.0;

    if let Some(viewport) = dynamic_state.viewports.as_ref().and_then(|v| v.get(0)) {
        view.width = viewport.dimensions[0];
        view.height = viewport.dimensions[1];

        width = viewport.dimensions[0];
        height = viewport.dimensions[1];
    }

    let (ui_thread, ui_cmd_tx) = UIThread::new(width, height);

    let mut recreate_swapchain = false;

    let mut previous_frame_end = Some(sync::now(device.clone()).boxed());

    let mut last_time = Instant::now();
    let mut t = 0.0;

    event_loop.run(move |event, _, control_flow| {
        let now = Instant::now();
        let delta = now.duration_since(last_time);

        if let Some(ui_state) = ui_thread.try_get_state() {
            view = ui_state.view;
            // println!("x: {}, y: {}", view.center.x, view.center.y);
        }

        t += delta.as_secs_f32();

        last_time = now;

        match event {
            Event::WindowEvent {
                event: WindowEvent::KeyboardInput { input, .. },
                ..
            } => {
                use winit::event::VirtualKeyCode as Key;

                let state = input.state;
                let keycode = input.virtual_keycode;

                let pressed = state == winit::event::ElementState::Pressed;

                let speed = 200.0;

                if let Some(key) = keycode {
                    match key {
                        Key::Up => {
                            if pressed {
                                let delta = Point { x: 0.0, y: speed };
                                ui_cmd_tx.send(UICmd::Pan { delta }).unwrap();
                            }
                        }
                        Key::Right => {
                            if pressed {
                                let delta = Point { x: -speed, y: 0.0 };
                                ui_cmd_tx.send(UICmd::Pan { delta }).unwrap();
                            }
                        }
                        Key::Down => {
                            if pressed {
                                let delta = Point { x: 0.0, y: -speed };
                                ui_cmd_tx.send(UICmd::Pan { delta }).unwrap();
                            }
                        }
                        Key::Left => {
                            if pressed {
                                let delta = Point { x: speed, y: 0.0 };
                                ui_cmd_tx.send(UICmd::Pan { delta }).unwrap();
                            }
                        }
                        _ => {}
                    }
                }
            }
            Event::WindowEvent {
                event: WindowEvent::MouseWheel { delta, .. },
                ..
            } => {
                use winit::event::MouseScrollDelta as ScrollDelta;
                match delta {
                    ScrollDelta::LineDelta(x, y) => {
                        if y > 0.0 {
                            ui_cmd_tx.send(UICmd::Zoom { delta: -0.15 }).unwrap();
                        } else if y < 0.0 {
                            ui_cmd_tx.send(UICmd::Zoom { delta: 0.15 }).unwrap();
                        }
                        println!("view scale {}", view.scale);
                    }
                    ScrollDelta::PixelDelta(pos) => {
                        println!("Scroll PixelDelta({}, {})", pos.x, pos.y);
                    }
                }
            }
            /*
            Event::WindowEvent {
                event: WindowEvent::CursorMoved { position, .. },
                ..
            } => {
                if let Some(viewport) = dynamic_state.viewports.as_ref().and_then(|v| v.get(0)) {
                    let pos_x = position.x as f32;
                    let pos_y = position.y as f32;
                    let norm_x = pos_x / viewport.dimensions[0];
                    let norm_y = pos_y / viewport.dimensions[1];
                    // view.center.x = 0.5 + (norm_x / -2.0);
                    // view.center.y = 0.5 + (norm_y / -2.0);
                    // view.center.x = (norm_x / -2.0);
                    // view.center.y = (norm_y / -2.0);

                    // ui_cmd_tx.send(UICmd::Zoom { delta: 0.05 });

                    view.center.x = 0.0;
                    view.center.y = 0.0;

                    view.width = viewport.dimensions[0];
                    view.height = viewport.dimensions[1];
                }
            }
            */
            Event::WindowEvent {
                event: WindowEvent::CloseRequested,
                ..
            } => {
                *control_flow = ControlFlow::Exit;
            }
            Event::WindowEvent {
                event: WindowEvent::Resized(_),
                ..
            } => {
                recreate_swapchain = true;
            }
            Event::RedrawEventsCleared => {
                previous_frame_end.as_mut().unwrap().cleanup_finished();

                if recreate_swapchain {
                    let dimensions: [u32; 2] = surface.window().inner_size().into();

                    let (new_swapchain, new_images) =
                        match swapchain.recreate_with_dimensions(dimensions) {
                            Ok(r) => r,
                            Err(SwapchainCreationError::UnsupportedDimensions) => return,
                            Err(e) => panic!("Failed to recreate swapchain: {:?}", e),
                        };

                    swapchain = new_swapchain;
                    framebuffers =
                        window_size_update(&new_images, render_pass.clone(), &mut dynamic_state);
                    recreate_swapchain = false;
                }

                let (image_num, suboptimal, acquire_future) =
                    match swapchain::acquire_next_image(swapchain.clone(), None) {
                        Ok(r) => r,
                        Err(AcquireError::OutOfDate) => {
                            recreate_swapchain = true;
                            return;
                        }
                        Err(e) => panic!("Failed to acquire next image: {:?}", e),
                    };

                if suboptimal {
                    recreate_swapchain = true;
                }

                let view_offset = {
                    // let vo_data = point_vert::ty::ViewOffset { x: 0.8, y: 0.1 };
                    // let vo_data = point_vert::ty::View {

                    // let view: () = ();

                    // let mat = view.to_matrix();
                    let mat = view.to_scaled_matrix();
                    let view_data = view::mat4_to_array(&mat);

                    let matrix = simple_vert::ty::View { view: view_data };

                    uniform_buffer.next(matrix).unwrap()
                };

                let layout = pipeline.layout().descriptor_set_layout(0).unwrap();
                let set = Arc::new(
                    PersistentDescriptorSet::start(layout.clone())
                        .add_buffer(view_offset)
                        .unwrap()
                        .build()
                        .unwrap(),
                );

                let clear_values = vec![[0.0, 0.0, 0.1, 1.0].into()];

                /*
                let segments = vec![
                    Segment {
                        // p0: Point { x: 0.5, y: 0.0 },
                        // p1: Point { x: 0.5, y: 0.5 },
                        p0: Point { x: 0.0, y: 0.0 },
                        p1: Point { x: 100.0, y: 100.0 },
                        // p1: Point { x: 100.0, y: 100.0 },
                        // p1: Point { x: 0.0, y: 50.0 },
                    },
                    Segment {
                        p0: Point { x: 250.0, y: 250.0 },
                        p1: Point { x: 275.0, y: 255.0 },
                    },
                ];



                let mut vertices = Vec::with_capacity(segments.len() * 4);

                for s in segments {
                    vertices.extend(s.vertices().iter());
                }
                */

                let colors = vec![
                    Color { color: 0xF0 },
                    Color { color: 0xF0 },
                    // Color { color: 0x0F },
                    // Color { color: 0x0F },
                ];

                let vertices = path_vertices(&segments);

                let vertex_buffer = vertex_buffer_pool.chunk(vertices).unwrap();
                let color_buffer = color_buffer_pool.chunk(colors).unwrap();

                let mut builder = AutoCommandBufferBuilder::primary_one_time_submit(
                    device.clone(),
                    queue.family(),
                )
                .unwrap();

                builder
                    .begin_render_pass(
                        framebuffers[image_num].clone(),
                        SubpassContents::Inline,
                        clear_values,
                    )
                    .unwrap()
                    .draw(
                        pipeline.clone(),
                        &dynamic_state,
                        vertex_buffer,
                        set.clone(),
                        (),
                    )
                    // .draw_indexed(
                    //     pipeline.clone(),
                    //     &dynamic_state,
                    //     vec![vertex_buffer, color_buffer],
                    //     set.clone(),
                    //     (),
                    // )
                    .unwrap()
                    .end_render_pass()
                    .unwrap();

                let command_buffer = builder.build().unwrap();

                let future = previous_frame_end
                    .take()
                    .unwrap()
                    .join(acquire_future)
                    .then_execute(queue.clone(), command_buffer)
                    .unwrap()
                    .then_swapchain_present(queue.clone(), swapchain.clone(), image_num)
                    .then_signal_fence_and_flush();

                match future {
                    Ok(future) => {
                        previous_frame_end = Some(future.boxed());
                    }
                    Err(FlushError::OutOfDate) => {
                        recreate_swapchain = true;
                        previous_frame_end = Some(sync::now(device.clone()).boxed());
                    }
                    Err(e) => {
                        println!("Failed to flush future: {:?}", e);
                        previous_frame_end = Some(sync::now(device.clone()).boxed());
                    }
                }
            }
            _ => (),
        }
    });
}

fn window_size_update(
    images: &[Arc<SwapchainImage<Window>>],
    render_pass: Arc<dyn RenderPassAbstract + Send + Sync>,
    dynamic_state: &mut DynamicState,
) -> Vec<Arc<dyn FramebufferAbstract + Send + Sync>> {
    let dims = images[0].dimensions();
    let dimensions = [dims[0] as f32, dims[1] as f32];

    let viewport = Viewport {
        origin: [0.0, 0.0],
        dimensions,
        depth_range: 0.0..1.0,
    };
    dynamic_state.viewports = Some(vec![viewport]);

    images
        .iter()
        .map(|image| {
            Arc::new(
                Framebuffer::start(render_pass.clone())
                    .add(image.clone())
                    .unwrap()
                    .build()
                    .unwrap(),
            ) as Arc<dyn FramebufferAbstract + Send + Sync>
        })
        .collect::<Vec<_>>()
}

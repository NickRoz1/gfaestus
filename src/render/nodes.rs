#[allow(unused_imports)]
use vulkano::buffer::{BufferUsage, CpuAccessibleBuffer, CpuBufferPool, ImmutableBuffer};
use vulkano::{
    command_buffer::{AutoCommandBuffer, AutoCommandBufferBuilder, DynamicState},
    image::StorageImage,
};
use vulkano::{
    descriptor::descriptor_set::PersistentDescriptorSet,
    framebuffer::{RenderPassAbstract, Subpass},
};
use vulkano::{device::Queue, image::Dimensions};

use vulkano::format::R32Uint;

use vulkano::pipeline::{GraphicsPipeline, GraphicsPipelineAbstract};

use std::sync::Arc;

use anyhow::Result;

use nalgebra_glm as glm;

use crate::geometry::*;
use crate::view;
use crate::view::View;

use super::Vertex;

mod vs {
    vulkano_shaders::shader! {
        ty: "vertex",
        path: "shaders/nodes/vertex.vert",
    }
}

mod gs {
    vulkano_shaders::shader! {
        ty: "geometry",
        path: "shaders/nodes/geometry.geom",
    }
}

mod fs {
    vulkano_shaders::shader! {
        ty: "fragment",
        path: "shaders/nodes/fragment.frag",
    }
}

pub struct NodeDrawSystem {
    gfx_queue: Arc<Queue>,
    vertex_buffer_pool: CpuBufferPool<Vertex>,
    pipeline: Arc<dyn GraphicsPipelineAbstract + Send + Sync>,
    // node_id_color_image: Option<Arc<StorageImage<R32Uint>>>,
    node_id_color_image: Option<Arc<StorageImage<R32Uint>>>,
    node_id_color_buffer: Option<Arc<CpuAccessibleBuffer<[u32]>>>,
    // node_id_color_buffer:
}

impl NodeDrawSystem {
    pub fn new<R>(gfx_queue: Arc<Queue>, subpass: Subpass<R>) -> NodeDrawSystem
    where
        R: RenderPassAbstract + Send + Sync + 'static,
    {
        let _ = include_str!("../../shaders/nodes/vertex.vert");
        let _ = include_str!("../../shaders/nodes/geometry.geom");
        let _ = include_str!("../../shaders/nodes/fragment.frag");

        let vs = vs::Shader::load(gfx_queue.device().clone()).unwrap();
        let fs = fs::Shader::load(gfx_queue.device().clone()).unwrap();
        let gs = gs::Shader::load(gfx_queue.device().clone()).unwrap();

        let vertex_buffer_pool: CpuBufferPool<Vertex> =
            CpuBufferPool::vertex_buffer(gfx_queue.device().clone());

        let pipeline = {
            Arc::new(
                GraphicsPipeline::start()
                    .vertex_input_single_buffer::<Vertex>()
                    .vertex_shader(vs.main_entry_point(), ())
                    .line_list()
                    .geometry_shader(gs.main_entry_point(), ())
                    .viewports_dynamic_scissors_irrelevant(1)
                    .fragment_shader(fs.main_entry_point(), ())
                    .render_pass(subpass)
                    .blend_alpha_blending()
                    .build(gfx_queue.device().clone())
                    .unwrap(),
            ) as Arc<_>
        };

        NodeDrawSystem {
            gfx_queue,
            pipeline,
            vertex_buffer_pool,
            node_id_color_image: None,
            node_id_color_buffer: None,
        }
    }

    fn id_color_image_dims(&self) -> Option<(u32, u32)> {
        if let Some(img) = &self.node_id_color_image {
            if let Dimensions::Dim2d { width, height } = img.dimensions() {
                return Some((width, height));
            }
        }
        None
    }

    fn create_id_color_image(&mut self, width: u32, height: u32) -> Result<()> {
        // Don't need to do anything if we already have an image of the correct size
        if let Some((w, h)) = self.id_color_image_dims() {
            if w == width && height == h {
                return Ok(());
            }
        }

        // Otherwise, create a new one even if we have an image

        let image = StorageImage::new(
            self.gfx_queue.device().clone(),
            Dimensions::Dim2d { width, height },
            R32Uint,
            Some(self.gfx_queue.family()),
        )?;

        let buffer = CpuAccessibleBuffer::from_iter(
            self.gfx_queue.device().clone(),
            BufferUsage::all(),
            false,
            (0..width * height).map(|_| 0u32),
        )?;

        self.node_id_color_image = Some(image);
        self.node_id_color_buffer = Some(buffer);

        Ok(())
    }

    pub fn clone_node_id_color_buffer(&self) -> Option<Vec<u32>> {
        let buf = self.node_id_color_buffer.as_ref()?;
        let buf_read = buf.read().unwrap();
        Some(Vec::from(&buf_read[..]))
    }

    pub fn read_node_id_at(
        &self,
        screen_width: u32,
        screen_height: u32,
        point: Point,
    ) -> Option<u32> {
        let xu = point.x as u32;
        let yu = point.y as u32;
        println!("reading node id at {}, {}", xu, yu);
        if xu >= screen_width || yu >= screen_height {
            return None;
        }

        let ix = yu * screen_width + xu;
        println!("buffer index {}", ix);

        let buffer = self.node_id_color_buffer.as_ref()?;
        println!("has buffer");
        let value = buffer.read().unwrap().get(ix as usize).copied();
        println!("read value");

        value
    }

    pub fn draw<VI>(
        &mut self,
        dynamic_state: &DynamicState,
        vertices: VI,
        view: View,
        offset: Point,
        node_width: f32,
    ) -> Result<AutoCommandBuffer>
    where
        VI: IntoIterator<Item = Vertex>,
        VI::IntoIter: ExactSizeIterator,
    {
        let mut builder: AutoCommandBufferBuilder = AutoCommandBufferBuilder::secondary_graphics(
            self.gfx_queue.device().clone(),
            self.gfx_queue.family(),
            self.pipeline.clone().subpass(),
        )?;

        let viewport_dims = {
            let viewport = dynamic_state
                .viewports
                .as_ref()
                .and_then(|v| v.get(0))
                .unwrap();
            viewport.dimensions
        };

        // self.create_id_color_image(viewport_dims[0] as u32, viewport_dims[1] as u32)?;

        #[rustfmt::skip]
        let view_pc = {
            // is this correct?
            let model_mat = glm::mat4(
                1.0, 0.0, 0.0, offset.x,
                0.0, 1.0, 0.0, offset.y,
                0.0, 0.0, 1.0, 0.0,
                0.0, 0.0, 0.0, 1.0
            );

            let view_mat = view.to_scaled_matrix();

            let width = viewport_dims[0];
            let height = viewport_dims[1];

            let viewport_mat = view::viewport_scale(width, height);

            let matrix = viewport_mat * view_mat * model_mat;

            let view_data = view::mat4_to_array(&matrix);

            vs::ty::View {
                node_width,
                // node_width: aspect_aware_node_width,
                viewport_dims,
                view: view_data,
                scale: view.scale,

            }
        };

        let data_buffer = {
            let data_iter =
                (0..((viewport_dims[0] as u32) * (viewport_dims[1] as u32))).map(|_| 0u32);
            CpuAccessibleBuffer::from_iter(
                self.gfx_queue.device().clone(),
                BufferUsage::all(),
                false,
                data_iter,
            )?
        };

        let layout = self.pipeline.descriptor_set_layout(0).unwrap();
        let set = {
            let set =
                PersistentDescriptorSet::start(layout.clone()).add_buffer(data_buffer.clone())?;
            // .add_image(self.node_id_color_image.as_ref().unwrap().clone())?;
            let set = set.build()?;
            Arc::new(set)
        };

        self.node_id_color_buffer = Some(data_buffer.clone());

        let vertex_buffer = self.vertex_buffer_pool.chunk(vertices)?;

        builder.draw(
            self.pipeline.clone(),
            dynamic_state,
            vec![Arc::new(vertex_buffer)],
            set.clone(),
            view_pc,
        )?;

        let builder = builder.build()?;

        Ok(builder)
    }
}

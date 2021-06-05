use ash::version::DeviceV1_0;
use ash::{vk, Device};

use std::ffi::CString;

use anyhow::Result;

use super::create_shader_module;

use crate::vulkan::{texture::Texture, GfaestusVk};
use crate::{geometry::Point, vulkan::render_pass::Framebuffers};

pub struct PostProcessPipeline {
    descriptor_pool: vk::DescriptorPool,
    descriptor_set_layout: vk::DescriptorSetLayout,
    descriptor_set: vk::DescriptorSet,

    pipeline_layout: vk::PipelineLayout,
    pipeline: vk::Pipeline,
}

impl PostProcessPipeline {
    pub fn new(
        app: &GfaestusVk,
        image_count: u32,
        render_pass: vk::RenderPass,
        frag_src: &[u8],
    ) -> Result<Self> {
        let vk_context = app.vk_context();
        let device = vk_context.device();

        let layout = Self::create_descriptor_set_layout(device)?;

        let descriptor_pool = {
            let pool_size = vk::DescriptorPoolSize {
                ty: vk::DescriptorType::COMBINED_IMAGE_SAMPLER,
                descriptor_count: image_count,
            };

            let pool_sizes = [pool_size];

            let pool_info = vk::DescriptorPoolCreateInfo::builder()
                .pool_sizes(&pool_sizes)
                .max_sets(image_count)
                .build();

            unsafe { device.create_descriptor_pool(&pool_info, None) }
        }?;

        let descriptor_sets = {
            let layouts = vec![layout];

            let alloc_info = vk::DescriptorSetAllocateInfo::builder()
                .descriptor_pool(descriptor_pool)
                .set_layouts(&layouts)
                .build();

            unsafe { device.allocate_descriptor_sets(&alloc_info) }
        }?;

        let (pipeline, pipeline_layout) =
            Self::create_pipeline(device, render_pass, layout, frag_src);

        Ok(Self {
            descriptor_pool,
            descriptor_set_layout: layout,
            descriptor_set: descriptor_sets[0],
            pipeline_layout,
            pipeline,
        })
    }

    pub fn new_buffer_read(
        app: &GfaestusVk,
        image_count: u32,
        render_pass: vk::RenderPass,
        frag_src: &[u8],
    ) -> Result<Self> {
        let vk_context = app.vk_context();
        let device = vk_context.device();

        let layout = Self::create_buffer_descriptor_set_layout(device)?;

        let descriptor_pool = {
            let pool_size = vk::DescriptorPoolSize {
                ty: vk::DescriptorType::STORAGE_BUFFER,
                descriptor_count: image_count,
            };

            let pool_sizes = [pool_size];

            let pool_info = vk::DescriptorPoolCreateInfo::builder()
                .pool_sizes(&pool_sizes)
                .max_sets(image_count)
                .build();

            unsafe { device.create_descriptor_pool(&pool_info, None) }
        }?;

        let descriptor_sets = {
            let layouts = vec![layout];

            let alloc_info = vk::DescriptorSetAllocateInfo::builder()
                .descriptor_pool(descriptor_pool)
                .set_layouts(&layouts)
                .build();

            unsafe { device.allocate_descriptor_sets(&alloc_info) }
        }?;

        let (pipeline, pipeline_layout) =
            Self::create_pipeline(device, render_pass, layout, frag_src);

        Ok(Self {
            descriptor_pool,
            descriptor_set_layout: layout,
            descriptor_set: descriptor_sets[0],
            pipeline_layout,
            pipeline,
        })
    }

    pub fn write_buffer_descriptor_set(
        &mut self,
        device: &Device,
        buffer: vk::Buffer,
    ) {
        let pixels_buf_info = vk::DescriptorBufferInfo::builder()
            .buffer(buffer)
            .offset(0)
            .range(vk::WHOLE_SIZE)
            .build();

        let pixels_buf_infos = [pixels_buf_info];

        let pixels = vk::WriteDescriptorSet::builder()
            .dst_set(self.descriptor_set)
            .dst_binding(0)
            .dst_array_element(0)
            .descriptor_type(vk::DescriptorType::STORAGE_BUFFER)
            .buffer_info(&pixels_buf_infos)
            .build();

        let descriptor_writes = [pixels];

        unsafe { device.update_descriptor_sets(&descriptor_writes, &[]) }
    }

    pub fn write_descriptor_set(
        &mut self,
        device: &Device,
        new_image: Texture,
        sampler: Option<vk::Sampler>,
    ) {
        let sampler = sampler.unwrap_or_else(|| new_image.sampler.unwrap());

        let image_info = vk::DescriptorImageInfo::builder()
            .image_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)
            .image_view(new_image.view)
            .sampler(sampler)
            .build();
        let image_infos = [image_info];

        let sampler_descriptor_write = vk::WriteDescriptorSet::builder()
            .dst_set(self.descriptor_set)
            .dst_binding(0)
            .dst_array_element(0)
            .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
            .image_info(&image_infos)
            .build();

        let descriptor_writes = [sampler_descriptor_write];

        unsafe { device.update_descriptor_sets(&descriptor_writes, &[]) }
    }

    pub fn draw(
        &self,
        device: &Device,
        cmd_buf: vk::CommandBuffer,
        render_pass: vk::RenderPass,
        framebuffers: &Framebuffers,
        screen_size: Point,
        sample_size: Point,
    ) -> Result<()> {
        let clear_values = {
            [vk::ClearValue {
                color: vk::ClearColorValue {
                    float32: [0.0, 0.0, 0.0, 0.0],
                },
            }]
        };

        let extent = vk::Extent2D {
            width: screen_size.x as u32,
            height: screen_size.y as u32,
        };

        let render_pass_begin_info = vk::RenderPassBeginInfo::builder()
            .render_pass(render_pass)
            .framebuffer(framebuffers.selection_blur)
            .render_area(vk::Rect2D {
                offset: vk::Offset2D { x: 0, y: 0 },
                extent,
            })
            .clear_values(&clear_values)
            .build();

        unsafe {
            device.cmd_begin_render_pass(
                cmd_buf,
                &render_pass_begin_info,
                vk::SubpassContents::INLINE,
            )
        };

        unsafe {
            device.cmd_bind_pipeline(
                cmd_buf,
                vk::PipelineBindPoint::GRAPHICS,
                self.pipeline,
            )
        };

        let desc_sets = [self.descriptor_set];

        unsafe {
            let null = [];
            device.cmd_bind_descriptor_sets(
                cmd_buf,
                vk::PipelineBindPoint::GRAPHICS,
                self.pipeline_layout,
                0,
                &desc_sets[0..=0],
                &null,
            );
        };

        let push_constants = PushConstants::new(sample_size, screen_size, true);

        let pc_bytes = push_constants.bytes();

        unsafe {
            use vk::ShaderStageFlags as Flags;
            device.cmd_push_constants(
                cmd_buf,
                self.pipeline_layout,
                Flags::VERTEX | Flags::FRAGMENT,
                0,
                &pc_bytes,
            )
        };

        unsafe { device.cmd_draw(cmd_buf, 3u32, 1, 0, 0) };

        // End render pass
        unsafe { device.cmd_end_render_pass(cmd_buf) };

        Ok(())
    }

    pub fn destroy(&self, device: &Device) {
        unsafe {
            device.destroy_descriptor_set_layout(
                self.descriptor_set_layout,
                None,
            );
            device.destroy_descriptor_pool(self.descriptor_pool, None);

            device.destroy_pipeline(self.pipeline, None);
            device.destroy_pipeline_layout(self.pipeline_layout, None);
        }
    }

    fn layout_binding() -> vk::DescriptorSetLayoutBinding {
        use vk::ShaderStageFlags as Stages;

        vk::DescriptorSetLayoutBinding::builder()
            .binding(0)
            .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
            .descriptor_count(1)
            .stage_flags(Stages::FRAGMENT)
            .build()
    }

    fn create_descriptor_set_layout(
        device: &Device,
    ) -> Result<vk::DescriptorSetLayout> {
        let binding = Self::layout_binding();
        let bindings = [binding];

        let layout_info = vk::DescriptorSetLayoutCreateInfo::builder()
            .bindings(&bindings)
            .build();

        let layout =
            unsafe { device.create_descriptor_set_layout(&layout_info, None) }?;

        Ok(layout)
    }

    fn create_buffer_descriptor_set_layout(
        device: &Device,
    ) -> Result<vk::DescriptorSetLayout> {
        use vk::ShaderStageFlags as Stages;

        let binding = vk::DescriptorSetLayoutBinding::builder()
            .binding(0)
            .descriptor_type(vk::DescriptorType::STORAGE_BUFFER)
            .descriptor_count(1)
            .stage_flags(Stages::FRAGMENT)
            .build();

        let bindings = [binding];

        let layout_info = vk::DescriptorSetLayoutCreateInfo::builder()
            .bindings(&bindings)
            .build();

        let layout =
            unsafe { device.create_descriptor_set_layout(&layout_info, None) }?;

        Ok(layout)
    }

    fn create_pipeline(
        device: &Device,
        render_pass: vk::RenderPass,
        descriptor_set_layout: vk::DescriptorSetLayout,
        frag_src: &[u8],
    ) -> (vk::Pipeline, vk::PipelineLayout) {
        create_pipeline(
            device,
            render_pass,
            descriptor_set_layout,
            crate::include_shader!("post/post.vert.spv"),
            frag_src,
        )
    }
}

pub(crate) fn create_pipeline(
    device: &Device,
    render_pass: vk::RenderPass,
    descriptor_set_layout: vk::DescriptorSetLayout,
    vert_shader: &[u8],
    frag_shader: &[u8],
) -> (vk::Pipeline, vk::PipelineLayout) {
    let vert_src = {
        let mut cursor = std::io::Cursor::new(vert_shader);
        ash::util::read_spv(&mut cursor).unwrap()
    };
    let frag_src = {
        let mut cursor = std::io::Cursor::new(frag_shader);
        ash::util::read_spv(&mut cursor).unwrap()
    };

    let vert_module = create_shader_module(device, &vert_src);
    let frag_module = create_shader_module(device, &frag_src);

    let entry_point = CString::new("main").unwrap();

    let vert_state_info = vk::PipelineShaderStageCreateInfo::builder()
        .stage(vk::ShaderStageFlags::VERTEX)
        .module(vert_module)
        .name(&entry_point)
        .build();

    let frag_state_info = vk::PipelineShaderStageCreateInfo::builder()
        .stage(vk::ShaderStageFlags::FRAGMENT)
        .module(frag_module)
        .name(&entry_point)
        .build();

    let shader_state_infos = [vert_state_info, frag_state_info];

    let vert_input_info =
        vk::PipelineVertexInputStateCreateInfo::builder().build();

    let input_assembly_info =
        vk::PipelineInputAssemblyStateCreateInfo::builder()
            .topology(vk::PrimitiveTopology::TRIANGLE_LIST)
            .primitive_restart_enable(false)
            .build();

    let viewport_info = vk::PipelineViewportStateCreateInfo::builder()
        .viewport_count(1)
        .scissor_count(1)
        .build();

    let dynamic_states = {
        use vk::DynamicState as DS;
        [DS::VIEWPORT, DS::SCISSOR]
    };

    let dynamic_state_info = vk::PipelineDynamicStateCreateInfo::builder()
        .dynamic_states(&dynamic_states)
        .build();

    let rasterizer_info = vk::PipelineRasterizationStateCreateInfo::builder()
        .depth_clamp_enable(false)
        .rasterizer_discard_enable(false)
        .polygon_mode(vk::PolygonMode::FILL)
        .line_width(1.0)
        .cull_mode(vk::CullModeFlags::NONE)
        .front_face(vk::FrontFace::COUNTER_CLOCKWISE)
        .depth_bias_enable(false)
        .depth_bias_constant_factor(0.0)
        .depth_bias_clamp(0.0)
        .depth_bias_slope_factor(0.0)
        .build();

    let multisampling_info = vk::PipelineMultisampleStateCreateInfo::builder()
        .sample_shading_enable(false)
        .rasterization_samples(vk::SampleCountFlags::TYPE_1)
        .min_sample_shading(1.0)
        .alpha_to_coverage_enable(false)
        .alpha_to_one_enable(false)
        .build();

    let color_blend_attachment =
        vk::PipelineColorBlendAttachmentState::builder()
            .color_write_mask(vk::ColorComponentFlags::all())
            .blend_enable(true)
            .src_color_blend_factor(vk::BlendFactor::SRC_ALPHA)
            .dst_color_blend_factor(vk::BlendFactor::ONE_MINUS_SRC_ALPHA)
            .color_blend_op(vk::BlendOp::ADD)
            .src_alpha_blend_factor(vk::BlendFactor::SRC_ALPHA)
            .dst_alpha_blend_factor(vk::BlendFactor::ONE_MINUS_SRC_ALPHA)
            .alpha_blend_op(vk::BlendOp::ADD)
            .build();

    let color_blend_attachments = [color_blend_attachment];

    let color_blending_info = vk::PipelineColorBlendStateCreateInfo::builder()
        .logic_op_enable(false)
        .logic_op(vk::LogicOp::COPY)
        .attachments(&color_blend_attachments)
        .blend_constants([0.0, 0.0, 0.0, 0.0])
        .build();

    let layout = {
        use vk::ShaderStageFlags as Flags;

        let layouts = [descriptor_set_layout];

        let pc_range = vk::PushConstantRange::builder()
            .stage_flags(Flags::VERTEX | Flags::FRAGMENT)
            .offset(0)
            .size(PushConstants::PC_RANGE)
            .build();

        let pc_ranges = [pc_range];

        let layout_info = vk::PipelineLayoutCreateInfo::builder()
            .set_layouts(&layouts)
            .push_constant_ranges(&pc_ranges)
            .build();

        unsafe { device.create_pipeline_layout(&layout_info, None).unwrap() }
    };

    let pipeline_info = vk::GraphicsPipelineCreateInfo::builder()
        .stages(&shader_state_infos)
        .vertex_input_state(&vert_input_info)
        .input_assembly_state(&input_assembly_info)
        .viewport_state(&viewport_info)
        .dynamic_state(&dynamic_state_info)
        .rasterization_state(&rasterizer_info)
        .multisample_state(&multisampling_info)
        .color_blend_state(&color_blending_info)
        .layout(layout)
        .render_pass(render_pass)
        .subpass(0)
        .build();

    let pipeline_infos = [pipeline_info];

    let pipeline = unsafe {
        device
            .create_graphics_pipelines(
                vk::PipelineCache::null(),
                &pipeline_infos,
                None,
            )
            .unwrap()[0]
    };

    unsafe {
        device.destroy_shader_module(vert_module, None);
        device.destroy_shader_module(frag_module, None);
    }

    (pipeline, layout)
}

pub struct PushConstants {
    source_size: Point,
    target_size: Point,
    enabled: bool,
}

impl PushConstants {
    pub const PC_RANGE: u32 =
        (std::mem::size_of::<u32>() + std::mem::size_of::<f32>() * 4) as u32;

    #[inline]
    pub fn new(source_size: Point, target_size: Point, enabled: bool) -> Self {
        Self {
            source_size,
            target_size,
            enabled,
        }
    }

    #[inline]
    pub fn bytes(&self) -> [u8; 20] {
        let mut bytes = [0u8; Self::PC_RANGE as usize];

        {
            let mut offset = 0;

            let mut add_float = |f: f32| {
                let f_bytes = f.to_ne_bytes();
                for i in 0..4 {
                    bytes[offset] = f_bytes[i];
                    offset += 1;
                }
            };

            add_float(self.source_size.x);
            add_float(self.source_size.y);

            add_float(self.target_size.x);
            add_float(self.target_size.y);
        }

        if self.enabled {
            bytes[19] = 1;
        } else {
            bytes[19] = 0;
        }

        bytes
    }
}

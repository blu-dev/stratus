use std::{collections::HashMap, rc::Rc};

use bitvec::vec::BitVec;
use bytemuck::Zeroable;
use envy::{ImageScalingMode, TextureRequestArgs};

use crate::{
    menu::{
        font::{FontHandle, FontManager, FontStage, GlyphHandle},
        shaders::{PerDrawCBuffer, PerViewCBuffer, TexturePipeline, TextureVertex, VertexPipeline},
    },
    nvn::{
        self,
        abstraction::{BufferVec, ImageSampler, ManagedImages, StagedBuffer},
    },
};

pub struct NvnBackendStage<'a> {
    constant_buffer: StagedBuffer<'a>,
    texture_buffer: StagedBuffer<'a>,
    view_buffer: StagedBuffer<'a>,
    draw_buffer: StagedBuffer<'a>,
    font: FontStage<'a>,
}

impl NvnBackendStage<'_> {
    pub fn exec(self) {
        self.constant_buffer.execute();
        self.texture_buffer.execute();
        self.view_buffer.execute();
        self.draw_buffer.execute();
        self.font.exec();
    }
}

struct EnvyTextureInfo {
    requested_id: usize,
    size: glam::UVec2,
    scaling_x: ImageScalingMode,
    scaling_y: ImageScalingMode,
}

pub struct NvnBackend {
    device: Rc<nvn::Device>,
    vertex_pipeline: VertexPipeline,
    texture_pipeline: TexturePipeline,
    constant_vertex_buffer: BufferVec<glam::Vec3>,
    texture_vertex_buffer: BufferVec<TextureVertex>,
    view_uniform: BufferVec<PerViewCBuffer>,
    draw_uniform: BufferVec<PerDrawCBuffer>,
    draw_uniform_availability: BitVec,
    images: ManagedImages,
    envy_images: HashMap<nvn::TextureHandle, EnvyTextureInfo>,
    fonts: FontManager,
}

impl NvnBackend {
    pub fn new(device: Rc<nvn::Device>) -> Self {
        let mut view_uniform = BufferVec::with_capacity(&device, 1);
        view_uniform.push(PerViewCBuffer {
            view_projection_matrix: glam::Mat4::orthographic_lh(0.0, 1920.0, 1080.0, 0.0, 0.0, 1.0),
            view_matrix: glam::Mat4::IDENTITY,
            padding: [0u8; 0x80],
        });

        let texture_vertex_buffer = BufferVec::new(&device);

        Self {
            vertex_pipeline: VertexPipeline::new(&device),
            texture_pipeline: TexturePipeline::new(&device),
            constant_vertex_buffer: BufferVec::new(&device),
            texture_vertex_buffer,
            view_uniform,
            draw_uniform: BufferVec::new(&device),
            draw_uniform_availability: BitVec::new(),
            images: ManagedImages::new(device.clone(), 0x100),
            envy_images: HashMap::new(),
            fonts: FontManager::new(&device),
            device,
        }
    }

    pub fn prepare_render(&self, cmdbuf: &mut nvn::CommandBuffer) {
        cmdbuf.set_texture_pool(self.images.texpool());
        cmdbuf.set_sampler_pool(self.images.sampool());
        cmdbuf.bind_uniform_buffer(
            0,
            0,
            self.view_uniform.buffer().get_address(),
            std::mem::size_of::<PerViewCBuffer>(),
        );
    }

    pub fn stage(&mut self) -> NvnBackendStage<'_> {
        NvnBackendStage {
            constant_buffer: self.constant_vertex_buffer.stage(&self.device),
            texture_buffer: self.texture_vertex_buffer.stage(&self.device),
            view_buffer: self.view_uniform.stage(&self.device),
            draw_buffer: self.draw_uniform.stage(&self.device),
            font: self.fonts.stage(&self.device),
        }
    }
}

#[repr(transparent)]
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct UniformHandle(pub(super) usize);

impl envy::EnvyBackend for NvnBackend {
    type TextureHandle = nvn::TextureHandle;

    type UniformHandle = UniformHandle;

    type FontHandle = FontHandle;

    type GlyphHandle = GlyphHandle;

    type RenderPass<'a> = nvn::CommandBuffer;

    fn request_texture_by_name(
        &mut self,
        name: impl AsRef<str>,
        args: TextureRequestArgs,
    ) -> Option<Self::TextureHandle> {
        let info = self.images.request_texture(
            name.as_ref(),
            ImageSampler {
                wrap_mode_x: match args.scaling_x {
                    ImageScalingMode::Stretch => nvn::WrapMode::ClampEdge,
                    ImageScalingMode::Tiling => nvn::WrapMode::Repeat,
                },
                wrap_mode_y: match args.scaling_y {
                    ImageScalingMode::Stretch => nvn::WrapMode::ClampEdge,
                    ImageScalingMode::Tiling => nvn::WrapMode::Repeat,
                },
            },
        )?;

        let len = self.envy_images.len();
        self.envy_images.insert(
            info.handle,
            EnvyTextureInfo {
                requested_id: len,
                size: info.size,
                scaling_x: args.scaling_x,
                scaling_y: args.scaling_y,
            },
        );

        self.texture_vertex_buffer.extend([
            TextureVertex::TOP_LEFT,
            TextureVertex::TOP_RIGHT,
            TextureVertex::BOTTOM_LEFT,
            TextureVertex::BOTTOM_LEFT,
            TextureVertex::TOP_RIGHT,
            TextureVertex::BOTTOM_RIGHT,
        ]);

        Some(info.handle)
    }

    fn request_font_by_name(&mut self, name: impl AsRef<str>) -> Option<Self::FontHandle> {
        self.fonts.get_handle(name.as_ref())
    }

    fn request_new_uniform(&mut self) -> Option<Self::UniformHandle> {
        if let Some(first_zero) = self.draw_uniform_availability.first_zero() {
            self.draw_uniform_availability.set(first_zero, true);
            Some(UniformHandle(first_zero))
        } else {
            let len = self.draw_uniform.len();
            self.draw_uniform.push(PerDrawCBuffer::zeroed());
            self.draw_uniform_availability.push(true);
            Some(UniformHandle(len))
        }
    }

    fn release_texture(&mut self, _handle: Self::TextureHandle) {}

    fn release_font(&mut self, _handle: Self::FontHandle) {}

    fn release_uniform(&mut self, handle: Self::UniformHandle) {
        self.draw_uniform_availability.set(handle.0, false);
    }

    fn update_uniform(&mut self, handle: Self::UniformHandle, uniform: envy::DrawUniform) {
        self.draw_uniform[handle.0] = PerDrawCBuffer {
            world_matrix: uniform.model_matrix,
            base_color: uniform.color,
            world_inverse_matrix: uniform.model_i_matrix,
            padding: [0u8; 0x70],
        };
    }

    fn layout_text(
        &mut self,
        args: envy::TextLayoutArgs<'_, Self>,
    ) -> Vec<envy::PreparedGlyph<Self>> {
        self.fonts.layout(|| {
            if let Some(first_zero) = self.draw_uniform_availability.first_zero() {
                self.draw_uniform_availability.set(first_zero, true);
                UniformHandle(first_zero)
            } else {
                let len = self.draw_uniform.len();
                self.draw_uniform.push(PerDrawCBuffer::zeroed());
                self.draw_uniform_availability.push(true);
                UniformHandle(len)
            }
        }, args)
    }

    fn draw_texture(
        &self,
        uniform: Self::UniformHandle,
        handle: Self::TextureHandle,
        pass: &mut Self::RenderPass<'_>,
    ) {
        let texture = self.envy_images.get(&handle).unwrap();
        self.texture_pipeline.bind(pass);
        pass.bind_vertex_buffer(
            0,
            self.texture_vertex_buffer.buffer().get_address(),
            std::mem::size_of::<TextureVertex>() * self.texture_vertex_buffer.len(),
        );
        pass.bind_uniform_buffer(
            0,
            1,
            self.draw_uniform.address_for_element(uniform.0),
            std::mem::size_of::<PerDrawCBuffer>(),
        );
        pass.bind_texture(1, 0, handle);
        pass.draw_arrays(4, texture.requested_id as i32 * 6, 6);
    }

    fn draw_glyph(
        &self,
        uniform: Self::UniformHandle,
        outline_uniform: Option<Self::UniformHandle>,
        handle: Self::GlyphHandle,
        pass: &mut Self::RenderPass<'_>,
    ) {
        self.vertex_pipeline.bind(pass);
        pass.bind_uniform_buffer(
            0,
            1,
            self.draw_uniform.address_for_element(uniform.0),
            std::mem::size_of::<PerDrawCBuffer>(),
        );
        self.fonts.bind_vertex_buffer(0, pass);
        self.fonts.draw_glyph(handle, pass);

        if let Some(outline) = outline_uniform {
            pass.bind_uniform_buffer(
                0,
                1,
                self.draw_uniform.address_for_element(outline.0),
                std::mem::size_of::<PerDrawCBuffer>(),
            );
            self.fonts.draw_glyph_outline(handle, pass);
        }
    }

    fn draw_texture_ext(
        &self,
        uniform: Self::UniformHandle,
        args: envy::DrawTextureArgs<Self>,
        pass: &mut Self::RenderPass<'_>,
    ) {
        self.draw_texture(uniform, args.texture, pass);
    }

    fn update_texture_scaling(
        &mut self,
        handle: Self::TextureHandle,
        uv_offset: glam::Vec2,
        uv_scale: glam::Vec2,
        size: glam::Vec2,
    ) {
        let texture = self.envy_images.get(&handle).unwrap();
        let texture_size = texture.size.as_vec2();

        let mut vertices = [
            TextureVertex::TOP_LEFT,
            TextureVertex::TOP_RIGHT,
            TextureVertex::BOTTOM_LEFT,
            TextureVertex::BOTTOM_LEFT,
            TextureVertex::TOP_RIGHT,
            TextureVertex::BOTTOM_RIGHT,
        ];

        match texture.scaling_x {
            ImageScalingMode::Stretch => {}
            ImageScalingMode::Tiling => {
                vertices
                    .iter_mut()
                    .for_each(|vertex| vertex.texcoord.x *= size.x / texture_size.x);
            }
        }

        match texture.scaling_y {
            ImageScalingMode::Stretch => {}
            ImageScalingMode::Tiling => {
                vertices
                    .iter_mut()
                    .for_each(|vertex| vertex.texcoord.y *= size.y / texture_size.y);
            }
        }

        let uv_scale = glam::Vec2::new(
            if uv_scale.x == 0.0 {
                0.0
            } else {
                uv_scale.x.recip()
            },
            if uv_scale.y == 0.0 {
                0.0
            } else {
                uv_scale.y.recip()
            },
        );

        vertices.iter_mut().for_each(|vert| {
            vert.texcoord = vert.texcoord * uv_scale + uv_offset / texture_size;
        });

        self.texture_vertex_buffer[texture.requested_id * 6..(texture.requested_id + 1) * 6]
            .copy_from_slice(&vertices);
    }
}

impl envy::asset::EnvyAssetProvider for NvnBackend {
    fn load_image_bytes_with_name(&mut self, name: String, bytes: Vec<u8>) {
        self.images.load_texture(&self.device, name, &bytes);
    }

    fn load_font_bytes_with_name(&mut self, name: String, bytes: Vec<u8>) {
        self.fonts.add_font(name, bytes);
    }

    fn fetch_image_bytes_by_name<'a>(&'a self, _name: &str) -> std::borrow::Cow<'a, [u8]> {
        unimplemented!("Not supported in NVN Backend")
    }

    fn fetch_font_bytes_by_name<'a>(&'a self, _name: &str) -> std::borrow::Cow<'a, [u8]> {
        unimplemented!("Not supported in NVN Backend")
    }
}

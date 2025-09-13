use std::rc::Rc;

use bitvec::vec::BitVec;
use bytemuck::Zeroable;

use crate::{menu::{font::{FontHandle, FontManager, FontStage, GlyphHandle}, shaders::{PerDrawCBuffer, PerViewCBuffer, TexturePipeline, TextureVertex, VertexPipeline}}, nvn::{self, abstraction::{BufferVec, ManagedImages, StagedBuffer}}};

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
    fonts: FontManager,
}

impl NvnBackend {
    pub fn new(device: Rc<nvn::Device>) -> Self {
        let mut view_uniform = BufferVec::with_capacity(&device, 1);
        view_uniform.push(PerViewCBuffer {
            view_projection_matrix: glam::Mat4::orthographic_lh(0.0, 1920.0, 1080.0, 0.0, 0.0, 1.0),
            view_matrix: glam::Mat4::IDENTITY,
            padding: [0u8; 0x80]
        });

        let mut texture_vertex_buffer = BufferVec::new(&device);
        texture_vertex_buffer.extend([
            TextureVertex::TOP_LEFT,
            TextureVertex::TOP_RIGHT,
            TextureVertex::BOTTOM_LEFT,
            TextureVertex::BOTTOM_LEFT,
            TextureVertex::TOP_RIGHT,
            TextureVertex::BOTTOM_RIGHT
        ]);

        Self {
            vertex_pipeline: VertexPipeline::new(&device),
            texture_pipeline: TexturePipeline::new(&device),
            constant_vertex_buffer: BufferVec::new(&device),
            texture_vertex_buffer,
            view_uniform,
            draw_uniform: BufferVec::new(&device),
            draw_uniform_availability: BitVec::new(),
            images: ManagedImages::new(&device, 0x100),
            fonts: FontManager::new(&device),
            device
        }
    }

    pub fn prepare_render(&self, cmdbuf: &mut nvn::CommandBuffer) {
        cmdbuf.set_texture_pool(self.images.texpool());
        cmdbuf.set_sampler_pool(self.images.sampool());
        cmdbuf.bind_uniform_buffer(0, 0, self.view_uniform.buffer().get_address(), std::mem::size_of::<PerViewCBuffer>());
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
pub struct UniformHandle(pub(in super) usize);

impl envy::EnvyBackend for NvnBackend {
    type TextureHandle = nvn::TextureHandle;

    type UniformHandle = UniformHandle;

    type FontHandle = FontHandle;

    type GlyphHandle = GlyphHandle;

    type RenderPass<'a> = nvn::CommandBuffer;

    fn request_texture_by_name(&mut self, name: impl AsRef<str>) -> Option<Self::TextureHandle> {
        self.images.get_info(name.as_ref()).map(|info| info.handle)
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

    fn release_texture(&mut self, _handle: Self::TextureHandle) {
    }

    fn release_font(&mut self, _handle: Self::FontHandle) {
    }

    fn release_uniform(&mut self, handle: Self::UniformHandle) {
        self.draw_uniform_availability.set(handle.0, false);
    }

    fn update_uniform(&mut self, handle: Self::UniformHandle, uniform: envy::DrawUniform) {
        self.draw_uniform[handle.0] = PerDrawCBuffer {
            world_matrix: uniform.model_matrix,
            base_color: uniform.color,
            world_inverse_matrix: uniform.model_i_matrix,
            padding: [0u8; 0x70]
        };
    }

    fn layout_text(&mut self, args: envy::TextLayoutArgs<'_, Self>) -> Vec<envy::PreparedGlyph<Self>> {
        self.fonts.layout(&mut self.draw_uniform, args)
    }

    fn draw_texture(
        &self,
        uniform: Self::UniformHandle,
        handle: Self::TextureHandle,
        pass: &mut Self::RenderPass<'_>,
    ) {
        self.texture_pipeline.bind(pass);
        pass.bind_vertex_buffer(0, self.texture_vertex_buffer.buffer().get_address(), std::mem::size_of::<TextureVertex>() * 6);
        pass.bind_uniform_buffer(0, 1, self.draw_uniform.address_for_element(uniform.0), std::mem::size_of::<PerDrawCBuffer>());
        pass.bind_texture(1, 0, handle);
        pass.draw_arrays(4, 0, 6);
    }

    fn draw_glyph(
        &self,
        uniform: Self::UniformHandle,
        handle: Self::GlyphHandle,
        pass: &mut Self::RenderPass<'_>,
    ) {
        self.vertex_pipeline.bind(pass);
        pass.bind_uniform_buffer(0, 1, self.draw_uniform.address_for_element(uniform.0), std::mem::size_of::<PerDrawCBuffer>());
        self.fonts.bind_vertex_buffer(0, pass);
        self.fonts.draw_glyph(handle, pass);
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

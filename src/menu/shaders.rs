//! This module is the pride and joy of stratus
//! 
//! These shaders are a `.nushdb` file that is embedded into the main executable.
//!
//! Through some reverse engineering effort, we were able to isolate the shader code vs. control
//! sections (it's probably a standard Nintendoware format tbh) to load them on our own
//!
//! On top of that, through runtime reflection (code not available in this repo), we were able to
//! determine the layout of the shader files and their uniforms, allowing us to interface with
//! shaders to provide a fully hardware-accelerated rendering environment.

use bytemuck::{Pod, Zeroable};
use glam::{Mat4, Vec4};

use crate::nvn::{self, abstraction::ManagedProgram, VertexAttribState, VertexStreamState};

#[repr(C)]
#[derive(Copy, Clone, Pod, Zeroable)]
pub struct PerViewCBuffer {
    pub view_projection_matrix: glam::Mat4,
    pub view_matrix: glam::Mat4,
    pub padding: [u8; 0x80],
}

#[repr(C)]
#[derive(Copy, Clone)]
pub struct PerDrawCBuffer {
    pub world_matrix: glam::Mat4,
    pub base_color: glam::Vec4,
    pub world_inverse_matrix: glam::Mat4,
    pub padding: [u8; 0x70],
}

// Manually impl traits because the derive doesn't like padding ig
unsafe impl Pod for PerDrawCBuffer {
}

unsafe impl Zeroable for PerDrawCBuffer {
    fn zeroed() -> Self {
        Self {
            world_matrix: glam::Mat4::zeroed(),
            base_color: glam::Vec4::zeroed(),
            world_inverse_matrix: glam::Mat4::zeroed(),
            padding: [0u8; 0x70]
        }
    }
}

impl Default for PerDrawCBuffer {
    fn default() -> Self {
        Self {
            world_matrix: Mat4::IDENTITY,
            base_color: Vec4::ONE,
            world_inverse_matrix: Mat4::IDENTITY,
            padding: [0u8; 0x70]
        }
    }
}

#[derive(Debug, Copy, Clone)]
pub enum StaticShaderData {
    SystemDebugDrawConstantVS,
    SystemDebugDrawConstantPS,
    SystemDebugDrawTexture2DVS,
    SystemDebugDrawTexture2DPS,
}

impl StaticShaderData {
    pub fn offset(&self) -> usize {
        match self {
            Self::SystemDebugDrawConstantVS => 0x4741338,
            Self::SystemDebugDrawConstantPS => 0x47443a8,
            Self::SystemDebugDrawTexture2DVS => 0x474dd90,
            Self::SystemDebugDrawTexture2DPS => 0x4750cd0,
        }
    }
}

macro_rules! extract_bytes {
    ($bytes:expr, $start:expr, $len:expr, $T:ty) => {{
        let mut repr = [0u8; $len];
        let slice =
            unsafe { std::slice::from_raw_parts($bytes.add($start as usize), $len as usize) };
        repr.copy_from_slice(slice);
        <$T>::from_le_bytes(repr)
    }};
}

pub fn get_shaders(shader: StaticShaderData) -> (*const u8, &'static [u8]) {
    let offset = shader.offset();
    let shader = unsafe {
        skyline::hooks::getRegionAddress(skyline::hooks::Region::Text)
            .cast::<u8>()
            .add(offset)
    };
    let section_count = extract_bytes!(shader, 0x4C, 4, u32);

    for section_idx in 0..section_count {
        let section_start = (section_idx + 1) * 0x90;
        // Identify which section is the shader program
        if extract_bytes!(shader, section_start + 0x8, 4, u32) != 0u32 {
            continue;
        }

        let control_offset1 = extract_bytes!(shader, section_start + 0x4, 4, u32);
        let control_offset2 = extract_bytes!(shader, section_start + 0x30, 4, u32);
        let data_offset1 = extract_bytes!(shader, section_start + 0x4, 4, u32);
        let data_offset2 = extract_bytes!(shader, section_start + 0x34, 4, u32);
        let data_size = extract_bytes!(shader, section_start + 0x38, 4, u32);

        let control_ptr = unsafe { shader.add((control_offset1 + control_offset2) as usize) };
        let code_section = unsafe {
            std::slice::from_raw_parts(
                shader.add((data_offset1 + data_offset2) as usize),
                data_size as usize,
            )
        };

        return (control_ptr, code_section);
    }

    panic!("Invalid shader accessor");
}

pub struct VertexPipeline {
    program: ManagedProgram,
    attrib_state: VertexAttribState,
    stream_state: VertexStreamState,
    blend_state: nvn::BlendState,
    color_state: nvn::ColorState,
    channel_state: nvn::ChannelMaskState,
    multisample_state: nvn::MultisampleState,
    polygon_state: nvn::PolygonState,
    depth_stencil_state: nvn::DepthStencilState,
}

impl VertexPipeline {
    pub fn new(device: &nvn::Device) -> Self {
        let (vctrl, vcode) = get_shaders(StaticShaderData::SystemDebugDrawConstantVS);
        let (fctrl, fcode) = get_shaders(StaticShaderData::SystemDebugDrawConstantPS);

        let program = ManagedProgram::new(device, vctrl, vcode, fctrl, fcode);
        let mut attrib_state = nvn::VertexAttribState::zeroed();
        attrib_state.set_defaults();
        attrib_state.set_format(nvn::Format::Rgb32, 0);
        attrib_state.set_stream_index(0);

        let mut stream_state = nvn::VertexStreamState::zeroed();
        stream_state.set_defaults();
        stream_state.set_stride(std::mem::size_of::<glam::Vec3>() as isize);

        let mut blend_state = nvn::BlendState::zeroed();
        let mut color_state = nvn::ColorState::zeroed();
        let mut channel_state = nvn::ChannelMaskState::zeroed();
        let mut multisample_state = nvn::MultisampleState::zeroed();
        let mut polygon_state = nvn::PolygonState::zeroed();
        let mut depth_stencil_state = nvn::DepthStencilState::zeroed();

        blend_state.set_defaults();
        blend_state.set_blend_target(0);
        blend_state.set_blend_func(5, 6, 5, 6);
        blend_state.set_blend_equation(1, 1);

        color_state.set_defaults();
        color_state.set_blend_enable(0, true);
        color_state.set_logic_op(3);

        polygon_state.set_defaults();
        polygon_state.set_cull_face(0);
        polygon_state.set_front_face(1);
        polygon_state.set_polygon_mode(2);

        channel_state.set_defaults();
        multisample_state.set_defaults();
        depth_stencil_state.set_defaults();

        Self {
            program,
            attrib_state,
            stream_state,
            blend_state,
            color_state,
            channel_state,
            multisample_state,
            polygon_state,
            depth_stencil_state
        }
    }

    pub fn bind(&self, cmdbuf: &mut nvn::CommandBuffer) {
        cmdbuf.bind_program(self.program.get(), 0x1f);
        cmdbuf.bind_blend_state(&self.blend_state);
        cmdbuf.bind_color_state(&self.color_state);
        cmdbuf.bind_channel_mask_state(&self.channel_state);
        cmdbuf.bind_multisample_state(&self.multisample_state);
        cmdbuf.bind_polygon_state(&self.polygon_state);
        cmdbuf.bind_depth_stencil_state(&self.depth_stencil_state);
        cmdbuf.bind_vertex_attrib_state(1, &self.attrib_state);
        cmdbuf.bind_vertex_stream_state(1, &self.stream_state);
    }
}

#[repr(C)]
#[derive(Debug, Copy, Clone, Pod, Zeroable)]
pub struct TextureVertex {
    pub position: glam::Vec3A,
    pub texcoord: glam::Vec2,
    _padding: glam::Vec2,
}

impl TextureVertex {
    pub const TOP_LEFT: Self = Self { position: glam::Vec3A::new(-0.5, -0.5, 0.0), texcoord: glam::Vec2::ZERO, _padding: glam::Vec2::ZERO };
    pub const TOP_RIGHT: Self = Self { position: glam::Vec3A::new(0.5, -0.5, 0.0), texcoord: glam::Vec2::new(1.0, 0.0), _padding: glam::Vec2::ZERO };
    pub const BOTTOM_LEFT: Self = Self { position: glam::Vec3A::new(-0.5, 0.5, 0.0), texcoord: glam::Vec2::new(0.0, 1.0), _padding: glam::Vec2::ZERO };
    pub const BOTTOM_RIGHT: Self = Self { position: glam::Vec3A::new(0.5, 0.5, 0.0), texcoord: glam::Vec2::ONE, _padding: glam::Vec2::ZERO };
}

pub struct TexturePipeline {
    program: ManagedProgram,
    attrib_state: [VertexAttribState; 3],
    stream_state: VertexStreamState,
    blend_state: nvn::BlendState,
    color_state: nvn::ColorState,
    channel_state: nvn::ChannelMaskState,
    multisample_state: nvn::MultisampleState,
    polygon_state: nvn::PolygonState,
    depth_stencil_state: nvn::DepthStencilState,
}

impl TexturePipeline {
    pub fn new(device: &nvn::Device) -> Self {
        let (vctrl, vcode) = get_shaders(StaticShaderData::SystemDebugDrawTexture2DVS);
        let (fctrl, fcode) = get_shaders(StaticShaderData::SystemDebugDrawTexture2DPS);

        let program = ManagedProgram::new(device, vctrl, vcode, fctrl, fcode);
        let mut attrib_state = [nvn::VertexAttribState::zeroed(), nvn::VertexAttribState::zeroed(), nvn::VertexAttribState::zeroed()];
        attrib_state[0].set_defaults();
        attrib_state[0].set_format(nvn::Format::Rgb32, 0);
        attrib_state[0].set_stream_index(0);
        attrib_state[1].set_defaults();
        attrib_state[2].set_defaults();
        attrib_state[2].set_format(nvn::Format::Rg32, std::mem::size_of::<glam::Vec3A>() as isize);
        attrib_state[2].set_stream_index(0);

        let mut stream_state = nvn::VertexStreamState::zeroed();
        stream_state.set_defaults();
        stream_state.set_stride(std::mem::size_of::<TextureVertex>() as isize);

        let mut blend_state = nvn::BlendState::zeroed();
        let mut color_state = nvn::ColorState::zeroed();
        let mut channel_state = nvn::ChannelMaskState::zeroed();
        let mut multisample_state = nvn::MultisampleState::zeroed();
        let mut polygon_state = nvn::PolygonState::zeroed();
        let mut depth_stencil_state = nvn::DepthStencilState::zeroed();

        blend_state.set_defaults();
        blend_state.set_blend_target(0);
        blend_state.set_blend_func(5, 6, 5, 6);
        blend_state.set_blend_equation(1, 1);

        color_state.set_defaults();
        color_state.set_blend_enable(0, true);
        color_state.set_logic_op(3);

        polygon_state.set_defaults();
        polygon_state.set_cull_face(0);
        polygon_state.set_front_face(1);
        polygon_state.set_polygon_mode(2);

        channel_state.set_defaults();
        multisample_state.set_defaults();
        depth_stencil_state.set_defaults();

        Self {
            program,
            attrib_state,
            stream_state,
            blend_state,
            color_state,
            channel_state,
            multisample_state,
            polygon_state,
            depth_stencil_state
        }
    }

    pub fn bind(&self, cmdbuf: &mut nvn::CommandBuffer) {
        cmdbuf.bind_program(self.program.get(), 0x1f);
        cmdbuf.bind_blend_state(&self.blend_state);
        cmdbuf.bind_color_state(&self.color_state);
        cmdbuf.bind_channel_mask_state(&self.channel_state);
        cmdbuf.bind_multisample_state(&self.multisample_state);
        cmdbuf.bind_polygon_state(&self.polygon_state);
        cmdbuf.bind_depth_stencil_state(&self.depth_stencil_state);
        cmdbuf.bind_vertex_attrib_state(3, self.attrib_state.as_ptr());
        cmdbuf.bind_vertex_stream_state(1, &self.stream_state);
    }
}

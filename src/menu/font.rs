use std::{ops::Range, sync::Arc};

use cosmic_text::{
    fontdb::{Database, FaceInfo, Source},
    Align, CacheKey, Command, Family, FontSystem, Metrics, SwashCache,
};
use envy::{PreparedGlyph, TextAlignment, TextLayoutArgs};
use glam::{Mat4, Vec3, Vec4};
use indexmap::IndexMap;
use lyon::{
    geom::point,
    path::FillRule,
    tessellation::{
        FillGeometryBuilder, FillOptions, FillTessellator, FillVertex, GeometryBuilder,
        GeometryBuilderError, StrokeGeometryBuilder, StrokeOptions, StrokeTessellator, VertexId,
    },
};

use crate::{
    menu::{
        envy::{NvnBackend, UniformHandle},
        shaders::PerDrawCBuffer,
    },
    nvn::{
        self,
        abstraction::{BufferVec, StagedBuffer},
    },
};

#[repr(transparent)]
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct FontHandle(usize);

#[repr(transparent)]
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct GlyphHandle(usize);

pub struct FontStage<'a> {
    vertices: StagedBuffer<'a>,
    indices: StagedBuffer<'a>,
}

impl FontStage<'_> {
    pub fn exec(self) {
        self.vertices.execute();
        self.indices.execute();
    }
}

struct GlyphIndices {
    fill: Range<u32>,
    outline: Option<Range<u32>>,
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
struct OutlineCacheKey {
    inner: CacheKey,
    outline: u32,
}

pub struct FontManager {
    system: FontSystem,
    swash: SwashCache,
    glyphs: IndexMap<OutlineCacheKey, GlyphIndices>,
    vertices: BufferVec<Vec3>,
    indices: BufferVec<i32>,
    fonts: IndexMap<String, FaceInfo>,
}

impl FontManager {
    pub fn new(device: &nvn::Device) -> Self {
        Self {
            system: FontSystem::new_with_locale_and_db("".to_string(), Database::new()),
            swash: SwashCache::new(),
            glyphs: IndexMap::new(),
            vertices: BufferVec::new(device),
            indices: BufferVec::new(device),
            fonts: IndexMap::new(),
        }
    }

    pub fn stage(&mut self, device: &nvn::Device) -> FontStage<'_> {
        FontStage {
            vertices: self.vertices.stage(device),
            indices: self.indices.stage(device),
        }
    }

    pub fn get_handle(&self, name: &str) -> Option<FontHandle> {
        self.fonts.get_index_of(name).map(FontHandle)
    }

    pub fn add_font(&mut self, name: String, bytes: Vec<u8>) {
        let ids = self
            .system
            .db_mut()
            .load_font_source(Source::Binary(Arc::new(bytes)));
        let face = self.system.db().face(ids[0]).unwrap();
        self.fonts.insert(name, face.clone());
    }

    fn prepare_glyph(
        &mut self,
        key: OutlineCacheKey,
        width: f32,
        height: f32,
        outline: f32,
    ) -> GlyphHandle {
        if let Some((idx, _, _)) = self.glyphs.get_full(&key) {
            return GlyphHandle(idx);
        }

        let commands = self
            .swash
            .get_outline_commands(&mut self.system, key.inner)
            .unwrap();

        let mut builder = lyon::path::Path::builder().with_svg();

        let mut is_open = false;

        let center_x = width / 2.0;
        let center_y = height / 2.0;
        let norm_point = |x: f32, y: f32| point(x - center_x, y - center_y);

        for command in commands.iter() {
            match command {
                Command::MoveTo(p) => {
                    if is_open {
                        builder.close();
                    }
                    is_open = true;

                    builder.move_to(norm_point(p.x, -p.y));
                }
                Command::Close => {
                    if is_open {
                        builder.close();
                    }
                    is_open = false;
                }
                Command::LineTo(p) => {
                    is_open = true;
                    builder.line_to(norm_point(p.x, -p.y));
                }
                Command::QuadTo(ctrl, p) => {
                    is_open = true;
                    builder.quadratic_bezier_to(norm_point(ctrl.x, -ctrl.y), norm_point(p.x, -p.y));
                }
                Command::CurveTo(ctrl_a, ctrl_b, p) => {
                    is_open = true;
                    builder.cubic_bezier_to(
                        norm_point(ctrl_a.x, -ctrl_a.y),
                        norm_point(ctrl_b.x, -ctrl_b.y),
                        norm_point(p.x, -p.y),
                    );
                }
            }
        }

        let path = builder.build();
        let start = self.indices.len() as u32;
        let mut fill_tesselator = FillTessellator::new();
        let mut builder = InPlaceBufferBuilders {
            vertex_start: self.vertices.len(),
            index_start: self.indices.len(),
            vertex_buffer: &mut self.vertices,
            index_buffer: &mut self.indices,
        };
        fill_tesselator
            .tessellate_path(
                &path,
                &FillOptions::tolerance(0.02).with_fill_rule(FillRule::NonZero),
                &mut builder,
            )
            .unwrap();

        let index = self.glyphs.len();

        let fill_indices = start..self.indices.len() as u32;

        let outline_indices = if outline > 0.0 {
            let start = self.indices.len() as u32;
            let mut builder = InPlaceStrokeBufferBuilders {
                vertex_start: self.vertices.len(),
                index_start: self.indices.len(),
                vertex_buffer: &mut self.vertices,
                index_buffer: &mut self.indices,
                outline,
            };
            let mut stroke_tesselator = StrokeTessellator::new();
            stroke_tesselator
                .tessellate_path(&path, &StrokeOptions::tolerance(0.02), &mut builder)
                .unwrap();
            Some(start..self.indices.len() as u32)
        } else {
            None
        };
        self.glyphs.insert(
            key,
            GlyphIndices {
                fill: fill_indices,
                outline: outline_indices,
            },
        );

        GlyphHandle(index)
    }

    pub fn layout(
        &mut self,
        mut new_uniform: impl FnMut() -> UniformHandle,
        args: TextLayoutArgs<'_, NvnBackend>,
    ) -> Vec<PreparedGlyph<NvnBackend>> {
        let face = &self.fonts[args.handle.0];

        let metrics = Metrics::new(args.font_size, args.line_height);
        let mut buffer = cosmic_text::Buffer::new(&mut self.system, metrics);
        let mut buffer = buffer.borrow_with(&mut self.system);
        buffer.set_size(Some(args.buffer_size.x), Some(args.buffer_size.y));
        buffer.set_rich_text(
            [(
                args.text,
                cosmic_text::Attrs {
                    family: Family::Name(&face.families[0].0),
                    stretch: face.stretch,
                    style: face.style,
                    weight: face.weight,
                    ..cosmic_text::Attrs::new()
                },
            )],
            &cosmic_text::Attrs {
                family: Family::Name(&face.families[0].0),
                stretch: face.stretch,
                style: face.style,
                weight: face.weight,
                ..cosmic_text::Attrs::new()
            },
            cosmic_text::Shaping::Basic,
            Some(match args.alignment {
                TextAlignment::Left => Align::Left,
                TextAlignment::Center => Align::Center,
                TextAlignment::Right => Align::Right,
                TextAlignment::Justify => Align::Justified,
                TextAlignment::End => Align::End,
            }),
        );
        // buffer.set_text(
        //     args.text.as_ref(),
        //     &cosmic_text::Attrs {
        //         family: Family::Name(&face.families[0].0),
        //         stretch: face.stretch,
        //         style: face.style,
        //         weight: face.weight,
        //         ..cosmic_text::Attrs::new()
        //     },
        //     cosmic_text::Shaping::Basic,
        // );

        let mut glyphs = vec![];

        for run in buffer.layout_runs() {
            for glyph in run.glyphs.iter() {
                glyphs.push((
                    OutlineCacheKey {
                        inner: CacheKey::new(
                            glyph.font_id,
                            glyph.glyph_id,
                            glyph.font_size,
                            (0.0, 0.0),
                            glyph.cache_key_flags,
                        )
                        .0,
                        outline: args.outline_thickness.to_bits(),
                    },
                    glyph.w,
                    run.line_height,
                    glyph.x + glyph.x_offset * glyph.font_size,
                    glyph.y + glyph.y_offset * glyph.font_size + run.line_y,
                ));
            }
        }

        let mut prepared_glyphs = vec![];
        for (key, w, h, x, y) in glyphs {
            let handle = self.prepare_glyph(key, w, h, args.outline_thickness);
            prepared_glyphs.push(PreparedGlyph {
                glyph_handle: handle,
                uniform_handle: new_uniform(),
                outline_uniform_handle: (args.outline_thickness > 0.0).then(|| new_uniform()),
                offset_in_buffer: glam::Vec2::new(x, y),
                size: glam::Vec2::new(w, h),
            });
        }

        prepared_glyphs
    }

    pub fn bind_vertex_buffer(&self, idx: i32, cmdbuf: &mut nvn::CommandBuffer) {
        cmdbuf.bind_vertex_buffer(
            idx,
            self.vertices.buffer().get_address(),
            self.vertices.len() * std::mem::size_of::<Vec3>(),
        );
    }

    pub fn draw_glyph(&self, handle: GlyphHandle, cmdbuf: &mut nvn::CommandBuffer) {
        let indices = self.glyphs[handle.0].fill.clone();
        cmdbuf.draw_elements(
            4,
            2,
            indices.end as i32 - indices.start as i32,
            self.indices.address_for_element(indices.start as usize),
        );
    }

    pub fn draw_glyph_outline(&self, handle: GlyphHandle, cmdbuf: &mut nvn::CommandBuffer) {
        let indices = self.glyphs[handle.0].outline.clone().unwrap();
        cmdbuf.draw_elements(
            4,
            2,
            indices.end as i32 - indices.start as i32,
            self.indices.address_for_element(indices.start as usize),
        );
    }
}

struct InPlaceBufferBuilders<'a> {
    vertex_buffer: &'a mut BufferVec<Vec3>,
    index_buffer: &'a mut BufferVec<i32>,
    vertex_start: usize,
    index_start: usize,
}

struct InPlaceStrokeBufferBuilders<'a> {
    vertex_buffer: &'a mut BufferVec<Vec3>,
    index_buffer: &'a mut BufferVec<i32>,
    vertex_start: usize,
    index_start: usize,
    outline: f32,
}

impl GeometryBuilder for InPlaceBufferBuilders<'_> {
    fn begin_geometry(&mut self) {
        self.vertex_start = self.vertex_buffer.len();
        self.index_start = self.index_buffer.len();
    }
    fn add_triangle(&mut self, a: VertexId, b: VertexId, c: VertexId) {
        debug_assert!(a != b);
        debug_assert!(a != c);
        debug_assert!(b != c);
        debug_assert!(a != VertexId::INVALID);
        debug_assert!(b != VertexId::INVALID);
        debug_assert!(c != VertexId::INVALID);

        self.index_buffer
            .extend([a, b, c].map(|vertex| u32::from(vertex) as i32));
    }

    fn abort_geometry(&mut self) {
        self.vertex_buffer.truncate(self.vertex_start);
        self.index_buffer.truncate(self.index_start);
    }
}

impl GeometryBuilder for InPlaceStrokeBufferBuilders<'_> {
    fn begin_geometry(&mut self) {
        self.vertex_start = self.vertex_buffer.len();
        self.index_start = self.index_buffer.len();
    }
    fn add_triangle(&mut self, a: VertexId, b: VertexId, c: VertexId) {
        debug_assert!(a != b);
        debug_assert!(a != c);
        debug_assert!(b != c);
        debug_assert!(a != VertexId::INVALID);
        debug_assert!(b != VertexId::INVALID);
        debug_assert!(c != VertexId::INVALID);

        self.index_buffer
            .extend([a, b, c].map(|vertex| u32::from(vertex) as i32));
    }

    fn abort_geometry(&mut self) {
        self.vertex_buffer.truncate(self.vertex_start);
        self.index_buffer.truncate(self.index_start);
    }
}

impl FillGeometryBuilder for InPlaceBufferBuilders<'_> {
    fn add_fill_vertex(&mut self, vertex: FillVertex) -> Result<VertexId, GeometryBuilderError> {
        let length = self.vertex_buffer.len();
        if length >= u32::MAX as usize {
            return Err(GeometryBuilderError::TooManyVertices);
        }
        self.vertex_buffer
            .push(Vec3::from(vertex.position().to_3d().to_array()));

        Ok(VertexId(length as u32))
    }
}

impl StrokeGeometryBuilder for InPlaceStrokeBufferBuilders<'_> {
    fn add_stroke_vertex(
        &mut self,
        vertex: lyon::tessellation::StrokeVertex,
    ) -> Result<VertexId, GeometryBuilderError> {
        let length = self.vertex_buffer.len();
        if length >= u32::MAX as usize {
            return Err(GeometryBuilderError::TooManyVertices);
        }
        self.vertex_buffer.push(Vec3::from(
            (vertex.position().to_3d() + vertex.normal().to_3d() * self.outline).to_array(),
        ));

        Ok(VertexId(length as u32))
    }
}

//! What the scenes hold on the GPU: made in one context, owned, counted
//! against a budget, and given back to that context or not at all.

use std::cell::{Cell, RefCell};
use std::ffi::{CString, c_void};
use std::rc::Rc;

use glow::{Context, HasContext, NativeFramebuffer, NativeProgram, NativeTexture};

/// Why something is not drawn on the GPU.
#[derive(Clone, Debug, PartialEq)]
pub enum Error {
    /// No context, or not the one everything here was made in.
    Context,
    /// A shader or a program: the driver will not have it, however often.
    Shader(String),
    /// A texture, a buffer or a framebuffer the driver did not give.
    Resource(String),
    /// GL's own error after drawing.
    Draw(u32),
    /// More than is given to scenes (`Budget`).
    Budget(&'static str),
}

impl Error {
    /// The same again until the context is another, so not tried again.
    pub fn lasts(&self) -> bool {
        matches!(self, Self::Shader(_))
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Context => f.write_str("no GL context"),
            Self::Shader(what) => write!(f, "shader: {what}"),
            Self::Resource(what) => write!(f, "resource: {what}"),
            Self::Draw(code) => write!(f, "GL error {code:#x}"),
            Self::Budget(what) => write!(f, "over budget: {what}"),
        }
    }
}

/// A failure made to happen, where the real one would: for the tests, and for
/// a laboratory build on a television.
#[cfg(any(test, feature = "spatial-lab"))]
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Fault {
    Shader,
    Buffer,
    Texture,
    Framebuffer,
    Draw,
    Context,
}

#[cfg(any(test, feature = "spatial-lab"))]
thread_local! {
    pub static FAULT: Cell<Option<Fault>> = const { Cell::new(None) };
}

/// Whether the failure `$kind` is to happen here; never in the product.
macro_rules! fault {
    ($kind:ident) => {{
        #[cfg(any(test, feature = "spatial-lab"))]
        let now = $crate::spatial::gl::FAULT.get() == Some($crate::spatial::gl::Fault::$kind);
        #[cfg(not(any(test, feature = "spatial-lab")))]
        let now = false;
        now
    }};
}

/// What one frame asked of the GPU.
#[derive(Clone, Copy, Default, Debug)]
pub struct Drawn {
    pub passes: u32,
    pub draws: u32,
    pub instances: u64,
    pub vertices: u64,
    pub triangles: u64,
}

/// Which context: its address, and how many had been made by then -- an
/// address is used again.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ContextId {
    address: usize,
    made: u64,
}

thread_local! {
    static MADE: Cell<u64> = const { Cell::new(0) };
}

/// The platform has made a context: what was made in one before it is not
/// this one's, whatever its address.
pub fn context_made() {
    MADE.set(MADE.get() + 1);
}

/// The context current on this thread.
pub fn current() -> Option<ContextId> {
    #[link(name = "EGL")]
    unsafe extern "C" {
        fn eglGetCurrentContext() -> *const c_void;
    }
    if fault!(Context) {
        return None;
    }
    // SAFETY: reads this thread's state.
    let address = unsafe { eglGetCurrentContext() } as usize;
    (address != 0).then(|| ContextId { address, made: MADE.get() })
}

fn load(name: &str) -> *const c_void {
    #[link(name = "EGL")]
    unsafe extern "C" {
        fn eglGetProcAddress(name: *const libc::c_char) -> *const c_void;
    }
    let name = CString::new(name).expect("a GL function's name");
    // SAFETY: a C string that outlives the call.
    let real = unsafe { eglGetProcAddress(name.as_ptr()) };
    #[cfg(feature = "gl-count")]
    let real = crate::glcount::wrap(&name, real);
    real
}

type TextureMultisample = unsafe extern "C" fn(u32, u32, u32, u32, i32, i32);
type StorageMultisample = unsafe extern "C" fn(u32, i32, u32, i32, i32);

/// GL_EXT_multisampled_render_to_texture: samples that live in the tile and
/// are resolved as it is written out. No multisampled memory, no resolve pass.
struct InTile {
    texture: TextureMultisample,
    storage: StorageMultisample,
}

/// One context's GL, and what is held in it.
pub struct Gpu {
    pub gl: Context,
    id: ContextId,
    in_tile: Option<InTile>,
    /// The pixel under the shader can be read.
    pub fetch: bool,
    /// Bytes of textures and buffers, and the most there may be.
    held: Cell<u64>,
    most: u64,
    objects: Cell<u32>,
}

impl Gpu {
    pub fn new(most: u64) -> Result<Rc<Self>, Error> {
        let id = current().ok_or(Error::Context)?;
        // SAFETY: the loader is EGL's, the context current on this thread.
        let gl = unsafe { Context::from_loader_function_cstr(|name| load(name.to_str().expect("a GL function's name"))) };
        // Instancing, vertex arrays and `texelFetch`.
        if gl.version().major < 3 {
            return Err(Error::Resource(format!("GLES {}.{}", gl.version().major, gl.version().minor)));
        }
        let has = |name: &str| gl.supported_extensions().contains(name);
        let in_tile = has("GL_EXT_multisampled_render_to_texture").then(|| {
            let (texture, storage) = (load("glFramebufferTexture2DMultisampleEXT"), load("glRenderbufferStorageMultisampleEXT"));
            // SAFETY: the extension's own two functions, by their names.
            (!texture.is_null() && !storage.is_null()).then(|| unsafe {
                InTile {
                    texture: std::mem::transmute::<*const c_void, TextureMultisample>(texture),
                    storage: std::mem::transmute::<*const c_void, StorageMultisample>(storage),
                }
            })
        });
        let fetch = has("GL_EXT_shader_framebuffer_fetch");
        Ok(Rc::new(Self {
            gl,
            id,
            in_tile: in_tile.flatten(),
            fetch,
            held: Cell::new(0),
            most,
            objects: Cell::new(0),
        }))
    }

    /// Whether this is the context of what is made here.
    pub fn is_current(&self) -> bool {
        current() == Some(self.id)
    }

    pub fn held(&self) -> u64 {
        self.held.get()
    }

    /// How many things are held, for the tests.
    #[cfg(test)]
    pub fn objects(&self) -> u32 {
        self.objects.get()
    }

    pub fn room_for(&self, bytes: u64) -> bool {
        self.held.get() + bytes <= self.most
    }

    /// What GL has to say of everything since it was last asked.
    fn complaint(&self) -> Option<u32> {
        let mut first = None;
        // A lost context answers for ever.
        for _ in 0..8 {
            // SAFETY: the context is current, as everywhere here.
            match unsafe { self.gl.get_error() } {
                glow::NO_ERROR => break,
                code => first = first.or(Some(code)),
            }
        }
        first
    }

    /// Forgets what others were told off for.
    pub fn listen(&self) {
        self.complaint();
    }

    pub fn well(&self) -> Result<(), Error> {
        if fault!(Draw) {
            // SAFETY: the same; an enum GL does not have.
            unsafe { self.gl.enable(0xFFFF) };
        }
        self.complaint().map_or(Ok(()), |code| Err(Error::Draw(code)))
    }

    fn given(&self, what: impl FnOnce() -> String) -> Result<(), Error> {
        self.complaint().map_or(Ok(()), |code| Err(Error::Resource(format!("{}: {code:#x}", what()))))
    }

    fn own<T: Copy>(self: &Rc<Self>, name: T, bytes: u64, delete: unsafe fn(&Context, T)) -> Owned<T> {
        self.held.set(self.held.get() + bytes);
        self.objects.set(self.objects.get() + 1);
        Owned { gpu: self.clone(), name, bytes, delete }
    }
}

/// Something on the GPU. Dropped, it is deleted -- in the context it was made
/// in: elsewhere its name is another thing's, and it is only forgotten (a
/// context takes what it held with it).
pub struct Owned<T: Copy> {
    gpu: Rc<Gpu>,
    pub name: T,
    bytes: u64,
    delete: unsafe fn(&Context, T),
}

impl<T: Copy> Drop for Owned<T> {
    fn drop(&mut self) {
        self.gpu.held.set(self.gpu.held.get() - self.bytes);
        self.gpu.objects.set(self.gpu.objects.get() - 1);
        if self.gpu.is_current() {
            // SAFETY: made in this context, and not deleted before.
            unsafe { (self.delete)(&self.gpu.gl, self.name) };
        }
    }
}

pub fn program(gpu: &Rc<Gpu>, vertex: &str, fragment: &str) -> Result<Owned<NativeProgram>, Error> {
    let gl = &gpu.gl;
    // SAFETY: the context is current on this thread, as everywhere here.
    unsafe {
        let program = gpu.own(gl.create_program().map_err(Error::Resource)?, 0, Context::delete_program);
        for (kind, source) in [(glow::VERTEX_SHADER, vertex), (glow::FRAGMENT_SHADER, fragment)] {
            let shader = gpu.own(gl.create_shader(kind).map_err(Error::Resource)?, 0, Context::delete_shader);
            if fault!(Shader) {
                gl.shader_source(shader.name, "#version 300 es\nnot a shader");
            } else {
                gl.shader_source(shader.name, source);
            }
            gl.compile_shader(shader.name);
            if !gl.get_shader_compile_status(shader.name) {
                return Err(Error::Shader(gl.get_shader_info_log(shader.name)));
            }
            // Alive while attached, whoever lets go of it.
            gl.attach_shader(program.name, shader.name);
        }
        gl.link_program(program.name);
        if !gl.get_program_link_status(program.name) {
            return Err(Error::Shader(gl.get_program_info_log(program.name)));
        }
        Ok(program)
    }
}

/// A mesh kept on the GPU, with room for per-instance data of its own. Made
/// once; a frame binds it and draws.
pub struct Mesh {
    vao: Owned<glow::NativeVertexArray>,
    _buffers: Vec<Owned<glow::NativeBuffer>>,
    indices: i32,
    pub instances: i32,
    mode: u32,
}

/// Per-vertex attributes come first (`vertex`: sizes in floats), per-instance
/// ones after them.
pub struct MeshData<'a> {
    pub vertices: &'a [f32],
    pub vertex: &'a [i32],
    pub indices: &'a [u32],
    pub instances: &'a [f32],
    pub instance: &'a [i32],
    pub mode: u32,
}

fn bytes<T: Copy>(data: &[T]) -> &[u8] {
    // SAFETY: plain numbers, read as bytes.
    unsafe { std::slice::from_raw_parts(data.as_ptr().cast(), std::mem::size_of_val(data)) }
}

impl Mesh {
    pub fn new(gpu: &Rc<Gpu>, data: &MeshData<'_>) -> Result<Self, Error> {
        let gl = &gpu.gl;
        let buffer = |target: u32, data: &[u8]| {
            if !gpu.room_for(data.len() as u64) {
                return Err(Error::Budget("geometry"));
            }
            if fault!(Buffer) {
                return Err(Error::Resource("buffer".into()));
            }
            // SAFETY: see `program`.
            unsafe {
                let made = gpu.own(gl.create_buffer().map_err(Error::Resource)?, data.len() as u64, Context::delete_buffer);
                gl.bind_buffer(target, Some(made.name));
                gl.buffer_data_u8_slice(target, data, glow::STATIC_DRAW);
                gpu.given(|| format!("buffer of {}", data.len()))?;
                Ok(made)
            }
        };
        gpu.listen();
        // SAFETY: see `program`; the vertex array keeps every binding made
        // here to itself.
        unsafe {
            let vao = gpu.own(gl.create_vertex_array().map_err(Error::Resource)?, 0, Context::delete_vertex_array);
            gl.bind_vertex_array(Some(vao.name));
            let mut buffers = Vec::new();
            let mut at = 0;
            for (values, sizes, divisor) in [(data.vertices, data.vertex, 0), (data.instances, data.instance, 1)] {
                if values.is_empty() {
                    continue;
                }
                buffers.push(buffer(glow::ARRAY_BUFFER, bytes(values))?);
                let stride = sizes.iter().sum::<i32>() * 4;
                let mut offset = 0;
                for &size in sizes {
                    gl.enable_vertex_attrib_array(at);
                    gl.vertex_attrib_pointer_f32(at, size, glow::FLOAT, false, stride, offset);
                    gl.vertex_attrib_divisor(at, divisor);
                    offset += size * 4;
                    at += 1;
                }
            }
            buffers.push(buffer(glow::ELEMENT_ARRAY_BUFFER, bytes(data.indices))?);
            gl.bind_vertex_array(None);
            let per = data.instance.iter().sum::<i32>().max(1) as usize;
            Ok(Self {
                vao,
                _buffers: buffers,
                indices: data.indices.len() as i32,
                instances: if data.instances.is_empty() { 1 } else { (data.instances.len() / per) as i32 },
                mode: data.mode,
            })
        }
    }

    /// What drawing it once costs.
    pub fn triangles(&self) -> u64 {
        self.instances as u64
            * if self.mode == glow::TRIANGLES { self.indices as u64 / 3 } else { self.indices.max(2) as u64 - 2 }
    }

    pub fn draw(&self, gl: &Context, drawn: &mut Drawn) {
        // SAFETY: see `program`.
        unsafe {
            gl.bind_vertex_array(Some(self.vao.name));
            gl.draw_elements_instanced(self.mode, self.indices, glow::UNSIGNED_INT, 0, self.instances);
        }
        drawn.draws += 1;
        drawn.instances += self.instances as u64;
        drawn.vertices += self.indices as u64 * self.instances as u64;
        drawn.triangles += self.triangles();
    }
}

/// A texture to draw into.
pub struct Target {
    pub texture: Owned<NativeTexture>,
    fbo: Owned<NativeFramebuffer>,
    depth: Option<Owned<glow::NativeRenderbuffer>>,
    pub size: (i32, i32),
    /// As asked for; 1 is what there is without `InTile`.
    pub samples: i32,
}

impl Target {
    /// Colour, and depth: the samples of both are the tile's.
    pub fn bytes(size: (i32, i32), depth: bool) -> u64 {
        size.0 as u64 * size.1 as u64 * if depth { 8 } else { 4 }
    }

    pub fn new(gpu: &Rc<Gpu>, size: (i32, i32), samples: i32, depth: bool) -> Result<Self, Error> {
        let one = Self::bytes(size, false);
        if !gpu.room_for(Self::bytes(size, depth)) {
            return Err(Error::Budget("memory"));
        }
        let gl = &gpu.gl;
        gpu.listen();
        // SAFETY: see `program`.
        unsafe {
            let texture = gpu.own(gl.create_texture().map_err(Error::Resource)?, one, Context::delete_texture);
            gl.active_texture(glow::TEXTURE0);
            gl.bind_texture(glow::TEXTURE_2D, Some(texture.name));
            let asked = if fault!(Texture) { (1 << 24, 1 << 24) } else { size };
            gl.tex_storage_2d(glow::TEXTURE_2D, 1, glow::RGBA8, asked.0, asked.1);
            gpu.given(|| format!("texture {asked:?}"))?;
            for (name, value) in [
                (glow::TEXTURE_MIN_FILTER, glow::NEAREST),
                (glow::TEXTURE_MAG_FILTER, glow::NEAREST),
                (glow::TEXTURE_WRAP_S, glow::CLAMP_TO_EDGE),
                (glow::TEXTURE_WRAP_T, glow::CLAMP_TO_EDGE),
            ] {
                gl.tex_parameter_i32(glow::TEXTURE_2D, name, value as i32);
            }
            let fbo = gpu.own(gl.create_framebuffer().map_err(Error::Resource)?, 0, Context::delete_framebuffer);
            gl.bind_framebuffer(glow::FRAMEBUFFER, Some(fbo.name));
            let in_tile = gpu.in_tile.as_ref().filter(|_| samples > 1);
            match in_tile {
                _ if fault!(Framebuffer) => {}
                Some(ext) => (ext.texture)(
                    glow::FRAMEBUFFER,
                    glow::COLOR_ATTACHMENT0,
                    glow::TEXTURE_2D,
                    texture.name.0.get(),
                    0,
                    samples,
                ),
                None => gl.framebuffer_texture_2d(
                    glow::FRAMEBUFFER,
                    glow::COLOR_ATTACHMENT0,
                    glow::TEXTURE_2D,
                    Some(texture.name),
                    0,
                ),
            }
            let depth = if depth {
                let buffer = gpu.own(gl.create_renderbuffer().map_err(Error::Resource)?, one, Context::delete_renderbuffer);
                gl.bind_renderbuffer(glow::RENDERBUFFER, Some(buffer.name));
                match in_tile {
                    Some(ext) => {
                        (ext.storage)(glow::RENDERBUFFER, samples, glow::DEPTH_COMPONENT24, size.0, size.1);
                    }
                    None => gl.renderbuffer_storage(glow::RENDERBUFFER, glow::DEPTH_COMPONENT24, size.0, size.1),
                }
                gl.framebuffer_renderbuffer(
                    glow::FRAMEBUFFER,
                    glow::DEPTH_ATTACHMENT,
                    glow::RENDERBUFFER,
                    Some(buffer.name),
                );
                Some(buffer)
            } else {
                None
            };
            gpu.given(|| format!("framebuffer {size:?} x{samples}"))?;
            let status = gl.check_framebuffer_status(glow::FRAMEBUFFER);
            if status != glow::FRAMEBUFFER_COMPLETE {
                return Err(Error::Resource(format!("framebuffer {size:?} x{samples}: {status:#x}")));
            }
            Ok(Self { texture, fbo, depth, size, samples })
        }
    }

    pub fn has_depth(&self) -> bool {
        self.depth.is_some()
    }

    pub fn holds(&self, size: (i32, i32)) -> bool {
        self.size.0 >= size.0 && self.size.1 >= size.1
    }

    /// Starts the pass: the part of the texture `used`, cleared, with the
    /// state a scene may count on. What was in it is not loaded into the
    /// tiles.
    pub fn begin(&self, gl: &Context, used: (i32, i32)) {
        // SAFETY: see `program`.
        unsafe {
            gl.bind_framebuffer(glow::FRAMEBUFFER, Some(self.fbo.name));
            gl.invalidate_framebuffer(
                glow::FRAMEBUFFER,
                if self.depth.is_some() { &[glow::COLOR_ATTACHMENT0, glow::DEPTH_ATTACHMENT] } else { &[glow::COLOR_ATTACHMENT0] },
            );
            gl.viewport(0, 0, used.0, used.1);
            for off in [glow::SCISSOR_TEST, glow::STENCIL_TEST, glow::DEPTH_TEST, glow::CULL_FACE, glow::BLEND] {
                gl.disable(off);
            }
            gl.blend_equation(glow::FUNC_ADD);
            gl.color_mask(true, true, true, true);
            gl.depth_mask(true);
            gl.clear_color(0.0, 0.0, 0.0, 0.0);
            gl.clear_depth_f32(1.0);
            gl.clear(glow::COLOR_BUFFER_BIT | if self.depth.is_some() { glow::DEPTH_BUFFER_BIT } else { 0 });
        }
    }

    /// Ends it: depth never leaves the tile.
    pub fn end(&self, gl: &Context) {
        if self.depth.is_some() {
            // SAFETY: see `program`.
            unsafe { gl.invalidate_framebuffer(glow::FRAMEBUFFER, &[glow::DEPTH_ATTACHMENT]) };
        }
    }
}

/// Everything of GL's state a scene may touch, as Slint's renderer left it;
/// put back when this is dropped, however the drawing ended. FemtoVG sets
/// most of it again for every flush, but not all -- it does not bind its
/// framebuffer, set the blend equation, the depth mask or the clear depth --
/// and what it does is not a promise.
pub struct Kept<'a> {
    gl: &'a Context,
    draw: Option<NativeFramebuffer>,
    read: Option<NativeFramebuffer>,
    renderbuffer: Option<glow::NativeRenderbuffer>,
    program: Option<NativeProgram>,
    vao: Option<glow::NativeVertexArray>,
    buffer: Option<glow::NativeBuffer>,
    unit: u32,
    texture: Option<NativeTexture>,
    viewport: [i32; 4],
    on: [(u32, bool); 5],
    blend: [u32; 6],
    depth: (u32, bool, f32),
    cull: (u32, u32),
    mask: [bool; 4],
    clear: [f32; 4],
}

impl<'a> Kept<'a> {
    pub fn of(gl: &'a Context) -> Self {
        // SAFETY: see `program`; state is read, and unit 0 made the active
        // one to read what it has bound.
        unsafe {
            let number = |name: u32| gl.get_parameter_i32(name) as u32;
            let unit = number(glow::ACTIVE_TEXTURE);
            gl.active_texture(glow::TEXTURE0);
            let mut viewport = [0; 4];
            gl.get_parameter_i32_slice(glow::VIEWPORT, &mut viewport);
            let mut clear = [0.0; 4];
            gl.get_parameter_f32_slice(glow::COLOR_CLEAR_VALUE, &mut clear);
            Self {
                gl,
                draw: gl.get_parameter_framebuffer(glow::DRAW_FRAMEBUFFER_BINDING),
                read: gl.get_parameter_framebuffer(glow::READ_FRAMEBUFFER_BINDING),
                renderbuffer: gl.get_parameter_renderbuffer(glow::RENDERBUFFER_BINDING),
                program: gl.get_parameter_program(glow::CURRENT_PROGRAM),
                vao: gl.get_parameter_vertex_array(glow::VERTEX_ARRAY_BINDING),
                buffer: gl.get_parameter_buffer(glow::ARRAY_BUFFER_BINDING),
                unit,
                texture: gl.get_parameter_texture(glow::TEXTURE_BINDING_2D),
                viewport,
                on: [glow::SCISSOR_TEST, glow::STENCIL_TEST, glow::DEPTH_TEST, glow::CULL_FACE, glow::BLEND]
                    .map(|name| (name, gl.is_enabled(name))),
                blend: [
                    glow::BLEND_SRC_RGB,
                    glow::BLEND_DST_RGB,
                    glow::BLEND_SRC_ALPHA,
                    glow::BLEND_DST_ALPHA,
                    glow::BLEND_EQUATION_RGB,
                    glow::BLEND_EQUATION_ALPHA,
                ]
                .map(number),
                depth: (
                    number(glow::DEPTH_FUNC),
                    gl.get_parameter_bool(glow::DEPTH_WRITEMASK),
                    gl.get_parameter_f32(glow::DEPTH_CLEAR_VALUE),
                ),
                cull: (number(glow::CULL_FACE_MODE), number(glow::FRONT_FACE)),
                mask: gl.get_parameter_bool_array(glow::COLOR_WRITEMASK),
                clear,
            }
        }
    }
}

impl Drop for Kept<'_> {
    fn drop(&mut self) {
        let gl = self.gl;
        // SAFETY: see `program`.
        unsafe {
            gl.bind_framebuffer(glow::DRAW_FRAMEBUFFER, self.draw);
            gl.bind_framebuffer(glow::READ_FRAMEBUFFER, self.read);
            gl.bind_renderbuffer(glow::RENDERBUFFER, self.renderbuffer);
            gl.use_program(self.program);
            gl.bind_vertex_array(self.vao);
            gl.bind_buffer(glow::ARRAY_BUFFER, self.buffer);
            gl.active_texture(glow::TEXTURE0);
            gl.bind_texture(glow::TEXTURE_2D, self.texture);
            gl.active_texture(self.unit);
            let [x, y, w, h] = self.viewport;
            gl.viewport(x, y, w, h);
            for (name, on) in self.on {
                if on {
                    gl.enable(name);
                } else {
                    gl.disable(name);
                }
            }
            let [src, dst, src_alpha, dst_alpha, rgb, alpha] = self.blend;
            gl.blend_func_separate(src, dst, src_alpha, dst_alpha);
            gl.blend_equation_separate(rgb, alpha);
            gl.depth_func(self.depth.0);
            gl.depth_mask(self.depth.1);
            gl.clear_depth_f32(self.depth.2);
            gl.cull_face(self.cull.0);
            gl.front_face(self.cull.1);
            let [r, g, b, a] = self.mask;
            gl.color_mask(r, g, b, a);
            let [r, g, b, a] = self.clear;
            gl.clear_color(r, g, b, a);
        }
    }
}

/// Straight alpha out of premultiplied: Slint takes a borrowed texture as
/// straight, and blending makes premultiplied. A pass of its own -- in the
/// tile the scene was drawn in it would run once per sample (2026-11-25:
/// 2.4 million cycles of the GPU's a frame more).
pub struct Straight {
    program: Owned<NativeProgram>,
    vao: Owned<glow::NativeVertexArray>,
}

const WHOLE: &str = r"#version 300 es
void main() {
    vec2 p = vec2(gl_VertexID == 1 ? 3.0 : -1.0, gl_VertexID == 2 ? 3.0 : -1.0);
    gl_Position = vec4(p, 0.0, 1.0);
}
";
const STRAIGHT: &str = r"#version 300 es
precision mediump float;
uniform sampler2D u_from;
out vec4 colour;
void main() {
    vec4 c = texelFetch(u_from, ivec2(gl_FragCoord.xy), 0);
    colour = c.a > 0.0 ? vec4(c.rgb / c.a, c.a) : vec4(0.0);
}
";

impl Straight {
    pub fn new(gpu: &Rc<Gpu>) -> Result<Self, Error> {
        let program = program(gpu, WHOLE, STRAIGHT)?;
        // SAFETY: see `program`.
        let vao = unsafe { gpu.gl.create_vertex_array() }.map_err(Error::Resource)?;
        Ok(Self { program, vao: gpu.own(vao, 0, Context::delete_vertex_array) })
    }

    /// The pass is begun.
    pub fn draw(&self, gl: &Context, from: &Target, drawn: &mut Drawn) {
        // SAFETY: see `program`.
        unsafe {
            gl.use_program(Some(self.program.name));
            gl.bind_vertex_array(Some(self.vao.name));
            gl.active_texture(glow::TEXTURE0);
            gl.bind_texture(glow::TEXTURE_2D, Some(from.texture.name));
            gl.draw_arrays(glow::TRIANGLES, 0, 3);
        }
        drawn.draws += 1;
        drawn.vertices += 3;
        drawn.triangles += 1;
    }
}

/// What every scene of a context shares: the target those with samples or a
/// depth buffer are drawn in, one after another, before each is put into its
/// own texture.
#[derive(Default)]
pub struct Scratch(RefCell<Vec<Target>>);

impl Scratch {
    /// One of this kind that holds `size`, grown to it if need be.
    pub fn with<T>(
        &self,
        gpu: &Rc<Gpu>,
        size: (i32, i32),
        (samples, depth): (i32, bool),
        draw: impl FnOnce(&Target) -> Result<T, Error>,
    ) -> Result<T, Error> {
        let mut all = self.0.borrow_mut();
        let same = |t: &Target| t.samples == samples && t.has_depth() == depth;
        if !all.iter().any(|t| same(t) && t.holds(size)) {
            let was = all.iter().position(same).map(|at| all.swap_remove(at).size).unwrap_or_default();
            all.push(Target::new(gpu, (size.0.max(was.0), size.1.max(was.1)), samples, depth)?);
        }
        draw(all.iter().find(|t| same(t)).ok_or(Error::Context)?)
    }

    pub fn clear(&self) {
        self.0.borrow_mut().clear();
    }
}

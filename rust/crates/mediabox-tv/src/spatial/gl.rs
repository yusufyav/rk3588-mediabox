//! What the scenes hold on the GPU, and the one pass they are drawn in.

use glow::{Context, HasContext, NativeFramebuffer, NativeProgram, NativeRenderbuffer, NativeTexture};
use std::ffi::{CString, c_void};

pub type Error = String;

/// What one frame asked of the GPU, for the report.
#[derive(Clone, Copy, Default)]
pub struct Drawn {
    pub passes: u32,
    pub draws: u32,
    pub instances: u64,
    pub vertices: u64,
    pub triangles: u64,
    pub objects: u64,
    pub pixels: u64,
    pub samples: i32,
}

pub fn program(gl: &Context, vertex: &str, fragment: &str) -> Result<NativeProgram, Error> {
    // SAFETY: the context is current on this thread, as everywhere here.
    unsafe {
        let program = gl.create_program()?;
        for (kind, source) in [(glow::VERTEX_SHADER, vertex), (glow::FRAGMENT_SHADER, fragment)] {
            let shader = gl.create_shader(kind)?;
            gl.shader_source(shader, source);
            gl.compile_shader(shader);
            if !gl.get_shader_compile_status(shader) {
                return Err(format!("shader: {}\n{source}", gl.get_shader_info_log(shader)));
            }
            gl.attach_shader(program, shader);
            gl.delete_shader(shader);
        }
        gl.link_program(program);
        if !gl.get_program_link_status(program) {
            return Err(format!("link: {}", gl.get_program_info_log(program)));
        }
        Ok(program)
    }
}

/// A mesh kept on the GPU, with room for per-instance data of its own. Made
/// once; a frame binds it and draws.
pub struct Mesh {
    vao: glow::NativeVertexArray,
    buffers: Vec<glow::NativeBuffer>,
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
    pub fn new(gl: &Context, data: &MeshData<'_>) -> Result<Self, Error> {
        // SAFETY: see `program`; the vertex array keeps every binding made
        // here to itself.
        unsafe {
            let vao = gl.create_vertex_array()?;
            gl.bind_vertex_array(Some(vao));
            let mut buffers = Vec::new();
            let mut at = 0;
            for (values, sizes, divisor) in [(data.vertices, data.vertex, 0), (data.instances, data.instance, 1)] {
                if values.is_empty() {
                    continue;
                }
                let buffer = gl.create_buffer()?;
                buffers.push(buffer);
                gl.bind_buffer(glow::ARRAY_BUFFER, Some(buffer));
                gl.buffer_data_u8_slice(glow::ARRAY_BUFFER, bytes(values), glow::STATIC_DRAW);
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
            let buffer = gl.create_buffer()?;
            buffers.push(buffer);
            gl.bind_buffer(glow::ELEMENT_ARRAY_BUFFER, Some(buffer));
            gl.buffer_data_u8_slice(glow::ELEMENT_ARRAY_BUFFER, bytes(data.indices), glow::STATIC_DRAW);
            gl.bind_vertex_array(None);
            gl.bind_buffer(glow::ARRAY_BUFFER, None);
            let per = data.instance.iter().sum::<i32>().max(1) as usize;
            Ok(Self {
                vao,
                buffers,
                indices: data.indices.len() as i32,
                instances: if data.instances.is_empty() { 1 } else { (data.instances.len() / per) as i32 },
                mode: data.mode,
            })
        }
    }

    pub fn draw(&self, gl: &Context, drawn: &mut Drawn) {
        // SAFETY: see `program`.
        unsafe {
            gl.bind_vertex_array(Some(self.vao));
            gl.draw_elements_instanced(self.mode, self.indices, glow::UNSIGNED_INT, 0, self.instances);
        }
        let n = self.instances as u64;
        drawn.draws += 1;
        drawn.instances += n;
        drawn.vertices += self.indices as u64 * n;
        drawn.triangles += n * if self.mode == glow::TRIANGLES { self.indices as u64 / 3 } else { self.indices.max(2) as u64 - 2 };
    }

    pub fn delete(self, gl: &Context) {
        // SAFETY: see `program`.
        unsafe {
            gl.delete_vertex_array(self.vao);
            for buffer in self.buffers {
                gl.delete_buffer(buffer);
            }
        }
    }
}

type TextureMultisample = unsafe extern "C" fn(u32, u32, u32, u32, i32, i32);
type StorageMultisample = unsafe extern "C" fn(u32, i32, u32, i32, i32);

/// GL_EXT_multisampled_render_to_texture: samples that live in the tile and
/// are resolved as it is written out. No multisampled memory, no resolve pass.
struct InTile {
    texture: TextureMultisample,
    storage: StorageMultisample,
}

/// What a tiled GPU offers that GLES does not have to.
pub struct Caps {
    in_tile: Option<InTile>,
    /// The pixel under the shader can be read: the picture is made straight
    /// alpha in the tile it was drawn in.
    pub fetch: bool,
}

pub fn load(name: &str) -> *const c_void {
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

/// The context current on this thread, as an identity.
pub fn current() -> usize {
    #[link(name = "EGL")]
    unsafe extern "C" {
        fn eglGetCurrentContext() -> *const c_void;
    }
    // SAFETY: reads this thread's state.
    unsafe { eglGetCurrentContext() as usize }
}

impl Caps {
    pub fn of(gl: &Context) -> Self {
        let has = |name: &str| gl.supported_extensions().contains(name);
        let in_tile = has("GL_EXT_multisampled_render_to_texture").then(|| {
            let (texture, storage) =
                (load("glFramebufferTexture2DMultisampleEXT"), load("glRenderbufferStorageMultisampleEXT"));
            // SAFETY: the extension's own two functions, by their names.
            (!texture.is_null() && !storage.is_null()).then(|| unsafe {
                InTile {
                    texture: std::mem::transmute::<*const c_void, TextureMultisample>(texture),
                    storage: std::mem::transmute::<*const c_void, StorageMultisample>(storage),
                }
            })
        });
        Self { in_tile: in_tile.flatten(), fetch: has("GL_EXT_shader_framebuffer_fetch") }
    }
}

/// The texture a scene is drawn into and Slint shows.
pub struct Target {
    pub texture: NativeTexture,
    fbo: NativeFramebuffer,
    depth: Option<NativeRenderbuffer>,
    pub size: (i32, i32),
    pub samples: i32,
}

impl Target {
    pub fn new(gl: &Context, caps: &Caps, size: (i32, i32), samples: i32, depth: bool) -> Result<Self, Error> {
        // SAFETY: see `program`.
        unsafe {
            let texture = gl.create_texture()?;
            gl.active_texture(glow::TEXTURE0);
            gl.bind_texture(glow::TEXTURE_2D, Some(texture));
            gl.tex_storage_2d(glow::TEXTURE_2D, 1, glow::RGBA8, size.0, size.1);
            for (name, value) in [
                (glow::TEXTURE_MIN_FILTER, glow::NEAREST),
                (glow::TEXTURE_MAG_FILTER, glow::NEAREST),
                (glow::TEXTURE_WRAP_S, glow::CLAMP_TO_EDGE),
                (glow::TEXTURE_WRAP_T, glow::CLAMP_TO_EDGE),
            ] {
                gl.tex_parameter_i32(glow::TEXTURE_2D, name, value as i32);
            }
            gl.bind_texture(glow::TEXTURE_2D, None);
            let fbo = gl.create_framebuffer()?;
            gl.bind_framebuffer(glow::FRAMEBUFFER, Some(fbo));
            let in_tile = caps.in_tile.as_ref().filter(|_| samples > 1);
            let samples = if in_tile.is_some() { samples } else { 1 };
            match in_tile {
                Some(ext) => (ext.texture)(
                    glow::FRAMEBUFFER,
                    glow::COLOR_ATTACHMENT0,
                    glow::TEXTURE_2D,
                    texture.0.get(),
                    0,
                    samples,
                ),
                None => gl.framebuffer_texture_2d(
                    glow::FRAMEBUFFER,
                    glow::COLOR_ATTACHMENT0,
                    glow::TEXTURE_2D,
                    Some(texture),
                    0,
                ),
            }
            let depth = if depth {
                let buffer = gl.create_renderbuffer()?;
                gl.bind_renderbuffer(glow::RENDERBUFFER, Some(buffer));
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
                    Some(buffer),
                );
                gl.bind_renderbuffer(glow::RENDERBUFFER, None);
                Some(buffer)
            } else {
                None
            };
            let status = gl.check_framebuffer_status(glow::FRAMEBUFFER);
            if status != glow::FRAMEBUFFER_COMPLETE {
                return Err(format!("framebuffer {size:?} x{samples}: {status:#x}"));
            }
            Ok(Self { texture, fbo, depth, size, samples })
        }
    }

    /// Starts the pass: the part of the texture `used`, cleared. What was in
    /// it is not loaded into the tiles.
    pub fn begin(&self, gl: &Context, used: (i32, i32)) {
        // SAFETY: see `program`.
        unsafe {
            gl.bind_framebuffer(glow::FRAMEBUFFER, Some(self.fbo));
            gl.invalidate_framebuffer(
                glow::FRAMEBUFFER,
                if self.depth.is_some() { &[glow::COLOR_ATTACHMENT0, glow::DEPTH_ATTACHMENT] } else { &[glow::COLOR_ATTACHMENT0] },
            );
            gl.viewport(0, 0, used.0, used.1);
            gl.disable(glow::SCISSOR_TEST);
            gl.disable(glow::STENCIL_TEST);
            gl.color_mask(true, true, true, true);
            gl.depth_mask(true);
            gl.clear_color(0.0, 0.0, 0.0, 0.0);
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

    pub fn delete(self, gl: &Context) {
        // SAFETY: see `program`.
        unsafe {
            gl.delete_framebuffer(self.fbo);
            gl.delete_texture(self.texture);
            if let Some(depth) = self.depth {
                gl.delete_renderbuffer(depth);
            }
        }
    }
}

/// The state Slint's renderer had, put back. (FemtoVG sets nearly all of its
/// own again for every flush; the framebuffer and the viewport it keeps.)
pub struct Kept {
    fbo: Option<NativeFramebuffer>,
    viewport: [i32; 4],
}

impl Kept {
    pub fn of(gl: &Context) -> Self {
        let mut viewport = [0; 4];
        // SAFETY: see `program`.
        let fbo = unsafe {
            gl.get_parameter_i32_slice(glow::VIEWPORT, &mut viewport);
            gl.get_parameter_framebuffer(glow::FRAMEBUFFER_BINDING)
        };
        Self { fbo, viewport }
    }

    pub fn put_back(self, gl: &Context) {
        // SAFETY: see `program`.
        unsafe {
            gl.bind_vertex_array(None);
            gl.bind_buffer(glow::ARRAY_BUFFER, None);
            gl.use_program(None);
            gl.active_texture(glow::TEXTURE0);
            gl.bind_texture(glow::TEXTURE_2D, None);
            gl.disable(glow::DEPTH_TEST);
            gl.disable(glow::CULL_FACE);
            gl.depth_mask(true);
            gl.bind_framebuffer(glow::FRAMEBUFFER, self.fbo);
            let [x, y, w, h] = self.viewport;
            gl.viewport(x, y, w, h);
        }
    }
}

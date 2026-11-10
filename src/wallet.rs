//! Original Monogram meshes/materials, rendered by native Rust in GtkGLArea.
use crate::widgets::*;
use glam::{Mat4, Vec3, Vec4};
use glow::HasContext;
use gtk::{cairo, gdk, glib, prelude::*, subclass::prelude::*};
use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    ffi::c_void,
    rc::Rc,
    sync::OnceLock,
};
pub fn matrices(w: f32, h: f32, x: f32, y: f32, padding: f32) -> (Mat4, Mat4) {
    let model = Mat4::from_rotation_x(-x)
        * Mat4::from_rotation_y(-y)
        * Mat4::from_rotation_y(std::f32::consts::FRAC_PI_2)
        * Mat4::from_translation(Vec3::new(-0.02, 0., 0.))
        * Mat4::from_scale(Vec3::splat(0.965));
    let mv = Mat4::from_translation(Vec3::new(0., 0., -6.7)) * model;
    let width = (w - padding * 2.)
        .max(1.)
        .min((h - padding * 2.).max(1.) * 1.64);
    let visible = width / 1.64;
    let fov = 2. * ((10.4f32.to_radians() / 2.).tan() * h / visible.max(1.)).atan();
    (
        mv,
        Mat4::perspective_rh_gl(fov, w / h.max(1.), 0.1, 20.) * mv,
    )
}
fn proc_address(name: &str) -> *const c_void {
    static LIB: OnceLock<libloading::Library> = OnceLock::new();
    let lib = LIB.get_or_init(|| unsafe {
        libloading::Library::new(
            std::env::var("MONOGRAM_EPOXY_LIBRARY").unwrap_or_else(|_| "libepoxy.so.0".into()),
        )
        .expect("libepoxy is packaged")
    });
    // libepoxy exports dispatch-table entries, so dereference the symbol once.
    unsafe {
        lib.get::<*const c_void>(format!("epoxy_{name}").as_bytes())
            .map(|s| {
                let entry = s.try_as_raw_ptr().unwrap() as *const *const c_void;
                *entry
            })
            .unwrap_or(std::ptr::null())
    }
}
pub fn mesh(name: &str) -> Vec<f32> {
    let data = std::fs::read(asset(&format!("wallet/{name}"))).expect("mesh asset");
    let mut at = 0;
    let integer = |at: &mut usize| {
        let n = u32::from_be_bytes(data[*at..*at + 4].try_into().unwrap()) as usize;
        *at += 4;
        n
    };
    let mut arrays = vec![];
    for _ in 0..3 {
        let n = integer(&mut at);
        let mut values = vec![];
        for _ in 0..n {
            values.push(f32::from_be_bytes(data[at..at + 4].try_into().unwrap()));
            at += 4
        }
        arrays.push(values)
    }
    let n = integer(&mut at);
    let mut out = vec![];
    for _ in 0..n {
        let p = integer(&mut at);
        let _uv = integer(&mut at);
        let norm = integer(&mut at);
        out.extend_from_slice(&arrays[0][p * 3..p * 3 + 3]);
        out.extend_from_slice(&arrays[2][norm * 3..norm * 3 + 3])
    }
    assert_eq!(at, data.len());
    assert_eq!(n % 3, 0);
    out
}
fn bytes(data: &[f32]) -> Vec<u8> {
    data.iter().flat_map(|v| v.to_ne_bytes()).collect()
}
#[derive(Clone)]
pub struct CardData {
    pub name: String,
    pub balance: String,
    pub precise: bool,
    pub compact: bool,
}
impl Default for CardData {
    fn default() -> Self {
        Self {
            name: "Everyday".into(),
            balance: "1.2048".into(),
            precise: false,
            compact: false,
        }
    }
}
fn content(d: &CardData, width: i32) -> cairo::ImageSurface {
    let height = (width as f64 * 625. / 1024.).round() as i32;
    let surface = cairo::ImageSurface::create(cairo::Format::ARgb32, width, height).unwrap();
    let c = cairo::Context::new(&surface).unwrap();
    c.scale(width as f64 / 336., height as f64 / 205.);
    let txt = |x: f64, y: f64, value: &str, size: f64, alpha: f64, bold: bool| {
        c.select_font_face(
            "Sans",
            cairo::FontSlant::Normal,
            if bold {
                cairo::FontWeight::Bold
            } else {
                cairo::FontWeight::Normal
            },
        );
        c.set_font_size(size);
        c.set_source_rgba(1., 1., 1., alpha);
        c.move_to(x, y);
        let _ = c.show_text(value);
    };
    let balance = if d.precise {
        format!("{}12345678", d.balance)
    } else {
        d.balance.clone()
    };
    if d.compact {
        txt(24., 45., &d.name, 14., 1., true);
        txt(24., 98., &format!("{balance} XMR"), 26., 1., true);
        txt(24., 125., "≈ €248.60", 16., 0.85, false)
    } else {
        txt(24., 32., &d.name, 12., 1., true);
        txt(
            if d.precise { 24. } else { 53. },
            94.,
            &format!("{balance} XMR"),
            if d.precise { 16. } else { 23. },
            1.,
            true,
        );
        txt(24., 115., "≈ €248.60", 14., 0.85, false);
        rounded(&c, 32., 135., 272., 30., 15.);
        c.set_source_rgba(1., 1., 1., 0.1);
        let _ = c.fill();
        let title = if d.precise {
            "Show rounded amount"
        } else {
            "Show precise amount"
        };
        c.set_font_size(12.);
        let size = c.text_extents(title).unwrap().width();
        txt((336. - size) / 2., 154., title, 12., 1., false);
        txt(24., 187., "ALEX MORGAN", 12., 0.95, false)
    }
    surface.flush();
    surface
}
fn rounded(c: &cairo::Context, x: f64, y: f64, w: f64, h: f64, r: f64) {
    c.new_sub_path();
    for (px, py, start) in [
        (x + w - r, y + r, -90f64),
        (x + w - r, y + h - r, 0.),
        (x + r, y + h - r, 90.),
        (x + r, y + r, 180.),
    ] {
        c.arc(px, py, r, start.to_radians(), (start + 90.).to_radians())
    }
    c.close_path()
}
struct Geometry {
    vao: glow::NativeVertexArray,
    vbo: glow::NativeBuffer,
    count: i32,
}
pub struct Renderer {
    gl: glow::Context,
    programs: HashMap<String, glow::NativeProgram>,
    meshes: HashMap<String, Geometry>,
    textures: Vec<glow::NativeTexture>,
    frames: Vec<glow::NativeFramebuffer>,
    buffers: Vec<glow::NativeRenderbuffer>,
    detail: glow::NativeTexture,
    engraving: glow::NativeTexture,
    front: glow::NativeTexture,
    tail: glow::NativeTexture,
    flecks: glow::NativeTexture,
    logo: glow::NativeTexture,
    ms: glow::NativeFramebuffer,
    color: glow::NativeRenderbuffer,
    depth: glow::NativeRenderbuffer,
    size: (i32, i32),
    uploaded: String,
    pub rendered: u32,
}
#[allow(unsafe_op_in_unsafe_fn)]
impl Renderer {
    unsafe fn program(
        gl: &glow::Context,
        vert: &str,
        frag: &str,
        define: &str,
    ) -> Result<glow::NativeProgram, String> {
        let p = gl.create_program()?;
        let mut shaders = vec![];
        for (kind, file) in [(glow::VERTEX_SHADER, vert), (glow::FRAGMENT_SHADER, frag)] {
            let code = std::fs::read_to_string(asset(&format!("wallet/{file}")))
                .map_err(|e| e.to_string())?
                .replace("#version 300 es", &format!("#version 330 core\n{define}"))
                .replace("precision highp float;", "");
            let s = gl.create_shader(kind)?;
            gl.shader_source(s, &code);
            gl.compile_shader(s);
            if !gl.get_shader_compile_status(s) {
                return Err(gl.get_shader_info_log(s));
            }
            gl.attach_shader(p, s);
            shaders.push(s)
        }
        gl.link_program(p);
        if !gl.get_program_link_status(p) {
            return Err(gl.get_program_info_log(p));
        }
        for s in shaders {
            gl.detach_shader(p, s);
            gl.delete_shader(s)
        }
        Ok(p)
    }
    unsafe fn geometry(
        gl: &glow::Context,
        data: &[f32],
        stride: i32,
        attributes: &[(u32, i32, i32)],
    ) -> Geometry {
        let vao = gl.create_vertex_array().unwrap();
        let vbo = gl.create_buffer().unwrap();
        gl.bind_vertex_array(Some(vao));
        gl.bind_buffer(glow::ARRAY_BUFFER, Some(vbo));
        gl.buffer_data_u8_slice(glow::ARRAY_BUFFER, &bytes(data), glow::STATIC_DRAW);
        for (i, size, offset) in attributes {
            gl.enable_vertex_attrib_array(*i);
            gl.vertex_attrib_pointer_f32(*i, *size, glow::FLOAT, false, stride * 4, offset * 4)
        }
        Geometry {
            vao,
            vbo,
            count: data.len() as i32 / stride,
        }
    }
    unsafe fn upload(gl: &glow::Context, w: i32, h: i32, data: &[u8], bgra: bool, mipmap: bool) {
        gl.tex_image_2d(
            glow::TEXTURE_2D,
            0,
            glow::RGBA8 as i32,
            w,
            h,
            0,
            if bgra { glow::BGRA } else { glow::RGBA },
            glow::UNSIGNED_BYTE,
            glow::PixelUnpackData::Slice(Some(data)),
        );
        for (k, v) in [
            (
                glow::TEXTURE_MIN_FILTER,
                if mipmap {
                    glow::LINEAR_MIPMAP_LINEAR
                } else {
                    glow::LINEAR
                },
            ),
            (glow::TEXTURE_MAG_FILTER, glow::LINEAR),
            (glow::TEXTURE_WRAP_S, glow::CLAMP_TO_EDGE),
            (glow::TEXTURE_WRAP_T, glow::CLAMP_TO_EDGE),
        ] {
            gl.tex_parameter_i32(glow::TEXTURE_2D, k, v as i32)
        }
        if mipmap {
            gl.generate_mipmap(glow::TEXTURE_2D)
        }
    }
    unsafe fn png(gl: &glow::Context, file: &str, unit: u32) -> glow::NativeTexture {
        let pix =
            gtk::gdk_pixbuf::Pixbuf::from_file(asset(&format!("wallet/{file}"))).expect("mask");
        let texture = gl.create_texture().unwrap();
        gl.active_texture(glow::TEXTURE0 + unit);
        gl.bind_texture(glow::TEXTURE_2D, Some(texture));
        Self::upload(
            gl,
            pix.width(),
            pix.height(),
            pix.read_pixel_bytes().as_ref(),
            false,
            true,
        );
        texture
    }
    unsafe fn matrix(&self, p: glow::NativeProgram, name: &str, value: Mat4) {
        self.gl.uniform_matrix_4_f32_slice(
            self.gl.get_uniform_location(p, name).as_ref(),
            false,
            &value.to_cols_array(),
        )
    }
    pub unsafe fn new(data: &CardData) -> Result<Self, String> {
        let gl = glow::Context::from_loader_function(proc_address);
        let mut programs = HashMap::new();
        for (name, define) in [
            ("body", "BODY"),
            ("engraving", "ENGRAVING"),
            ("qr", "QR"),
            ("edge", "EDGE"),
            ("flecks", "FLECKS"),
        ] {
            programs.insert(
                name.into(),
                Self::program(
                    &gl,
                    "wallet_card_vertex.glsl",
                    "wallet_card_fragment.glsl",
                    &format!("#define WALLET_{define}_PASS"),
                )?,
            );
        }
        for (name, frag) in [
            ("content", "wallet_card_content_fragment.glsl"),
            ("logo", "wallet_card_diamond_fragment.glsl"),
            ("monero", "modelFragment.glsl"),
        ] {
            programs.insert(
                name.into(),
                Self::program(
                    &gl,
                    if name == "monero" {
                        "modelVertex.glsl"
                    } else {
                        "wallet_card_vertex.glsl"
                    },
                    frag,
                    "",
                )?,
            );
        }
        let mut meshes = HashMap::new();
        for name in ["body", "engraving", "qr", "content", "logo", "edge"] {
            let p = programs[name];
            let values = mesh(if name == "edge" {
                "card_edge.binobj"
            } else {
                "card_back.binobj"
            });
            let attrs: Vec<_> = [("aPosition", 3, 0), ("aNormal", 3, 3)]
                .iter()
                .filter_map(|(a, s, o)| gl.get_attrib_location(p, a).map(|i| (i, *s, *o)))
                .collect();
            meshes.insert(name.into(), Self::geometry(&gl, &values, 6, &attrs));
        }
        let detail = Self::png(&gl, "details.png", 0);
        if data.compact {
            gl.bind_texture(glow::TEXTURE_2D, Some(detail));
            Self::upload(&gl, 1, 1, &[0; 4], false, false);
        }
        let engraving = Self::png(&gl, "engraving.png", 1);
        let tail = Self::png(&gl, "flecks-tail.png", 3);
        let front = gl.create_texture()?;
        gl.active_texture(glow::TEXTURE0);
        gl.bind_texture(glow::TEXTURE_2D, Some(front));
        let mut surface = content(data, 1024);
        Self::upload(
            &gl,
            surface.width(),
            surface.height(),
            &surface.data().unwrap(),
            true,
            false,
        );
        let flecks = gl.create_texture()?;
        gl.active_texture(glow::TEXTURE2);
        gl.bind_texture(glow::TEXTURE_2D_ARRAY, Some(flecks));
        let tiles = std::fs::read(asset("wallet/flecks-tiles.bin")).unwrap();
        let n = u32::from_le_bytes(tiles[..4].try_into().unwrap()) as i32;
        gl.tex_image_3d(
            glow::TEXTURE_2D_ARRAY,
            0,
            glow::RGBA8 as i32,
            48,
            48,
            n,
            0,
            glow::BGRA,
            glow::UNSIGNED_BYTE,
            glow::PixelUnpackData::Slice(Some(&tiles[4..])),
        );
        for (k, v) in [
            (glow::TEXTURE_MIN_FILTER, glow::LINEAR_MIPMAP_LINEAR),
            (glow::TEXTURE_MAG_FILTER, glow::LINEAR),
            (glow::TEXTURE_WRAP_S, glow::CLAMP_TO_EDGE),
            (glow::TEXTURE_WRAP_T, glow::CLAMP_TO_EDGE),
        ] {
            gl.tex_parameter_i32(glow::TEXTURE_2D_ARRAY, k, v as i32)
        }
        gl.tex_parameter_i32(glow::TEXTURE_2D_ARRAY, glow::TEXTURE_MAX_LEVEL, 3);
        gl.generate_mipmap(glow::TEXTURE_2D_ARRAY);
        let raw = std::fs::read(asset("wallet/flecks-vertices.bin")).unwrap();
        let verts: Vec<_> = raw
            .chunks_exact(4)
            .map(|b| f32::from_le_bytes(b.try_into().unwrap()))
            .collect();
        let p = programs["flecks"];
        let attrs: Vec<_> = [
            ("aPosition", 3, 0),
            ("aFlecksRegion", 3, 3),
            ("aFlecksCorner", 2, 6),
        ]
        .iter()
        .filter_map(|(a, s, o)| gl.get_attrib_location(p, a).map(|i| (i, *s, *o)))
        .collect();
        meshes.insert("flecks".into(), Self::geometry(&gl, &verts, 8, &attrs));
        if let Some(i) = gl.get_attrib_location(p, "aNormal") {
            gl.disable_vertex_attrib_array(i);
            gl.vertex_attrib_3_f32(i, -1., 0., 0.)
        }
        let ms = gl.create_framebuffer()?;
        let color = gl.create_renderbuffer()?;
        let depth = gl.create_renderbuffer()?;
        let logo = gl.create_texture()?;
        let textures = vec![detail, engraving, front, tail, flecks, logo];
        let mut r = Self {
            gl,
            programs,
            meshes,
            textures,
            frames: vec![ms],
            buffers: vec![color, depth],
            detail,
            engraving,
            front,
            tail,
            flecks,
            logo,
            ms,
            color,
            depth,
            size: (0, 0),
            uploaded: String::new(),
            rendered: 0,
        };
        r.bake_logo()?;
        Ok(r)
    }
    unsafe fn bake_logo(&mut self) -> Result<(), String> {
        let gl = &self.gl;
        let saved =
            std::num::NonZeroU32::new(gl.get_parameter_i32(glow::FRAMEBUFFER_BINDING) as u32)
                .map(glow::NativeFramebuffer);
        let mut viewport = [0; 4];
        gl.get_parameter_i32_slice(glow::VIEWPORT, &mut viewport);
        let frame = gl.create_framebuffer()?;
        self.frames.push(frame);
        gl.bind_framebuffer(glow::FRAMEBUFFER, Some(frame));
        gl.active_texture(glow::TEXTURE0);
        gl.bind_texture(glow::TEXTURE_2D, Some(self.logo));
        Self::upload(gl, 512, 512, &vec![0; 512 * 512 * 4], false, false);
        gl.framebuffer_texture_2d(
            glow::FRAMEBUFFER,
            glow::COLOR_ATTACHMENT0,
            glow::TEXTURE_2D,
            Some(self.logo),
            0,
        );
        let depth = gl.create_renderbuffer()?;
        self.buffers.push(depth);
        gl.bind_renderbuffer(glow::RENDERBUFFER, Some(depth));
        gl.renderbuffer_storage(glow::RENDERBUFFER, glow::DEPTH_COMPONENT24, 512, 512);
        gl.framebuffer_renderbuffer(
            glow::FRAMEBUFFER,
            glow::DEPTH_ATTACHMENT,
            glow::RENDERBUFFER,
            Some(depth),
        );
        if gl.check_framebuffer_status(glow::FRAMEBUFFER) != glow::FRAMEBUFFER_COMPLETE {
            return Err("Logo framebuffer incomplete".into());
        }
        let raw = std::fs::read(asset("monero.bin")).unwrap();
        assert_eq!(&raw[..4], b"XMR2");
        let values: Vec<_> = raw[12..]
            .chunks_exact(4)
            .map(|b| f32::from_le_bytes(b.try_into().unwrap()))
            .collect();
        let geometry = Self::geometry(
            gl,
            &values,
            11,
            &[(0, 3, 0), (1, 3, 3), (2, 3, 6), (3, 2, 9)],
        );
        let p = self.programs["monero"];
        gl.use_program(Some(p));
        let model = Mat4::from_rotation_x(-0.1)
            * Mat4::from_rotation_y(0.2)
            * Mat4::from_scale(Vec3::splat(0.8));
        self.matrix(p, "model", model);
        self.matrix(
            p,
            "mvp",
            Mat4::perspective_rh_gl(35f32.to_radians(), 1., 0.1, 10.)
                * Mat4::from_translation(Vec3::new(0., 0., -3.8))
                * model,
        );
        gl.uniform_1_f32(gl.get_uniform_location(p, "time").as_ref(), 0.);
        gl.viewport(0, 0, 512, 512);
        gl.clear_color(0., 0., 0., 0.);
        gl.depth_mask(true);
        gl.clear(glow::COLOR_BUFFER_BIT | glow::DEPTH_BUFFER_BIT);
        gl.enable(glow::DEPTH_TEST);
        gl.enable(glow::CULL_FACE);
        gl.disable(glow::BLEND);
        gl.bind_vertex_array(Some(geometry.vao));
        gl.draw_arrays(glow::TRIANGLES, 0, geometry.count);
        gl.generate_mipmap(glow::TEXTURE_2D);
        gl.tex_parameter_i32(
            glow::TEXTURE_2D,
            glow::TEXTURE_MIN_FILTER,
            glow::LINEAR_MIPMAP_LINEAR as i32,
        );
        gl.disable(glow::CULL_FACE);
        gl.delete_vertex_array(geometry.vao);
        gl.delete_buffer(geometry.vbo);
        gl.bind_framebuffer(glow::FRAMEBUFFER, saved);
        gl.viewport(viewport[0], viewport[1], viewport[2], viewport[3]);
        Ok(())
    }
    pub unsafe fn draw(&mut self, area: &gtk::GLArea, data: &CardData, x: f32, y: f32) {
        let gl = &self.gl;
        let mut viewport = [0; 4];
        gl.get_parameter_i32_slice(glow::VIEWPORT, &mut viewport);
        let (w, h) = (viewport[2].max(1), viewport[3].max(1));
        let scale = w as f32 / area.width().max(1) as f32;
        let (mv, mvp) = matrices(w as f32, h as f32, x, y, 24. * scale);
        let target =
            std::num::NonZeroU32::new(gl.get_parameter_i32(glow::FRAMEBUFFER_BINDING) as u32)
                .map(glow::NativeFramebuffer);
        gl.bind_framebuffer(glow::FRAMEBUFFER, Some(self.ms));
        if self.size != (w, h) {
            let samples = gl.get_parameter_i32(glow::MAX_SAMPLES).min(4);
            for (buffer, format, attachment) in [
                (self.color, glow::RGBA8, glow::COLOR_ATTACHMENT0),
                (self.depth, glow::DEPTH_COMPONENT24, glow::DEPTH_ATTACHMENT),
            ] {
                gl.bind_renderbuffer(glow::RENDERBUFFER, Some(buffer));
                gl.renderbuffer_storage_multisample(glow::RENDERBUFFER, samples, format, w, h);
                gl.framebuffer_renderbuffer(
                    glow::FRAMEBUFFER,
                    attachment,
                    glow::RENDERBUFFER,
                    Some(buffer),
                )
            }
            self.size = (w, h)
        }
        gl.viewport(0, 0, w, h);
        gl.clear_color(0., 0., 0., 0.);
        gl.depth_mask(true);
        gl.clear(glow::COLOR_BUFFER_BIT | glow::DEPTH_BUFFER_BIT);
        gl.enable(glow::DEPTH_TEST);
        gl.enable(glow::MULTISAMPLE);
        gl.disable(glow::CULL_FACE);
        let (_, rest) = matrices(w as f32, h as f32, 0., 0., 24. * scale);
        let a = rest * Vec4::new(-0.0125, 0., -1., 1.);
        let b = rest * Vec4::new(-0.0125, 0., 1., 1.);
        let width = ((b.x / b.w - a.x / a.w).abs() * w as f32 / 2.)
            .round()
            .max(1.) as i32;
        let key = format!("{}:{}:{}:{width}", data.name, data.balance, data.precise);
        if self.uploaded != key {
            let mut surface = content(data, width);
            gl.active_texture(glow::TEXTURE0);
            gl.bind_texture(glow::TEXTURE_2D, Some(self.front));
            Self::upload(
                gl,
                surface.width(),
                surface.height(),
                &surface.data().unwrap(),
                true,
                false,
            );
            self.uploaded = key
        }
        for (unit, texture) in [(0, self.detail), (1, self.engraving), (3, self.tail)] {
            gl.active_texture(glow::TEXTURE0 + unit);
            gl.bind_texture(glow::TEXTURE_2D, Some(texture))
        }
        gl.active_texture(glow::TEXTURE2);
        gl.bind_texture(glow::TEXTURE_2D_ARRAY, Some(self.flecks));
        for name in [
            "body",
            "engraving",
            "qr",
            "flecks",
            "logo",
            "content",
            "edge",
        ] {
            if data.compact && matches!(name, "engraving" | "qr" | "logo") {
                continue;
            }
            if name == "logo" && data.precise {
                continue;
            }
            let p = self.programs[name];
            gl.use_program(Some(p));
            self.matrix(p, "uModelViewMatrix", mv);
            self.matrix(p, "uMVPMatrix", mvp);
            gl.uniform_3_f32(
                gl.get_uniform_location(p, "uMainLightDirection").as_ref(),
                0.175,
                0.,
                0.83,
            );
            for (n, v) in [
                (
                    "uCardGradientRotation",
                    y.to_degrees() * 2. - x.to_degrees() * 1.25,
                ),
                ("uDetailsOffset", 0.),
                ("uDiamondAlpha", 1.),
            ] {
                gl.uniform_1_f32(gl.get_uniform_location(p, n).as_ref(), v)
            }
            for (n, v) in [
                ("uCardDetailTexture", 0),
                ("uEngravingTexture", 1),
                ("uFlecksTiles", 2),
                ("uFlecksTail", 3),
            ] {
                gl.uniform_1_i32(gl.get_uniform_location(p, n).as_ref(), v)
            }
            for (n, a, b) in [
                ("uEngravingTexel", 1. / 2048., 1. / 1024.),
                ("uQrCenter", 287. / 336., 92. / 205.),
                ("uFlecksPadding", 0., 0.),
            ] {
                gl.uniform_2_f32(gl.get_uniform_location(p, n).as_ref(), a, b)
            }
            gl.uniform_4_f32(
                gl.get_uniform_location(p, "uDiamondBounds").as_ref(),
                24. / 336.,
                75. / 205.,
                24. / 336.,
                24. / 205.,
            );
            if matches!(name, "logo" | "content") {
                gl.enable(glow::BLEND);
                gl.blend_func(glow::ONE, glow::ONE_MINUS_SRC_ALPHA)
            } else {
                gl.disable(glow::BLEND)
            }
            gl.depth_mask(matches!(name, "body" | "edge"));
            gl.depth_func(if matches!(name, "body" | "edge") {
                glow::LEQUAL
            } else {
                glow::EQUAL
            });
            if name == "flecks" {
                gl.enable(glow::POLYGON_OFFSET_FILL);
                gl.polygon_offset(1., 2.);
                gl.depth_func(glow::GREATER)
            }
            gl.active_texture(glow::TEXTURE0);
            gl.bind_texture(
                glow::TEXTURE_2D,
                Some(if name == "logo" {
                    self.logo
                } else if name == "content" {
                    self.front
                } else {
                    self.detail
                }),
            );
            let mesh = &self.meshes[name];
            gl.bind_vertex_array(Some(mesh.vao));
            gl.draw_arrays(glow::TRIANGLES, 0, mesh.count);
            if name == "flecks" {
                gl.disable(glow::POLYGON_OFFSET_FILL)
            }
        }
        gl.depth_mask(true);
        gl.disable(glow::BLEND);
        gl.bind_vertex_array(None);
        gl.bind_framebuffer(glow::READ_FRAMEBUFFER, Some(self.ms));
        gl.bind_framebuffer(glow::DRAW_FRAMEBUFFER, target);
        gl.blit_framebuffer(
            0,
            0,
            w,
            h,
            0,
            0,
            w,
            h,
            glow::COLOR_BUFFER_BIT,
            glow::NEAREST,
        );
        gl.bind_framebuffer(glow::FRAMEBUFFER, target);
        self.rendered += 1;
    }
}
impl Drop for Renderer {
    fn drop(&mut self) {
        unsafe {
            for (_, p) in self.programs.drain() {
                self.gl.delete_program(p)
            }
            for (_, g) in self.meshes.drain() {
                self.gl.delete_vertex_array(g.vao);
                self.gl.delete_buffer(g.vbo)
            }
            for t in self.textures.drain(..) {
                self.gl.delete_texture(t)
            }
            for f in self.frames.drain(..) {
                self.gl.delete_framebuffer(f)
            }
            for b in self.buffers.drain(..) {
                self.gl.delete_renderbuffer(b)
            }
        }
    }
}
mod imp {
    use super::*;
    #[derive(Default)]
    pub struct CardArea {
        pub renderer: RefCell<Option<Renderer>>,
        pub data: RefCell<CardData>,
        pub rx: Cell<f32>,
        pub ry: Cell<f32>,
    }
    #[glib::object_subclass]
    impl ObjectSubclass for CardArea {
        const NAME: &'static str = "MonogramWalletArea";
        type Type = super::CardArea;
        type ParentType = gtk::GLArea;
    }
    impl ObjectImpl for CardArea {}
    impl WidgetImpl for CardArea {
        fn request_mode(&self) -> gtk::SizeRequestMode {
            gtk::SizeRequestMode::HeightForWidth
        }
        fn measure(&self, o: gtk::Orientation, size: i32) -> (i32, i32, i32, i32) {
            let (w, h) = if self.data.borrow().compact {
                (320, 224)
            } else {
                (500, 305)
            };
            let natural = if o == gtk::Orientation::Horizontal {
                w
            } else if size >= 0 {
                size * h / w
            } else {
                h
            };
            (0, natural, -1, -1)
        }
        fn realize(&self) {
            self.parent_realize();
            let area = self.obj();
            area.make_current();
            if area.error().is_none() {
                match unsafe { Renderer::new(&self.data.borrow()) } {
                    Ok(r) => {
                        self.renderer.replace(Some(r));
                    }
                    Err(e) => {
                        area.set_error(Some(&glib::Error::new(
                            gtk::gdk::GLError::NotAvailable,
                            &e,
                        )));
                    }
                }
            }
        }
        fn unrealize(&self) {
            self.obj().make_current();
            self.renderer.borrow_mut().take();
            self.parent_unrealize()
        }
    }
    impl GLAreaImpl for CardArea {
        fn render(&self, _: &gdk::GLContext) -> glib::Propagation {
            if let Some(ref mut renderer) = *self.renderer.borrow_mut() {
                unsafe {
                    renderer.draw(
                        self.obj().upcast_ref(),
                        &self.data.borrow(),
                        self.rx.get(),
                        self.ry.get(),
                    )
                }
            }
            glib::Propagation::Stop
        }
    }
}
glib::wrapper! {pub struct CardArea(ObjectSubclass<imp::CardArea>) @extends gtk::GLArea,gtk::Widget,@implements gtk::Accessible,gtk::Buildable,gtk::ConstraintTarget;}
pub struct WalletCard {
    pub root: gtk::Overlay,
    pub area: CardArea,
    controls: gtk::Fixed,
    precision: gtk::Button,
    qr: gtk::Button,
}
impl WalletCard {
    pub fn new(name: &str, balance: &str, compact: bool, on_qr: impl Fn() + 'static) -> Rc<Self> {
        let area: CardArea = glib::Object::new();
        area.set_required_version(3, 3);
        area.set_allowed_apis(gdk::GLAPI::GL);
        area.set_has_depth_buffer(true);
        area.set_auto_render(false);
        area.set_hexpand(true);
        area.imp().data.replace(CardData {
            name: name.into(),
            balance: balance.into(),
            compact,
            precise: false,
        });
        let root = gtk::Overlay::new();
        root.set_child(Some(&area));
        root.add_css_class("wallet-model");
        let controls = gtk::Fixed::new();
        root.add_overlay(&controls);
        root.set_measure_overlay(&controls, false);
        let precision = gtk::Button::builder()
            .tooltip_text("Show precise amount")
            .build();
        let qr = gtk::Button::builder()
            .tooltip_text("Show receiving QR code")
            .build();
        for b in [&precision, &qr] {
            b.add_css_class("wallet-hit-target");
            controls.put(b, 0., 0.);
            b.set_visible(!compact)
        }
        let card = Rc::new(Self {
            root,
            area,
            controls,
            precision,
            qr,
        });
        let weak = Rc::downgrade(&card);
        card.precision.connect_clicked(move |_| {
            if let Some(c) = weak.upgrade() {
                let value = !c.area.imp().data.borrow().precise;
                c.area.imp().data.borrow_mut().precise = value;
                c.area.queue_render();
            }
        });
        card.qr.connect_clicked(move |_| on_qr());
        let motion = gtk::EventControllerMotion::new();
        let weak = Rc::downgrade(&card);
        motion.connect_motion(move |_, x, y| {
            if let Some(c) = weak.upgrade() {
                c.area
                    .imp()
                    .ry
                    .set((x as f32 / c.area.width().max(1) as f32 - 0.5) * 30f32.to_radians());
                c.area
                    .imp()
                    .rx
                    .set((y as f32 / c.area.height().max(1) as f32 - 0.5) * 30f32.to_radians());
                c.position();
                c.area.queue_render();
            }
        });
        let weak = Rc::downgrade(&card);
        motion.connect_leave(move |_| {
            if let Some(c) = weak.upgrade() {
                c.area.imp().rx.set(0.);
                c.area.imp().ry.set(0.);
                c.position();
                c.area.queue_render();
            }
        });
        card.root.add_controller(motion);
        let weak = Rc::downgrade(&card);
        card.area.connect_resize(move |_, _, _| {
            let weak = weak.clone();
            glib::idle_add_local_once(move || {
                if let Some(c) = weak.upgrade() {
                    c.position()
                }
            });
        });
        card
    }
    fn position(&self) {
        if self.area.imp().data.borrow().compact {
            return;
        }
        let (w, h) = (self.area.width() as f32, self.area.height() as f32);
        if w <= 0. || h <= 0. {
            return;
        }
        let (_, m) = matrices(
            w,
            h,
            self.area.imp().rx.get(),
            self.area.imp().ry.get(),
            24.,
        );
        for (button, (x, y, rw, rh), material) in [
            (&self.precision, (32., 135., 272., 30.), false),
            (&self.qr, (262., 73., 50., 38.), true),
        ] {
            let mut xs = vec![];
            let mut ys = vec![];
            for (u, v) in [(x, y), (x + rw, y), (x + rw, y + rh), (x, y + rh)] {
                let py = if material {
                    (0.5 - v / 205.) * 1.219512
                } else {
                    0.618561 - v / 205. * 1.212122
                };
                let p = m * Vec4::new(-0.0125, py, u / 336. * 2. - 1., 1.);
                xs.push((p.x / p.w * 0.5 + 0.5) * w);
                ys.push((0.5 - p.y / p.w * 0.5) * h)
            }
            let left = xs.iter().copied().fold(f32::INFINITY, f32::min);
            let top = ys.iter().copied().fold(f32::INFINITY, f32::min);
            let right = xs.iter().copied().fold(f32::NEG_INFINITY, f32::max);
            let bottom = ys.iter().copied().fold(f32::NEG_INFINITY, f32::max);
            self.controls.move_(button, left as f64, top as f64);
            button.set_size_request(
                (right - left).round().max(1.) as i32,
                (bottom - top).round().max(1.) as i32,
            )
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn tilted_face_stays_inside() {
        for w in [320., 500.] {
            for x in [-0.2618, 0., 0.2618] {
                for y in [-0.2618, 0., 0.2618] {
                    let (_, m) = matrices(w, w / 1.64, x, y, 24.);
                    for a in [-0.593561, 0.618561] {
                        for b in [-1., 1.] {
                            let p = m * Vec4::new(-0.0125, a, b, 1.);
                            assert!((p.x / p.w).abs() < 1. && (p.y / p.w).abs() < 1.)
                        }
                    }
                }
            }
        }
        assert_eq!(mesh("card_back.binobj").len() % 18, 0);
    }
}

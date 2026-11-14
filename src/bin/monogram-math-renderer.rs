// SPDX-License-Identifier: GPL-3.0-or-later
//! Private bounded LaTeX math helper. Launched only inside bwrap.
//! Args: `display|inline` and an `AARRGGBB` hex colour; formula source on stdin.
//! Output: `<depth>\n` then a PNG (same protocol the former JLaTeXMath helper used).
use latex_rust::{Color, Dim, MathFont, MathStyle, PngOptions, layout_with_em_size_pt, parse, render_png};
use std::io::{Read, Write};

#[path = "../renderer_hardening.rs"]
mod hardening;
use hardening::{limit, restrict_calls};

/// Formulas cannot import resources or alter parser-wide command definitions.
const BLOCKED: [&str; 11] = [
    "includegraphics", "input", "include", "write", "read", "newcommand", "renewcommand",
    "newenvironment", "def", "catcode", "special",
];
/// Em size in pixels, matching the former helper (36 px at 72 dpi).
const EM: i64 = 36;

fn run() -> Result<(), i32> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() != 2 || !matches!(args[0].as_str(), "display" | "inline") {
        return Err(3);
    }
    let argb = u32::from_str_radix(&args[1], 16).map_err(|_| 3)?;
    let mut input = Vec::new();
    std::io::stdin().lock().take(32769).read_to_end(&mut input).map_err(|_| 3)?;
    let source = String::from_utf8(input).map_err(|_| 3)?;
    if source.is_empty() || source.len() > 32768 || source.encode_utf16().count() > 8192 {
        return Err(3);
    }
    if BLOCKED.iter().any(|command| source.contains(&format!("\\{command}"))) {
        return Err(4);
    }
    let font = MathFont::stix_two_math().map_err(|_| 5)?;
    let style = if args[0] == "display" { MathStyle::Display } else { MathStyle::Text };
    let ast = parse(&source).map_err(|_| 5)?;
    let em = Dim::from_i64(EM);
    let tree = layout_with_em_size_pt(&ast, &font, style, &em).map_err(|_| 5)?;
    let mut options = PngOptions::new();
    options.font_size_pt = em.clone();
    options.dpi = Dim::from_i64(72);
    options.color = Color::rgb((argb >> 16) as u8, (argb >> 8) as u8, argb as u8);
    // Layout boxes are in em units; the rasterizer places the baseline at ceil(height * em px).
    let baseline = (&tree.height * &em).ceil_to_u32().map_err(|_| 5)?;
    // Reject oversized output from the layout box, before anything is rasterized.
    let wide = (&tree.width * &em).ceil_to_u32().map_err(|_| 6)?;
    let tall = ((&tree.height + &tree.depth) * &em).ceil_to_u32().map_err(|_| 6)?;
    if wide < 1 || tall < 1 || wide > 4096 || tall > 2048 || u64::from(wide) * u64::from(tall) > 4_194_304 {
        return Err(6);
    }
    let mut png = render_png(&tree, &font, &options).map_err(|_| 5)?;
    let mut pixmap = tiny_skia::Pixmap::decode_png(&png).map_err(|_| 5)?;
    let (width, height) = (pixmap.width(), pixmap.height());
    if width < 1 || height < 1 || width > 4096 || height > 2048 || u64::from(width) * u64::from(height) > 4_194_304 {
        return Err(6);
    }
    let alpha = (argb >> 24) as u8;
    if alpha != 255 {
        for pixel in pixmap.data_mut().chunks_exact_mut(4) {
            for channel in pixel {
                *channel = (u16::from(*channel) * u16::from(alpha) / 255) as u8;
            }
        }
        png = pixmap.encode_png().map_err(|_| 5)?;
    }
    let depth = height.saturating_sub(baseline);
    let mut output = std::io::stdout().lock();
    output
        .write_all(format!("{depth}\n").as_bytes())
        .and_then(|_| output.write_all(&png))
        .and_then(|_| output.flush())
        .map_err(|_| 7)
}
fn main() {
    const MIB: libc::rlim_t = 1024 * 1024;
    if !limit(libc::RLIMIT_AS, 512 * MIB) || !limit(libc::RLIMIT_STACK, 8 * MIB) || !limit(libc::RLIMIT_CORE, 0)
        || !limit(libc::RLIMIT_FSIZE, 0) || !limit(libc::RLIMIT_NOFILE, 32)
        || unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) } != 0
        || !restrict_calls()
    {
        std::process::exit(2);
    }
    unsafe { libc::umask(0o077); libc::alarm(6) };
    if let Err(code) = run() {
        std::process::exit(code);
    }
}

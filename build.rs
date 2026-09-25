//! Build-time conversion of the app icon PNG into raw RGBA bytes for the
//! Windows system-tray icon (`--tray` mode). `tray-icon` needs plain RGBA
//! pixel data, and rather than decoding the PNG at runtime we do it once here
//! with the pure-Rust `png` crate — no C compiler or system libraries, so
//! Linux/macOS builds gain no system dependencies from this script.

use std::fmt::Write as _;
use std::path::Path;

fn main() {
    let icon_path = Path::new("web/icons/icon-192.png");
    println!("cargo:rerun-if-changed={}", icon_path.display());

    let file = std::fs::File::open(icon_path).expect("tray icon source web/icons/icon-192.png");
    let decoder = png::Decoder::new(std::io::BufReader::new(file));
    let mut reader = decoder.read_info().expect("PNG header");
    let mut buf = vec![0u8; reader.output_buffer_size().expect("PNG output buffer size")];
    let info = reader.next_frame(&mut buf).expect("PNG frame");
    let (width, height) = (info.width, info.height);

    // Expand whatever the PNG stores to 32-bit RGBA for Icon::from_rgba.
    let rgba: Vec<u8> = match info.color_type {
        png::ColorType::Rgba => buf,
        png::ColorType::Rgb => {
            assert_eq!(info.bit_depth, png::BitDepth::Eight);
            let (chunks, _rem) = buf.as_chunks::<3>();
            let mut out = Vec::with_capacity(chunks.len() * 4);
            for px in chunks {
                out.extend_from_slice(px);
                out.push(255);
            }
            out
        }
        other => panic!("tray icon PNG has unsupported color type {other:?}"),
    };
    assert_eq!(rgba.len(), width as usize * height as usize * 4);

    let out_dir = std::env::var("OUT_DIR").expect("OUT_DIR");
    std::fs::write(Path::new(&out_dir).join("tray_icon.rgba"), &rgba).expect("write rgba");
    let mut rs = String::new();
    writeln!(rs, "pub const TRAY_ICON_WIDTH: u32 = {width};").unwrap();
    writeln!(rs, "pub const TRAY_ICON_HEIGHT: u32 = {height};").unwrap();
    std::fs::write(Path::new(&out_dir).join("tray_icon.rs"), rs).expect("write rs");
}

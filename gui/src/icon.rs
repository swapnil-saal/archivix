//! The app icon, drawn procedurally so it is crisp at every size and needs no
//! binary assets: a rounded square with an indigo→teal gradient and three
//! stacked white "compressed layers".

use eframe::egui;

/// Signed distance to a rounded rectangle centred at (cx, cy).
fn sd_round_rect(x: f32, y: f32, cx: f32, cy: f32, hw: f32, hh: f32, r: f32) -> f32 {
    let qx = (x - cx).abs() - (hw - r);
    let qy = (y - cy).abs() - (hh - r);
    let ox = qx.max(0.0);
    let oy = qy.max(0.0);
    (ox * ox + oy * oy).sqrt() + qx.max(qy).min(0.0) - r
}

fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

/// Straight-alpha RGBA pixels, `size × size`.
pub fn rgba(size: u32) -> Vec<u8> {
    let n = size as usize;
    let mut out = vec![0u8; n * n * 4];
    let ss = 4; // 4×4 supersampling for anti-aliasing
    let top = [0x5B as f32, 0x5B as f32, 0xF0 as f32]; // indigo
    let bot = [0x14 as f32, 0xC8 as f32, 0xB4 as f32]; // teal
    for py in 0..n {
        for px in 0..n {
            let (mut a_bg, mut a_fg) = (0.0f32, 0.0f32);
            let mut fg_alpha_weight = 0.0f32;
            for sy in 0..ss {
                for sx in 0..ss {
                    // Normalised coordinates in [0, 1].
                    let x = (px as f32 + (sx as f32 + 0.5) / ss as f32) / n as f32;
                    let y = (py as f32 + (sy as f32 + 0.5) / ss as f32) / n as f32;
                    // macOS-style tile with a small margin.
                    if sd_round_rect(x, y, 0.5, 0.5, 0.41, 0.41, 0.095) <= 0.0 {
                        a_bg += 1.0;
                        // Three layers, narrowing towards the top, the middle one offset.
                        let layers = [(0.355, 0.24, 0.62), (0.5, 0.29, 0.82), (0.645, 0.24, 1.0)];
                        for (i, (cy, hw, alpha)) in layers.iter().enumerate() {
                            let cx = if i == 1 { 0.53 } else { 0.5 };
                            if sd_round_rect(x, y, cx, *cy, *hw, 0.052, 0.052) <= 0.0 {
                                a_fg += 1.0;
                                fg_alpha_weight += alpha;
                                break;
                            }
                        }
                    }
                }
            }
            let total = (ss * ss) as f32;
            let cov_bg = a_bg / total;
            if cov_bg == 0.0 {
                continue;
            }
            let t = (px as f32 + py as f32) / (2.0 * n as f32);
            let mut c = [lerp(top[0], bot[0], t), lerp(top[1], bot[1], t), lerp(top[2], bot[2], t)];
            if a_fg > 0.0 {
                // Blend white layers over the gradient by their coverage × opacity.
                let k = fg_alpha_weight / a_bg;
                for ch in &mut c {
                    *ch = lerp(*ch, 255.0, k);
                }
            }
            let o = (py * n + px) * 4;
            out[o] = c[0].round() as u8;
            out[o + 1] = c[1].round() as u8;
            out[o + 2] = c[2].round() as u8;
            out[o + 3] = (cov_bg * 255.0).round() as u8;
        }
    }
    out
}

pub fn icon_data(size: u32) -> egui::IconData {
    egui::IconData { rgba: rgba(size), width: size, height: size }
}

/// Write a macOS `.iconset` directory (input for `iconutil -c icns`).
pub fn write_iconset(dir: &std::path::Path) -> anyhow::Result<()> {
    std::fs::create_dir_all(dir)?;
    for (base, scale) in [(16, 1), (16, 2), (32, 1), (32, 2), (128, 1), (128, 2), (256, 1), (256, 2), (512, 1), (512, 2)] {
        let px = base * scale;
        let name = if scale == 1 { format!("icon_{base}x{base}.png") } else { format!("icon_{base}x{base}@2x.png") };
        write_png(&dir.join(name), px)?;
    }
    Ok(())
}

pub fn write_png(path: &std::path::Path, size: u32) -> anyhow::Result<()> {
    let file = std::io::BufWriter::new(std::fs::File::create(path)?);
    let mut enc = png::Encoder::new(file, size, size);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    enc.write_header()?.write_image_data(&rgba(size))?;
    Ok(())
}

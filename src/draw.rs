use fontdue::Font;
use std::error::Error;
use std::process::Command;

#[derive(Clone, Copy)]
pub struct Color {
    pub r: f32,
    pub g: f32,
    pub b: f32,
    pub a: f32,
}

impl Color {
    pub const fn new(r: f32, g: f32, b: f32, a: f32) -> Self {
        Self { r, g, b, a }
    }
}

/// Argb8888 canvas. Wayland expects premultiplied alpha, so every write
/// premultiplies color channels by the effective alpha.
pub struct Canvas<'a> {
    pub buf: &'a mut [u8],
    pub w: i32,
    pub h: i32,
}

impl Canvas<'_> {
    /// Overwrite the whole buffer with one premultiplied color.
    pub fn clear(&mut self, c: Color) {
        let px = [
            (c.b * c.a * 255.0).round() as u8,
            (c.g * c.a * 255.0).round() as u8,
            (c.r * c.a * 255.0).round() as u8,
            (c.a * 255.0).round() as u8,
        ];
        for chunk in self.buf.chunks_exact_mut(4) {
            chunk.copy_from_slice(&px);
        }
    }

    fn blend_px(&mut self, x: i32, y: i32, c: Color, coverage: f32) {
        if x < 0 || y < 0 || x >= self.w || y >= self.h {
            return;
        }
        let a = c.a * coverage;
        if a <= 0.0 {
            return;
        }
        let i = ((y * self.w + x) * 4) as usize;
        let inv = 1.0 - a;
        // Memory order for little-endian Argb8888 is B, G, R, A.
        let src = [c.b * a, c.g * a, c.r * a, a];
        for (off, s) in src.iter().enumerate() {
            let d = self.buf[i + off] as f32 / 255.0;
            self.buf[i + off] = ((s + d * inv) * 255.0).round().min(255.0) as u8;
        }
    }

    pub fn fill_rect(&mut self, x: i32, y: i32, w: i32, h: i32, c: Color) {
        for yy in y..y + h {
            for xx in x..x + w {
                self.blend_px(xx, yy, c, 1.0);
            }
        }
    }

    pub fn stroke_rect(&mut self, x: i32, y: i32, w: i32, h: i32, t: i32, c: Color) {
        if w < 2 * t || h < 2 * t {
            self.fill_rect(x, y, w, h, c);
            return;
        }
        self.fill_rect(x, y, w, t, c);
        self.fill_rect(x, y + h - t, w, t, c);
        self.fill_rect(x, y + t, t, h - 2 * t, c);
        self.fill_rect(x + w - t, y + t, t, h - 2 * t, c);
    }

    pub fn text_centered(&mut self, font: &Font, text: &str, cx: f32, cy: f32, px: f32, c: Color) {
        let total = text_width(font, text, px);
        let baseline = match font.horizontal_line_metrics(px) {
            Some(lm) => cy + (lm.ascent + lm.descent) / 2.0,
            None => cy + px * 0.35,
        };
        let mut pen = cx - total / 2.0;
        for ch in text.chars() {
            let (m, bitmap) = font.rasterize(ch, px);
            let x0 = (pen + m.xmin as f32).round() as i32;
            let y0 = (baseline - m.ymin as f32).round() as i32 - m.height as i32;
            for row in 0..m.height {
                for col in 0..m.width {
                    let cov = bitmap[row * m.width + col] as f32 / 255.0;
                    if cov > 0.0 {
                        self.blend_px(x0 + col as i32, y0 + row as i32, c, cov);
                    }
                }
            }
            pen += m.advance_width;
        }
    }
}

pub fn text_width(font: &Font, text: &str, px: f32) -> f32 {
    text.chars().map(|ch| font.metrics(ch, px).advance_width).sum()
}

pub fn load_font() -> Result<Font, Box<dyn Error>> {
    let mut candidates: Vec<String> = Vec::new();
    if let Ok(out) = Command::new("fc-match")
        .args(["-f", "%{file}", "sans:bold"])
        .output()
    {
        if out.status.success() {
            let p = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if !p.is_empty() {
                candidates.push(p);
            }
        }
    }
    candidates.extend(
        [
            "/usr/share/fonts/noto/NotoSans-Bold.ttf",
            "/usr/share/fonts/TTF/DejaVuSans-Bold.ttf",
            "/usr/share/fonts/liberation/LiberationSans-Bold.ttf",
        ]
        .map(String::from),
    );
    for path in &candidates {
        if let Ok(bytes) = std::fs::read(path) {
            if let Ok(font) = Font::from_bytes(bytes, fontdue::FontSettings::default()) {
                return Ok(font);
            }
        }
    }
    Err(format!("no usable font found, tried: {}", candidates.join(", ")).into())
}

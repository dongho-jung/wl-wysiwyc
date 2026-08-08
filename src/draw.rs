use fontdue::{Font, Metrics};
use std::cell::RefCell;
use std::collections::HashMap;
use std::error::Error;
use std::process::Command;

/// A glyph at one size: what fontdue hands back, kept for next time.
type Raster = (Metrics, Vec<u8>);

thread_local! {
    /// Rasterized glyphs, kept between frames. fontdue rasterizes on every
    /// call, and a frame is a few hundred glyphs; doing that again for each
    /// frame of a pointer's travel is most of what a frame costs.
    static GLYPHS: RefCell<HashMap<(char, u32), Raster>> = RefCell::new(HashMap::new());
}

/// Look at a glyph's raster, drawing it once and keeping it.
fn with_glyph<T>(font: &Font, ch: char, px: f32, f: impl FnOnce(&Metrics, &[u8]) -> T) -> T {
    let key = (ch, (px * 64.0) as u32);
    GLYPHS.with(|cache| {
        let mut cache = cache.borrow_mut();
        let entry = cache.entry(key).or_insert_with(|| font.rasterize(ch, px));
        f(&entry.0, &entry.1)
    })
}

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

    /// The same color, thinned by a factor: 1.0 keeps it, 0.0 hides it.
    pub fn fade(self, f: f32) -> Self {
        Self {
            a: self.a * f,
            ..self
        }
    }
}

/// A rectangle in buffer pixels, kept in floats so shapes can land on half
/// pixels and still come out with clean edges.
#[derive(Clone, Copy)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    pub fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        Self { x, y, w, h }
    }

    pub fn grow(self, d: f32) -> Self {
        Self {
            x: self.x - d,
            y: self.y - d,
            w: self.w + 2.0 * d,
            h: self.h + 2.0 * d,
        }
    }

    pub fn shift(self, dx: f32, dy: f32) -> Self {
        Self {
            x: self.x + dx,
            y: self.y + dy,
            ..self
        }
    }

    /// Distance from a point to the rectangle's rounded outline: negative
    /// inside, positive outside, in pixels. Every rounded shape below is
    /// drawn from this one measurement, which is what keeps their edges
    /// smooth instead of stair-stepped.
    fn distance(self, radius: f32, px: f32, py: f32) -> f32 {
        let (hx, hy) = (self.w / 2.0, self.h / 2.0);
        let radius = radius.clamp(0.0, hx.min(hy));
        let qx = (px - (self.x + hx)).abs() - (hx - radius);
        let qy = (py - (self.y + hy)).abs() - (hy - radius);
        qx.max(0.0).hypot(qy.max(0.0)) + qx.max(qy).min(0.0) - radius
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
        if self.buf.len() < 4 {
            return;
        }
        // One pixel, then keep doubling what is already written. A whole
        // output is fifty megabytes and a per-pixel loop over it is slow
        // enough to be felt between frames; this is memmove all the way up.
        self.buf[..4].copy_from_slice(&px);
        let mut done = 4;
        while done < self.buf.len() {
            let n = done.min(self.buf.len() - done);
            self.buf.copy_within(0..n, done);
            done += n;
        }
    }

    /// Source-over-destination on premultiplied bytes. Whole numbers rather
    /// than floats: this runs once per pixel of every shape drawn, and a
    /// window's worth of them is felt.
    fn blend_px(&mut self, x: i32, y: i32, c: Color, coverage: f32) {
        if x < 0 || y < 0 || x >= self.w || y >= self.h {
            return;
        }
        let a = ((c.a * coverage).clamp(0.0, 1.0) * 255.0 + 0.5) as u32;
        if a == 0 {
            return;
        }
        let i = ((y * self.w + x) * 4) as usize;
        let inv = 255 - a;
        // Memory order for little-endian Argb8888 is B, G, R, A.
        let src = [c.b, c.g, c.r, 1.0];
        for (off, s) in src.iter().enumerate() {
            let s = (s.clamp(0.0, 1.0) * 255.0 + 0.5) as u32 * a;
            let d = self.buf[i + off] as u32 * inv;
            self.buf[i + off] = ((s + d + 127) / 255).min(255) as u8;
        }
    }

    /// Paint one color over a box, asking `coverage` how much of each pixel
    /// the shape covers.
    fn shade<F: Fn(f32, f32) -> f32>(&mut self, bounds: Rect, c: Color, coverage: &F) {
        let x0 = (bounds.x.floor() as i32).max(0);
        let y0 = (bounds.y.floor() as i32).max(0);
        let x1 = ((bounds.x + bounds.w).ceil() as i32).min(self.w);
        let y1 = ((bounds.y + bounds.h).ceil() as i32).min(self.h);
        for y in y0..y1 {
            for x in x0..x1 {
                let a = coverage(x as f32 + 0.5, y as f32 + 0.5);
                if a > 0.0 {
                    self.blend_px(x, y, c, a.min(1.0));
                }
            }
        }
    }

    /// Ask `coverage` only about the pixels within `band` of the rectangle's
    /// edge, and take the rest as given: covered inside, empty outside.
    ///
    /// A window-sized outline visits a window's worth of pixels otherwise, and
    /// the distance measurement behind these shapes is too expensive to spend
    /// on the middle of a shape whose middle never changes.
    fn shade_edge<F: Fn(f32, f32) -> f32>(
        &mut self,
        r: Rect,
        band: f32,
        fill_inside: Option<Color>,
        c: Color,
        coverage: F,
    ) {
        let outer = r.grow(band);
        let inner = r.grow(-band);
        if inner.w <= 0.0 || inner.h <= 0.0 {
            self.shade(outer, c, &coverage);
            return;
        }
        let (ix, iy, iw, ih) = (inner.x, inner.y, inner.w, inner.h);
        for band in [
            Rect::new(outer.x, outer.y, outer.w, iy - outer.y),
            Rect::new(outer.x, iy + ih, outer.w, outer.y + outer.h - (iy + ih)),
            Rect::new(outer.x, iy, ix - outer.x, ih),
            Rect::new(ix + iw, iy, outer.x + outer.w - (ix + iw), ih),
        ] {
            self.shade(band, c, &coverage);
        }
        if let Some(solid) = fill_inside {
            self.shade(inner, solid, &|_, _| 1.0);
        }
    }

    pub fn round_rect(&mut self, r: Rect, radius: f32, c: Color) {
        let band = radius + 1.0;
        self.shade_edge(r, band, Some(c), c, |px, py| {
            0.5 - r.distance(radius, px, py)
        });
    }

    pub fn round_rect_outline(&mut self, r: Rect, radius: f32, t: f32, c: Color) {
        let band = radius + t + 1.0;
        self.shade_edge(r, band, None, c, |px, py| {
            0.5 + t / 2.0 - r.distance(radius, px, py).abs()
        });
    }

    /// A shadow cast by the shape, fading out over `blur` pixels. Enough to
    /// lift a label off the window under it and off its neighbours.
    pub fn round_rect_shadow(&mut self, r: Rect, radius: f32, blur: f32, c: Color) {
        let band = radius + blur + 1.0;
        self.shade_edge(r, band, Some(c), c, |px, py| {
            let t = (1.0 - r.distance(radius, px, py) / blur).clamp(0.0, 1.0);
            t * t
        });
    }

    /// Draw text from a pen position and return where the pen ends up, so a
    /// caller can lay several runs in a row.
    pub fn text_run(
        &mut self,
        font: &Font,
        text: &str,
        pen: f32,
        baseline: f32,
        px: f32,
        c: Color,
    ) -> f32 {
        let mut pen = pen;
        for ch in text.chars() {
            // Blend straight out of the cache: copying the raster out first
            // costs an allocation per glyph per frame.
            pen += with_glyph(font, ch, px, |m, bitmap| {
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
                m.advance_width
            });
        }
        pen
    }

    pub fn text_centered(&mut self, font: &Font, text: &str, cx: f32, cy: f32, px: f32, c: Color) {
        let pen = cx - text_width(font, text, px) / 2.0;
        self.text_run(font, text, pen, baseline(font, cy, px), px, c);
    }
}

/// The baseline that puts a line of text on a given center.
pub fn baseline(font: &Font, cy: f32, px: f32) -> f32 {
    match font.horizontal_line_metrics(px) {
        Some(lm) => cy + (lm.ascent + lm.descent) / 2.0,
        None => cy + px * 0.35,
    }
}

pub fn text_width(font: &Font, text: &str, px: f32) -> f32 {
    text.chars()
        .map(|ch| with_glyph(font, ch, px, |m, _| m.advance_width))
        .sum()
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

//! Draws the widget into a per-pixel-alpha bitmap for UpdateLayeredWindow.
//!
//! GDI text ignores alpha, so text is drawn white on black and the result is
//! used as a coverage mask, then recolored. Grayscale antialiasing is required:
//! ClearType's per-channel fringes would be wrong on a transparent background.

use crate::config::{FontWeight, Rgba, SYSTEM_FONT};
use std::ptr::{null, null_mut};
use windows_sys::Win32::Foundation::{HWND, POINT, SIZE};
use windows_sys::Win32::Graphics::Gdi::*;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    SystemParametersInfoW, UpdateLayeredWindow, NONCLIENTMETRICSW, SPI_GETNONCLIENTMETRICS, ULW_ALPHA,
};

/// The family Windows uses for its own interface text.
pub fn system_font_name() -> String {
    unsafe {
        let mut metrics: NONCLIENTMETRICSW = std::mem::zeroed();
        metrics.cbSize = std::mem::size_of::<NONCLIENTMETRICSW>() as u32;
        let ok = SystemParametersInfoW(SPI_GETNONCLIENTMETRICS, metrics.cbSize, &mut metrics as *mut _ as *mut _, 0) != 0;
        let face = &metrics.lfMessageFont.lfFaceName;
        let end = face.iter().position(|&c| c == 0).unwrap_or(face.len());
        if ok && end > 0 {
            String::from_utf16_lossy(&face[..end])
        } else {
            "Segoe UI".to_owned()
        }
    }
}

pub struct Style<'a> {
    pub font: &'a str,
    pub font_size: f32,
    pub font_weight: FontWeight,
    pub rows: usize,
    pub background: Rgba,
}

pub struct Cell {
    pub label: String,
    pub value: String,
    pub template: &'static str,
    /// Values with a stable width line up best on the right; ones that vary a
    /// lot, like network rates, would otherwise drift away from their label.
    pub align_left: bool,
    pub color: Rgba,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CellLayout {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub label_width: i32,
}

/// Places cells top to bottom in columns of `rows`, vertically centered.
/// `sizes` is (label width, value width reserved) per cell. Returns the cells
/// and the total width.
pub fn layout(sizes: &[(i32, i32)], rows: usize, line_height: i32, height: i32, gap: i32, padding: i32) -> (Vec<CellLayout>, i32) {
    let mut cells = Vec::with_capacity(sizes.len());
    let mut x = padding;
    let top = (height - line_height * rows as i32) / 2;
    for column in sizes.chunks(rows.max(1)) {
        let label = column.iter().map(|s| s.0).max().unwrap_or(0);
        let value = column.iter().map(|s| s.1).max().unwrap_or(0);
        let width = label + gap + value;
        for row in 0..column.len() {
            cells.push(CellLayout { x, y: top + line_height * row as i32, width, label_width: label });
        }
        x += width + gap * 2;
    }
    let total = if sizes.is_empty() { 0 } else { x - gap * 2 + padding };
    (cells, total)
}

/// Premultiplied BGRA for `color` at `coverage` over a premultiplied background.
pub fn compose(coverage: u8, color: Rgba, background: Rgba) -> u32 {
    let cov = coverage as u32;
    let inv = 255 - cov;
    let div = |v: u32| (v + 127) / 255;
    let channel = |c: u8, bg: u8| div(c as u32 * cov) + div(bg as u32 * inv);
    let a = cov + div(background.a as u32 * inv);
    (a << 24) | (channel(color.r, background.r) << 16) | (channel(color.g, background.g) << 8) | channel(color.b, background.b)
}

/// Premultiplies a background and keeps alpha at 1 or more: fully transparent
/// pixels of a layered window do not receive mouse input, and the widget must
/// answer right-clicks anywhere on it.
pub fn clickable_background(background: Rgba) -> Rgba {
    let premultiply = |c: u8| ((c as u32 * background.a as u32 + 127) / 255) as u8;
    Rgba { r: premultiply(background.r), g: premultiply(background.g), b: premultiply(background.b), a: background.a.max(1) }
}

pub struct Renderer {
    dc: HDC,
    font: HFONT,
    font_key: (String, i32, i32),
    bitmap: HBITMAP,
    bits: *mut u32,
    size: (i32, i32),
}

impl Renderer {
    pub fn new() -> Renderer {
        let dc = unsafe { CreateCompatibleDC(null_mut()) };
        unsafe {
            SetBkMode(dc, TRANSPARENT as _);
            SetTextColor(dc, 0x00FF_FFFF);
        }
        Renderer { dc, font: null_mut(), font_key: (String::new(), 0, 0), bitmap: null_mut(), bits: null_mut(), size: (0, 0) }
    }

    fn set_font(&mut self, face: &str, points: f32, weight: FontWeight, dpi: u32) {
        let system;
        let face = if face == SYSTEM_FONT {
            system = system_font_name();
            system.as_str()
        } else {
            face
        };
        let height = -(points * dpi as f32 / 72.0).round() as i32;
        let weight = weight.value();
        let same = self.font_key.0 == face && self.font_key.1 == height && self.font_key.2 == weight;
        if same && !self.font.is_null() {
            return;
        }
        unsafe {
            let wide = crate::wide(face);
            let font = CreateFontW(
                height,
                0,
                0,
                0,
                weight,
                0,
                0,
                0,
                DEFAULT_CHARSET as _,
                OUT_DEFAULT_PRECIS as _,
                CLIP_DEFAULT_PRECIS as _,
                ANTIALIASED_QUALITY as _,
                DEFAULT_PITCH as _,
                wide.as_ptr(),
            );
            SelectObject(self.dc, font as _);
            if !self.font.is_null() {
                DeleteObject(self.font as _);
            }
            self.font = font;
        }
        self.font_key = (face.to_owned(), height, weight);
    }

    fn measure(&self, text: &str) -> (i32, i32) {
        let wide: Vec<u16> = text.encode_utf16().collect();
        let mut size = SIZE { cx: 0, cy: 0 };
        unsafe {
            GetTextExtentPoint32W(self.dc, wide.as_ptr(), wide.len() as i32, &mut size);
        }
        (size.cx, size.cy)
    }

    fn draw_text(&self, x: i32, y: i32, text: &str) {
        let wide: Vec<u16> = text.encode_utf16().collect();
        unsafe {
            TextOutW(self.dc, x, y, wide.as_ptr(), wide.len() as i32);
        }
    }

    fn ensure_bitmap(&mut self, width: i32, height: i32) {
        if self.size == (width, height) && !self.bitmap.is_null() {
            return;
        }
        unsafe {
            let mut info: BITMAPINFO = std::mem::zeroed();
            info.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
            info.bmiHeader.biWidth = width;
            info.bmiHeader.biHeight = -height; // top-down
            info.bmiHeader.biPlanes = 1;
            info.bmiHeader.biBitCount = 32;
            let mut bits = null_mut();
            let bitmap = CreateDIBSection(self.dc, &info, DIB_RGB_COLORS, &mut bits, null_mut(), 0);
            SelectObject(self.dc, bitmap as _);
            if !self.bitmap.is_null() {
                DeleteObject(self.bitmap as _);
            }
            self.bitmap = bitmap;
            self.bits = bits as *mut u32;
        }
        self.size = (width, height);
    }

    /// Draws the cells and returns the bitmap size. Call `present` afterwards,
    /// once the window has been moved and resized to match.
    pub fn render(&mut self, cells: &[Cell], style: &Style, dpi: u32, height: i32) -> (i32, i32) {
        self.set_font(style.font, style.font_size, style.font_weight, dpi);
        let scale = |dips: i32| dips * dpi as i32 / 96;
        let line_height = self.measure("Ag").1;
        let sizes: Vec<(i32, i32)> =
            cells.iter().map(|c| (self.measure(&c.label).0, self.measure(c.template).0.max(self.measure(&c.value).0))).collect();
        let gap = scale(4);
        let (placed, width) = layout(&sizes, style.rows, line_height, height, gap, gap);
        let width = width.max(1);
        self.ensure_bitmap(width, height);

        let pixels = unsafe { std::slice::from_raw_parts_mut(self.bits, (width * height) as usize) };
        pixels.fill(0);
        for (cell, at) in cells.iter().zip(&placed) {
            self.draw_text(at.x, at.y, &cell.label);
            let value_x = if cell.align_left { at.x + at.label_width + gap } else { at.x + at.width - self.measure(&cell.value).0 };
            self.draw_text(value_x, at.y, &cell.value);
        }
        unsafe {
            GdiFlush(); // GDI batches calls; finish before touching the bits.
        }

        let background = clickable_background(style.background);
        let idle = compose(0, Rgba::TRANSPARENT, background);
        // Pixels are converted per cell, because each cell has its own color.
        let mut converted = vec![false; pixels.len()];
        for (cell, at) in cells.iter().zip(&placed) {
            let top = at.y.clamp(0, height);
            let bottom = (at.y + line_height).clamp(0, height);
            for y in top..bottom {
                for x in at.x.clamp(0, width)..(at.x + at.width).clamp(0, width) {
                    let i = (y * width + x) as usize;
                    pixels[i] = compose(pixels[i] as u8, cell.color, background);
                    converted[i] = true;
                }
            }
        }
        for (pixel, done) in pixels.iter_mut().zip(&converted) {
            if !done {
                *pixel = idle;
            }
        }
        (width, height)
    }

    pub fn present(&self, hwnd: HWND) -> bool {
        let size = SIZE { cx: self.size.0, cy: self.size.1 };
        let source = POINT { x: 0, y: 0 };
        let blend = BLENDFUNCTION { BlendOp: AC_SRC_OVER as u8, BlendFlags: 0, SourceConstantAlpha: 255, AlphaFormat: AC_SRC_ALPHA as u8 };
        unsafe { UpdateLayeredWindow(hwnd, null_mut(), null(), &size, self.dc, &source, 0, &blend, ULW_ALPHA) != 0 }
    }
}

impl Drop for Renderer {
    fn drop(&mut self) {
        unsafe {
            DeleteDC(self.dc);
            if !self.bitmap.is_null() {
                DeleteObject(self.bitmap as _);
            }
            if !self.font.is_null() {
                DeleteObject(self.font as _);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const WHITE: Rgba = Rgba { r: 255, g: 255, b: 255, a: 255 };

    #[test]
    fn layout_fills_columns_and_centers_vertically() {
        let (cells, width) = layout(&[(10, 30), (12, 20), (8, 10)], 2, 15, 48, 4, 4);
        assert_eq!(cells[0], CellLayout { x: 4, y: 9, width: 46, label_width: 12 });
        assert_eq!(cells[1], CellLayout { x: 4, y: 24, width: 46, label_width: 12 });
        assert_eq!(cells[2], CellLayout { x: 58, y: 9, width: 22, label_width: 8 });
        assert_eq!(width, 84);
    }

    #[test]
    fn empty_layout_has_no_width() {
        assert_eq!(layout(&[], 2, 15, 48, 4, 4).1, 0);
    }

    #[test]
    fn compose_on_transparent_background() {
        let bg = clickable_background(Rgba::TRANSPARENT);
        assert_eq!(compose(0, WHITE, bg), 0x0100_0000);
        assert_eq!(compose(255, WHITE, bg), 0xFFFF_FFFF);
        // Premultiplied: no color channel may exceed alpha.
        let px = compose(128, Rgba { r: 255, g: 0, b: 0, a: 255 }, bg);
        assert_eq!(px >> 24, 128);
        assert_eq!((px >> 16) & 0xFF, 128);
        assert_eq!(px & 0xFFFF, 0);
    }

    #[test]
    fn compose_over_translucent_background_stays_premultiplied() {
        let bg = clickable_background(Rgba { r: 200, g: 100, b: 50, a: 128 });
        for coverage in [0u8, 1, 64, 128, 200, 254, 255] {
            let px = compose(coverage, WHITE, bg);
            let a = px >> 24;
            assert!((px >> 16) & 0xFF <= a && (px >> 8) & 0xFF <= a && px & 0xFF <= a);
        }
        assert_eq!(compose(0, WHITE, bg) >> 24, 128);
    }

    #[test]
    fn clickable_background_is_never_fully_transparent() {
        assert_eq!(clickable_background(Rgba::TRANSPARENT).a, 1);
    }
}

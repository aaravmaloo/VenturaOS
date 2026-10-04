use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicBool, Ordering};
use crate::font;

pub const CELL_WIDTH: usize = font::GLYPH_WIDTH;
pub const CELL_HEIGHT: usize = font::GLYPH_HEIGHT * 2; // each glyph row drawn twice

const FG_COLOR: u32 = 0x00CC_CCCC; // light gray, same value in RGB and BGR layouts
const BG_COLOR: u32 = 0x0000_0000;

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum PixelFormat {
    Rgb,
    Bgr,
    Bitmask,
    BltOnly,
}

impl PixelFormat {
    pub fn from_uefi(value: u32) -> Self {
        match value {
            0 => PixelFormat::Rgb,
            1 => PixelFormat::Bgr,
            2 => PixelFormat::Bitmask,
            _ => PixelFormat::BltOnly,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            PixelFormat::Rgb => "RGBX-8888",
            PixelFormat::Bgr => "BGRX-8888",
            PixelFormat::Bitmask => "BITMASK",
            PixelFormat::BltOnly => "BLT_ONLY",
        }
    }
}

#[derive(Copy, Clone, Debug)]
pub struct FramebufferInfo {
    pub phys_base: u64,
    pub size_bytes: u64,
    pub width: usize,
    pub height: usize,
    pub stride: usize, // pixels per scan line
    pub format: PixelFormat,
}

struct Console {
    info: Option<FramebufferInfo>,
    cols: usize,
    rows: usize,
    col: usize,
    row: usize,
}

struct SyncCell<T>(UnsafeCell<T>);
unsafe impl<T> Sync for SyncCell<T> {}

static CONSOLE: SyncCell<Console> = SyncCell(UnsafeCell::new(Console {
    info: None,
    cols: 0,
    rows: 0,
    col: 0,
    row: 0,
}));
static ACTIVE: AtomicBool = AtomicBool::new(false);

pub fn set_info(info: FramebufferInfo) {
    unsafe { (*CONSOLE.0.get()).info = Some(info) };
}

pub fn info() -> Option<FramebufferInfo> {
    unsafe { (*CONSOLE.0.get()).info }
}

pub fn is_active() -> bool {
    ACTIVE.load(Ordering::Relaxed)
}

pub fn init() -> bool {
    let console = unsafe { &mut *CONSOLE.0.get() };
    let info = match console.info {
        Some(i) if i.format != PixelFormat::BltOnly && i.width > 0 && i.height > 0 => i,
        _ => return false,
    };

    console.cols = info.width / CELL_WIDTH;
    console.rows = info.height / CELL_HEIGHT;
    console.col = 0;
    console.row = 0;

    unsafe {
        let fb = info.phys_base as *mut u32;
        for i in 0..(info.stride * info.height) {
            fb.add(i).write_volatile(BG_COLOR);
        }
    }

    ACTIVE.store(true, Ordering::SeqCst);
    true
}

pub fn console_size() -> (usize, usize) {
    let console = unsafe { &*CONSOLE.0.get() };
    (console.cols, console.rows)
}

unsafe fn draw_glyph(info: &FramebufferInfo, col: usize, row: usize, byte: u8) {
    let glyph = font::glyph(byte);
    let fb = info.phys_base as *mut u32;
    let x0 = col * CELL_WIDTH;
    let y0 = row * CELL_HEIGHT;

    for gy in 0..font::GLYPH_HEIGHT {
        let bits = glyph[gy];
        for dy in 0..2 {
            let line = fb.add((y0 + gy * 2 + dy) * info.stride + x0);
            for gx in 0..font::GLYPH_WIDTH {
                let color = if (bits >> gx) & 1 != 0 { FG_COLOR } else { BG_COLOR };
                line.add(gx).write_volatile(color);
            }
        }
    }
}

unsafe fn scroll(info: &FramebufferInfo, rows: usize) {
    let fb = info.phys_base as *mut u32;
    let row_pixels = info.stride * CELL_HEIGHT;
    let text_pixels = row_pixels * rows;

    core::ptr::copy(fb.add(row_pixels), fb, text_pixels - row_pixels);
    for i in (text_pixels - row_pixels)..text_pixels {
        fb.add(i).write_volatile(BG_COLOR);
    }
}

pub fn write_byte(byte: u8) {
    if !is_active() {
        return;
    }
    let console = unsafe { &mut *CONSOLE.0.get() };
    let info = match console.info {
        Some(i) => i,
        None => return,
    };

    match byte {
        b'\n' => {
            console.col = 0;
            console.row += 1;
        }
        b'\r' => console.col = 0,
        b => {
            if console.col >= console.cols {
                console.col = 0;
                console.row += 1;
            }
            if console.row >= console.rows {
                unsafe { scroll(&info, console.rows) };
                console.row = console.rows - 1;
            }
            unsafe { draw_glyph(&info, console.col, console.row, b) };
            console.col += 1;
        }
    }

    if console.row >= console.rows {
        unsafe { scroll(&info, console.rows) };
        console.row = console.rows - 1;
    }
}

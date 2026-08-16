use core::fmt::{self, Write};
use core::sync::atomic::{AtomicBool, AtomicPtr, AtomicU64, Ordering};
use crate::EfiSimpleTextOutput;
use crate::platform;

static CON_OUT: AtomicPtr<EfiSimpleTextOutput> = AtomicPtr::new(core::ptr::null_mut());
static UEFI_CONSOLE_ACTIVE: AtomicBool = AtomicBool::new(true);
static VGA_ACTIVE: AtomicBool = AtomicBool::new(false);

const VGA_BUFFER: *mut u16 = 0x000B_8000 as *mut u16;
const VGA_WIDTH: usize = 80;
const VGA_HEIGHT: usize = 25;
const COLOR_WHITE_ON_BLACK: u16 = 0x0F00;

static VGA_COL: AtomicU64 = AtomicU64::new(0);
static VGA_ROW: AtomicU64 = AtomicU64::new(0);

pub unsafe fn init(out: *mut EfiSimpleTextOutput) {
    CON_OUT.store(out, Ordering::SeqCst);
    
    // Initialize COM1 serial port (0x3F8) for hardware logging
    platform::outb(platform::COM1_PORT + 1, 0x00); // Disable interrupts
    platform::outb(platform::COM1_PORT + 3, 0x80); // Enable DLAB
    platform::outb(platform::COM1_PORT + 0, 0x03); // Set baud rate divisor to 3 (38400 baud)
    platform::outb(platform::COM1_PORT + 1, 0x00);
    platform::outb(platform::COM1_PORT + 3, 0x03); // 8 bits, no parity, one stop bit
    platform::outb(platform::COM1_PORT + 2, 0xC7); // Enable FIFO, clear them, 14-byte threshold
    platform::outb(platform::COM1_PORT + 4, 0x0B); // IRQs enabled, RTS/DSR set
}

pub fn clear_vga() {
    unsafe {
        for i in 0..(VGA_WIDTH * VGA_HEIGHT) {
            VGA_BUFFER.add(i).write_volatile(b' ' as u16 | COLOR_WHITE_ON_BLACK);
        }
    }
    VGA_COL.store(0, Ordering::Relaxed);
    VGA_ROW.store(0, Ordering::Relaxed);
}

pub fn enable_vga_console() {
    UEFI_CONSOLE_ACTIVE.store(false, Ordering::SeqCst);
    clear_vga();
    VGA_ACTIVE.store(true, Ordering::SeqCst);
}

pub fn disable_uefi_console() {
    UEFI_CONSOLE_ACTIVE.store(false, Ordering::SeqCst);
}

#[inline(always)]
pub fn write_serial_byte(byte: u8) {
    unsafe {
        let mut timeout = 10_000usize;
        while (platform::inb(platform::COM1_PORT + 5) & 0x20) == 0 && timeout > 0 {
            timeout -= 1;
        }
        platform::outb(platform::COM1_PORT, byte);
    }
}

fn scroll_vga() {
    unsafe {
        for r in 1..VGA_HEIGHT {
            for c in 0..VGA_WIDTH {
                let src_idx = r * VGA_WIDTH + c;
                let dst_idx = (r - 1) * VGA_WIDTH + c;
                let val = VGA_BUFFER.add(src_idx).read_volatile();
                VGA_BUFFER.add(dst_idx).write_volatile(val);
            }
        }
        for c in 0..VGA_WIDTH {
            let idx = (VGA_HEIGHT - 1) * VGA_WIDTH + c;
            VGA_BUFFER.add(idx).write_volatile(b' ' as u16 | COLOR_WHITE_ON_BLACK);
        }
    }
}

pub fn write_vga_byte(byte: u8) {
    let mut col = VGA_COL.load(Ordering::Relaxed) as usize;
    let mut row = VGA_ROW.load(Ordering::Relaxed) as usize;

    match byte {
        b'\n' => {
            col = 0;
            row += 1;
        }
        b'\r' => {
            col = 0;
        }
        b => {
            if col >= VGA_WIDTH {
                col = 0;
                row += 1;
            }

            if row >= VGA_HEIGHT {
                scroll_vga();
                row = VGA_HEIGHT - 1;
            }

            let index = row * VGA_WIDTH + col;
            unsafe {
                VGA_BUFFER.add(index).write_volatile((b as u16) | COLOR_WHITE_ON_BLACK);
            }
            col += 1;
        }
    }

    if row >= VGA_HEIGHT {
        scroll_vga();
        row = VGA_HEIGHT - 1;
    }

    VGA_COL.store(col as u64, Ordering::Relaxed);
    VGA_ROW.store(row as u64, Ordering::Relaxed);
}

pub fn write_str_raw(s: &str) {
    // 1. Send all characters to COM1 serial port (hardware I/O)
    for byte in s.bytes() {
        if byte == b'\n' {
            write_serial_byte(b'\r');
        }
        write_serial_byte(byte);
    }

    // 2. Output to VGA text buffer (0xB8000) post-CR3 switch
    if VGA_ACTIVE.load(Ordering::Relaxed) {
        for byte in s.bytes() {
            write_vga_byte(byte);
        }
    }

    // 3. Output to UEFI screen console while active during early boot
    if UEFI_CONSOLE_ACTIVE.load(Ordering::Relaxed) {
        let out = CON_OUT.load(Ordering::Relaxed);
        if !out.is_null() {
            for ch in s.chars() {
                if ch == '\n' {
                    let cr_buf: [u16; 2] = ['\r' as u16, 0u16];
                    unsafe {
                        ((*out).output_string)(out, cr_buf.as_ptr());
                    }
                }
                let buf: [u16; 2] = [ch as u16, 0u16];
                unsafe {
                    ((*out).output_string)(out, buf.as_ptr());
                }
            }
        }
    }
}

pub struct KernelWriter;

impl Write for KernelWriter {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        write_str_raw(s);
        Ok(())
    }
}

pub fn _klog(args: fmt::Arguments) {
    let mut w = KernelWriter;
    let _ = fmt::write(&mut w, args);
    let _ = w.write_str("\r\n");
}

#[macro_export]
macro_rules! klog {
    ($($arg:tt)*) => {
        $crate::logger::_klog(::core::format_args!($($arg)*))
    };
}

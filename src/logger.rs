use core::fmt::{self, Write};
use core::sync::atomic::{AtomicBool, AtomicPtr, Ordering};
use crate::EfiSimpleTextOutput;
use crate::platform;

static CON_OUT: AtomicPtr<EfiSimpleTextOutput> = AtomicPtr::new(core::ptr::null_mut());
static UEFI_CONSOLE_ACTIVE: AtomicBool = AtomicBool::new(true);

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

pub fn disable_uefi_console() {
    UEFI_CONSOLE_ACTIVE.store(false, Ordering::SeqCst);
}

#[inline(always)]
pub fn write_serial_byte(byte: u8) {
    unsafe {
        // Wait for serial transmit buffer to be empty (bit 5 of Line Status Register 0x3FD)
        let mut timeout = 10_000usize;
        while (platform::inb(platform::COM1_PORT + 5) & 0x20) == 0 && timeout > 0 {
            timeout -= 1;
        }
        platform::outb(platform::COM1_PORT, byte);
    }
}

pub fn write_str_raw(s: &str) {
    // 1. Send characters to COM1 serial port (hardware I/O)
    for byte in s.bytes() {
        if byte == b'\n' {
            write_serial_byte(b'\r');
        }
        write_serial_byte(byte);
    }

    // 2. Output to UEFI screen console while active (expanding \n to \r\n for column alignment)
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

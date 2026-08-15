use core::fmt::{self, Write};
use core::sync::atomic::{AtomicPtr, Ordering};
use crate::EfiSimpleTextOutput;

static CON_OUT: AtomicPtr<EfiSimpleTextOutput> = AtomicPtr::new(core::ptr::null_mut());

pub unsafe fn init(out: *mut EfiSimpleTextOutput) {
    CON_OUT.store(out, Ordering::SeqCst);
}

pub fn write_str_raw(s: &str) {
    let out = CON_OUT.load(Ordering::SeqCst);
    if out.is_null() { return; }
    for ch in s.chars() {
        let buf: [u16; 2] = [ch as u16, 0u16];
        unsafe { ((*out).output_string)(out, buf.as_ptr()); }
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

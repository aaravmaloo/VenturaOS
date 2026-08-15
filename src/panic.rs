use core::fmt::Write;
use crate::logger::KernelWriter;
use crate::platform;

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    platform::cli();

    let mut w = KernelWriter;

    let _ = w.write_str("\r\n[KERNEL PANIC]\r\n");

    let _ = w.write_str("  message  : ");
    let _ = core::fmt::write(&mut w, core::format_args!("{}", info.message()));
    let _ = w.write_str("\r\n");

    if let Some(loc) = info.location() {
        let _ = w.write_str("  location : ");
        let _ = w.write_str(loc.file());
        let _ = core::fmt::write(&mut w, core::format_args!(":{}\r\n", loc.line()));
    }

    let _ = w.write_str("\r\n");

    platform::halt()
}

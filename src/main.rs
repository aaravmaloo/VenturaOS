#![no_std]
#![no_main]
#![allow(private_interfaces)]

pub mod apic;
pub mod gdt;
pub mod idt;
pub mod logger;
pub mod panic;
pub mod platform;
pub mod timer;

type EfiHandle = *mut u8;
type EfiStatus = usize;

#[repr(C)]
struct EfiTableHeader {
    signature:   u64,
    revision:    u32,
    header_size: u32,
    crc32:       u32,
    reserved:    u32,
}

#[repr(C)]
pub struct EfiSimpleTextOutput {
    reset: unsafe extern "efiapi" fn(
        this:                  *mut EfiSimpleTextOutput,
        extended_verification: u8,
    ) -> EfiStatus,

    pub output_string: unsafe extern "efiapi" fn(
        this:   *mut EfiSimpleTextOutput,
        string: *const u16,
    ) -> EfiStatus,

    test_string:         unsafe extern "efiapi" fn(*mut EfiSimpleTextOutput, *const u16) -> EfiStatus,
    query_mode:          unsafe extern "efiapi" fn(*mut EfiSimpleTextOutput, usize, *mut usize, *mut usize) -> EfiStatus,
    set_mode:            unsafe extern "efiapi" fn(*mut EfiSimpleTextOutput, usize) -> EfiStatus,
    set_attribute:       unsafe extern "efiapi" fn(*mut EfiSimpleTextOutput, usize) -> EfiStatus,
    clear_screen:        unsafe extern "efiapi" fn(*mut EfiSimpleTextOutput) -> EfiStatus,
    set_cursor_position: unsafe extern "efiapi" fn(*mut EfiSimpleTextOutput, usize, usize) -> EfiStatus,
    enable_cursor:       unsafe extern "efiapi" fn(*mut EfiSimpleTextOutput, u8) -> EfiStatus,
    mode:                *mut u8,
}

#[repr(C)]
struct EfiSystemTable {
    hdr:                EfiTableHeader,
    firmware_vendor:    *const u16,
    firmware_revision:  u32,
    console_in_handle:  EfiHandle,
    con_in:             *mut u8,
    console_out_handle: EfiHandle,
    con_out:            *mut EfiSimpleTextOutput,
}

#[macro_export]
macro_rules! utf16 {
    ($s:literal) => {{
        const SRC: &[u8] = $s.as_bytes();
        const LEN: usize = SRC.len() + 1;
        const ARR: [u16; LEN] = {
            let mut a = [0u16; LEN];
            let mut i = 0usize;
            while i < SRC.len() {
                a[i] = SRC[i] as u16;
                i += 1;
            }
            a
        };
        ARR
    }};
}

#[inline(always)]
pub unsafe fn print(out: *mut EfiSimpleTextOutput, wstr: &[u16]) {
    ((*out).output_string)(out, wstr.as_ptr());
}

#[no_mangle]
pub extern "efiapi" fn efi_main(
    _image_handle: EfiHandle,
    system_table:  *mut EfiSystemTable,
) -> EfiStatus {
    unsafe {
        let out = (*system_table).con_out;
        ((*out).reset)(out, 0u8);
        logger::init(out);
    }

    kernel_main()
}

fn kernel_main() -> ! {
    klog!("[BOOT] Ventura kernel starting (x86_64)");

    initialize_logging();
    initialize_platform();
    initialize_gdt();
    initialize_idt();
    initialize_apic();
    initialize_timer();

    platform::sti();

    klog!("[BOOT] Kernel initialization complete");
    klog!("[KERNEL] entering main loop");

    kernel_main_loop()
}

fn initialize_logging() {
    klog!("[BOOT] Logging initialized");
}

fn initialize_platform() {
    klog!("[BOOT] Platform: x86_64 / UEFI boot services");
}

fn initialize_gdt() {
    gdt::init();
    klog!("[BOOT] GDT initialized");
    klog!("[BOOT] TSS initialized");
}

fn initialize_idt() {
    idt::init();
    klog!("[BOOT] IDT initialized");
    klog!("[BOOT] Exception handlers installed");
}

fn initialize_apic() {
    unsafe {
        apic::disable_pic();
        apic::init_lapic();
        apic::init_ioapic();
    }
    klog!("[BOOT] Local APIC initialized");
    klog!("[BOOT] I/O APIC initialized");
    klog!("[BOOT] Hardware IRQ routing enabled");
}

fn initialize_timer() {
    timer::init();
}

fn kernel_main_loop() -> ! {
    loop {
        platform::hlt();
    }
}

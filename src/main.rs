#![no_std]
#![no_main]
#![allow(private_interfaces)]

extern crate alloc;

pub mod apic;
pub mod context;
pub mod gdt;
pub mod heap;
pub mod idt;
pub mod logger;
pub mod memory;
pub mod panic;
pub mod platform;
pub mod pmm;
pub mod timer;
pub mod vmm;

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
pub struct EfiBootServices {
    hdr: EfiTableHeader,
    raise_tpl: usize,
    restore_tpl: usize,
    allocate_pages: usize,
    free_pages: usize,
    pub get_memory_map: unsafe extern "efiapi" fn(
        memory_map_size: *mut usize,
        memory_map: *mut u8,
        map_key: *mut usize,
        descriptor_size: *mut usize,
        descriptor_version: *mut u32,
    ) -> EfiStatus,
    allocate_pool: usize,
    free_pool: usize,
    create_event: usize,
    set_timer: usize,
    wait_for_event: usize,
    signal_event: usize,
    close_event: usize,
    check_event: usize,
    install_protocol_interface: usize,
    reinstall_protocol_interface: usize,
    uninstall_protocol_interface: usize,
    handle_protocol: usize,
    reserved: usize,
    register_protocol_notify: usize,
    locate_handle: usize,
    locate_device_path: usize,
    install_configuration_table: usize,
    load_image: usize,
    start_image: usize,
    exit: usize,
    unload_image: usize,
    exit_boot_services: usize,
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
    hdr:                   EfiTableHeader,
    firmware_vendor:       *const u16,
    firmware_revision:     u32,
    console_in_handle:     EfiHandle,
    con_in:                *mut u8,
    console_out_handle:    EfiHandle,
    con_out:               *mut EfiSimpleTextOutput,
    standard_error_handle: EfiHandle,
    std_err:               *mut u8,
    runtime_services:      *mut u8,
    boot_services:         *mut EfiBootServices,
}

struct SyncCell<T>(core::cell::UnsafeCell<T>);
unsafe impl<T> Sync for SyncCell<T> {}

static RAW_MMAP_BUFFER: SyncCell<[u8; 16384]> = SyncCell(core::cell::UnsafeCell::new([0; 16384]));

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

        let bs = (*system_table).boot_services;
        let buf = &mut *RAW_MMAP_BUFFER.0.get();
        let mut map_size: usize = buf.len();
        let mut map_key: usize = 0;
        let mut desc_size: usize = 0;
        let mut desc_version: u32 = 0;

        let status = ((*bs).get_memory_map)(
            &mut map_size,
            buf.as_mut_ptr(),
            &mut map_key,
            &mut desc_size,
            &mut desc_version,
        );

        if status == 0 && desc_size > 0 {
            let count = map_size / desc_size;
            memory::init_from_uefi(&buf[..map_size], desc_size, count);
        }
    }

    kernel_main()
}

fn kernel_main() -> ! {
    // Disable interrupts immediately.  UEFI hands control with IF=1.
    // Until our IDT *and* APIC are fully initialized, any hardware interrupt
    // would be dispatched to our half-ready IDT, which calls apic::eoi() on
    // an uninitialized LAPIC and never sends a PIC EOI → infinite IRQ loop →
    // reboot.  without_interrupts() in pmm/vmm re-enables IF on exit, making
    // the window between calls unsafe.  A single cli() here keeps IF=0
    // throughout init; we restore it with sti() only after APIC+timer are up.
    platform::cli();

    klog!("[BOOT] Ventura kernel starting (x86_64)");

    initialize_logging();
    initialize_platform();

    // PMM must come first — it discovers usable physical frames.
    initialize_pmm();

    // GDT and IDT must be installed BEFORE the CR3 switch so that
    // Ventura's own exception handlers are live when vmm::init() calls
    // write_cr3().  Without this, any post-switch fault uses UEFI's IDT
    // whose handlers access UEFI's now-gone page tables → triple fault.
    initialize_gdt();
    initialize_idt();

    // VMM (page tables + CR3 switch) and heap come after GDT/IDT.
    initialize_vmm_and_heap();

    initialize_apic();
    initialize_timer();

    // Everything initialized — safe to enable interrupts now.
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

fn initialize_pmm() {
    memory::log_diagnostics();
    pmm::init();
    pmm::test_allocator();
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

fn initialize_vmm_and_heap() {
    vmm::init();
    heap::init();
    memory::run_self_tests();
    context::run_self_tests();
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

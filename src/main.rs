#![no_std]
#![no_main]
#![allow(private_interfaces)]

extern crate alloc;

pub mod acpi;
pub mod address_space;
pub mod apic;
pub mod context;
pub mod font;
pub mod framebuffer;
pub mod gdt;
pub mod heap;
pub mod idt;
pub mod logger;
pub mod memory;
pub mod panic;
pub mod platform;
pub mod pmm;
pub mod process;
pub mod thread;
pub mod timer;
pub mod vmm;

type EfiHandle = *mut u8;
type EfiStatus = usize;

const EFI_SUCCESS: EfiStatus = 0;

#[repr(C)]
#[derive(Copy, Clone, PartialEq, Eq)]
struct EfiGuid {
    data1: u32,
    data2: u16,
    data3: u16,
    data4: [u8; 8],
}

const EFI_GRAPHICS_OUTPUT_PROTOCOL_GUID: EfiGuid = EfiGuid {
    data1: 0x9042_a9de, data2: 0x23dc, data3: 0x4a38,
    data4: [0x96, 0xfb, 0x7a, 0xde, 0xd0, 0x80, 0x51, 0x6a],
};
const EFI_ACPI_20_TABLE_GUID: EfiGuid = EfiGuid {
    data1: 0x8868_e871, data2: 0xe4f1, data3: 0x11d3,
    data4: [0xbc, 0x22, 0x00, 0x80, 0xc7, 0x3c, 0x88, 0x81],
};
const EFI_ACPI_10_TABLE_GUID: EfiGuid = EfiGuid {
    data1: 0xeb9d_2d30, data2: 0x2d88, data3: 0x11d3,
    data4: [0x9a, 0x16, 0x00, 0x90, 0x27, 0x3f, 0xc1, 0x4d],
};

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
    exit_boot_services: unsafe extern "efiapi" fn(
        image_handle: EfiHandle,
        map_key: usize,
    ) -> EfiStatus,
    get_next_monotonic_count: usize,
    stall: usize,
    set_watchdog_timer: usize,
    connect_controller: usize,
    disconnect_controller: usize,
    open_protocol: usize,
    close_protocol: usize,
    open_protocol_information: usize,
    protocols_per_handle: usize,
    locate_handle_buffer: usize,
    locate_protocol: unsafe extern "efiapi" fn(
        protocol: *const EfiGuid,
        registration: *mut u8,
        interface: *mut *mut u8,
    ) -> EfiStatus,
}

#[repr(C)]
struct EfiGraphicsOutputProtocol {
    query_mode: usize,
    set_mode: usize,
    blt: usize,
    mode: *const EfiGraphicsOutputMode,
}

#[repr(C)]
struct EfiGraphicsOutputMode {
    max_mode: u32,
    mode: u32,
    info: *const EfiGraphicsOutputModeInfo,
    size_of_info: usize,
    frame_buffer_base: u64,
    frame_buffer_size: usize,
}

#[repr(C)]
struct EfiGraphicsOutputModeInfo {
    version: u32,
    horizontal_resolution: u32,
    vertical_resolution: u32,
    pixel_format: u32,
    pixel_information: [u32; 4],
    pixels_per_scan_line: u32,
}

#[repr(C)]
struct EfiConfigurationTable {
    vendor_guid: EfiGuid,
    vendor_table: *const u8,
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
    number_of_table_entries: usize,
    configuration_table:   *const EfiConfigurationTable,
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

unsafe fn find_gop(bs: *mut EfiBootServices) {
    let mut gop: *mut u8 = core::ptr::null_mut();
    let status = ((*bs).locate_protocol)(&EFI_GRAPHICS_OUTPUT_PROTOCOL_GUID, core::ptr::null_mut(), &mut gop);
    if status != EFI_SUCCESS || gop.is_null() {
        klog!("[BOOT WARN] UEFI GOP not found, screen output stops after ExitBootServices");
        return;
    }

    let mode = &*(*(gop as *const EfiGraphicsOutputProtocol)).mode;
    let info = &*mode.info;
    let fb = framebuffer::FramebufferInfo {
        phys_base: mode.frame_buffer_base,
        size_bytes: mode.frame_buffer_size as u64,
        width: info.horizontal_resolution as usize,
        height: info.vertical_resolution as usize,
        stride: info.pixels_per_scan_line as usize,
        format: framebuffer::PixelFormat::from_uefi(info.pixel_format),
    };
    framebuffer::set_info(fb);
    klog!("[BOOT] GOP framebuffer {}x{} {} at {:#x}", fb.width, fb.height, fb.format.name(), fb.phys_base);
}

unsafe fn find_rsdp(system_table: *mut EfiSystemTable) {
    let count = (*system_table).number_of_table_entries;
    let tables = (*system_table).configuration_table;
    let mut rsdp = 0u64;

    // Prefer the ACPI 2.0 RSDP (XSDT) over the ACPI 1.0 one (RSDT)
    for i in 0..count {
        let t = &*tables.add(i);
        if t.vendor_guid == EFI_ACPI_20_TABLE_GUID {
            rsdp = t.vendor_table as u64;
            break;
        }
        if t.vendor_guid == EFI_ACPI_10_TABLE_GUID && rsdp == 0 {
            rsdp = t.vendor_table as u64;
        }
    }

    if rsdp == 0 {
        klog!("[BOOT WARN] ACPI RSDP not found in UEFI configuration table");
        return;
    }
    acpi::set_rsdp(rsdp);
}

unsafe fn exit_boot_services(image_handle: EfiHandle, bs: *mut EfiBootServices) -> Option<(usize, usize)> {
    let buf = &mut *RAW_MMAP_BUFFER.0.get();

    // No logging between GetMemoryMap and ExitBootServices: it can change the map key
    for _ in 0..4 {
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
        if status != EFI_SUCCESS || desc_size == 0 {
            return None;
        }

        if ((*bs).exit_boot_services)(image_handle, map_key) == EFI_SUCCESS {
            return Some((map_size, desc_size));
        }
    }
    None
}

#[no_mangle]
pub extern "efiapi" fn efi_main(
    image_handle: EfiHandle,
    system_table:  *mut EfiSystemTable,
) -> EfiStatus {
    unsafe {
        let out = (*system_table).con_out;
        ((*out).reset)(out, 0u8);
        logger::init(out);

        let bs = (*system_table).boot_services;
        find_gop(bs);
        find_rsdp(system_table);

        klog!("[BOOT] Exiting UEFI boot services...");
        let (map_size, desc_size) = match exit_boot_services(image_handle, bs) {
            Some(m) => m,
            None => {
                klog!("[BOOT PANIC] ExitBootServices failed!");
                platform::halt();
            }
        };

        // Firmware no longer owns the machine: no UEFI calls past this point
        platform::cli();
        logger::disable_uefi_console();
        if !framebuffer::init() {
            klog!("[BOOT WARN] No linear framebuffer, logging to COM1 only");
        }

        let buf = &*RAW_MMAP_BUFFER.0.get();
        memory::init_from_uefi(&buf[..map_size], desc_size, map_size / desc_size);
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
    initialize_acpi();

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
    klog!("[BOOT] Platform: x86_64 / UTM Q35 (UEFI boot services exited)");
    if let Some(fb) = framebuffer::info() {
        let (cols, rows) = framebuffer::console_size();
        klog!("[BOOT] Framebuffer console: {}x{} pixels, {}x{} text", fb.width, fb.height, cols, rows);
    }
}

fn initialize_acpi() {
    if let Err(e) = acpi::init() {
        klog!("[ACPI WARN] ACPI init failed: {:?}", e);
    }
    apic::configure_from_acpi();
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
    address_space::run_self_tests();
    context::run_self_tests();
    thread::run_self_tests();
    process::run_self_tests();
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

use core::cell::UnsafeCell;
use crate::klog;

pub const MAX_CPUS: usize = 64;
pub const MAX_IOAPICS: usize = 4;
pub const MAX_OVERRIDES: usize = 16;

const MADT_LOCAL_APIC: u8 = 0;
const MADT_IO_APIC: u8 = 1;
const MADT_INTERRUPT_OVERRIDE: u8 = 2;
const MADT_LOCAL_APIC_ADDRESS_OVERRIDE: u8 = 5;

// MPS INTI flags (ACPI spec 5.2.12.5)
pub const POLARITY_MASK: u16 = 0x3;
pub const POLARITY_ACTIVE_LOW: u16 = 0x3;
pub const TRIGGER_MASK: u16 = 0xC;
pub const TRIGGER_LEVEL: u16 = 0xC;

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum AcpiError {
    NoRsdp,
    BadRsdpChecksum,
    BadTableChecksum,
    NoMadt,
}

#[allow(dead_code)]
#[repr(C, packed)]
struct Rsdp {
    signature: [u8; 8],
    checksum: u8,
    oem_id: [u8; 6],
    revision: u8,
    rsdt_address: u32,
    // ACPI 2.0+
    length: u32,
    xsdt_address: u64,
    extended_checksum: u8,
    reserved: [u8; 3],
}

#[allow(dead_code)]
#[repr(C, packed)]
struct SdtHeader {
    signature: [u8; 4],
    length: u32,
    revision: u8,
    checksum: u8,
    oem_id: [u8; 6],
    oem_table_id: [u8; 8],
    oem_revision: u32,
    creator_id: u32,
    creator_revision: u32,
}

#[derive(Copy, Clone, Debug)]
pub struct IoApic {
    pub id: u8,
    pub address: u64,
    pub gsi_base: u32,
}

#[derive(Copy, Clone, Debug)]
pub struct InterruptOverride {
    pub isa_irq: u8,
    pub gsi: u32,
    pub flags: u16,
}

pub struct MadtInfo {
    pub lapic_address: u64,
    pub pcat_compat: bool,
    pub cpu_apic_ids: [u8; MAX_CPUS],
    pub cpu_count: usize,
    pub ioapics: [IoApic; MAX_IOAPICS],
    pub ioapic_count: usize,
    pub overrides: [InterruptOverride; MAX_OVERRIDES],
    pub override_count: usize,
}

impl MadtInfo {
    const fn new() -> Self {
        Self {
            lapic_address: 0,
            pcat_compat: false,
            cpu_apic_ids: [0; MAX_CPUS],
            cpu_count: 0,
            ioapics: [IoApic { id: 0, address: 0, gsi_base: 0 }; MAX_IOAPICS],
            ioapic_count: 0,
            overrides: [InterruptOverride { isa_irq: 0, gsi: 0, flags: 0 }; MAX_OVERRIDES],
            override_count: 0,
        }
    }
}

struct SyncCell<T>(UnsafeCell<T>);
unsafe impl<T> Sync for SyncCell<T> {}

static RSDP_ADDRESS: SyncCell<u64> = SyncCell(UnsafeCell::new(0));
static MADT: SyncCell<MadtInfo> = SyncCell(UnsafeCell::new(MadtInfo::new()));
static MADT_VALID: SyncCell<bool> = SyncCell(UnsafeCell::new(false));

pub fn set_rsdp(address: u64) {
    unsafe { *RSDP_ADDRESS.0.get() = address };
}

pub fn madt() -> Option<&'static MadtInfo> {
    unsafe {
        if *MADT_VALID.0.get() {
            Some(&*MADT.0.get())
        } else {
            None
        }
    }
}

fn checksum_ok(addr: u64, len: usize) -> bool {
    let bytes = unsafe { core::slice::from_raw_parts(addr as *const u8, len) };
    bytes.iter().fold(0u8, |acc, b| acc.wrapping_add(*b)) == 0
}

unsafe fn read_header(addr: u64) -> SdtHeader {
    core::ptr::read_unaligned(addr as *const SdtHeader)
}

fn ascii(bytes: &[u8]) -> &str {
    core::str::from_utf8(bytes).unwrap_or("?").trim_end()
}

// Must run while physical memory is identity mapped (UEFI or Ventura page tables)
pub fn init() -> Result<(), AcpiError> {
    let rsdp_addr = unsafe { *RSDP_ADDRESS.0.get() };
    if rsdp_addr == 0 {
        return Err(AcpiError::NoRsdp);
    }

    let rsdp = unsafe { core::ptr::read_unaligned(rsdp_addr as *const Rsdp) };
    if &rsdp.signature != b"RSD PTR " || !checksum_ok(rsdp_addr, 20) {
        return Err(AcpiError::BadRsdpChecksum);
    }

    let revision = rsdp.revision;
    let use_xsdt = revision >= 2 && rsdp.xsdt_address != 0;
    let root_addr = if use_xsdt { rsdp.xsdt_address } else { rsdp.rsdt_address as u64 };
    let root = unsafe { read_header(root_addr) };
    if !checksum_ok(root_addr, root.length as usize) {
        return Err(AcpiError::BadTableChecksum);
    }

    klog!("[ACPI] RSDP revision {} at {:#x}, OEM '{}', using {}",
        revision, rsdp_addr, ascii(&rsdp.oem_id), if use_xsdt { "XSDT" } else { "RSDT" });

    let entry_size = if use_xsdt { 8 } else { 4 };
    let entry_count = (root.length as usize - core::mem::size_of::<SdtHeader>()) / entry_size;
    let entries = root_addr + core::mem::size_of::<SdtHeader>() as u64;

    let mut madt_addr = 0u64;
    for i in 0..entry_count {
        let table_addr = unsafe {
            if use_xsdt {
                core::ptr::read_unaligned((entries + (i * 8) as u64) as *const u64)
            } else {
                core::ptr::read_unaligned((entries + (i * 4) as u64) as *const u32) as u64
            }
        };
        let header = unsafe { read_header(table_addr) };
        if &header.signature == b"APIC" {
            madt_addr = table_addr;
        }
    }

    if madt_addr == 0 {
        return Err(AcpiError::NoMadt);
    }
    parse_madt(madt_addr)
}

fn parse_madt(addr: u64) -> Result<(), AcpiError> {
    let header = unsafe { read_header(addr) };
    let length = header.length as u64;
    if !checksum_ok(addr, length as usize) {
        return Err(AcpiError::BadTableChecksum);
    }

    let info = unsafe { &mut *MADT.0.get() };
    let body = addr + core::mem::size_of::<SdtHeader>() as u64;
    unsafe {
        info.lapic_address = core::ptr::read_unaligned(body as *const u32) as u64;
        info.pcat_compat = core::ptr::read_unaligned((body + 4) as *const u32) & 1 != 0;
    }

    let end = addr + length;
    let mut entry = body + 8;
    while entry + 2 <= end {
        let (kind, len) = unsafe { (*(entry as *const u8), *((entry + 1) as *const u8) as u64) };
        if len < 2 || entry + len > end {
            break;
        }

        unsafe {
            match kind {
                MADT_LOCAL_APIC => {
                    let apic_id = *((entry + 3) as *const u8);
                    let flags = core::ptr::read_unaligned((entry + 4) as *const u32);
                    if flags & 1 != 0 && info.cpu_count < MAX_CPUS {
                        info.cpu_apic_ids[info.cpu_count] = apic_id;
                        info.cpu_count += 1;
                    }
                }
                MADT_IO_APIC => {
                    if info.ioapic_count < MAX_IOAPICS {
                        info.ioapics[info.ioapic_count] = IoApic {
                            id: *((entry + 2) as *const u8),
                            address: core::ptr::read_unaligned((entry + 4) as *const u32) as u64,
                            gsi_base: core::ptr::read_unaligned((entry + 8) as *const u32),
                        };
                        info.ioapic_count += 1;
                    }
                }
                MADT_INTERRUPT_OVERRIDE => {
                    if info.override_count < MAX_OVERRIDES {
                        info.overrides[info.override_count] = InterruptOverride {
                            isa_irq: *((entry + 3) as *const u8),
                            gsi: core::ptr::read_unaligned((entry + 4) as *const u32),
                            flags: core::ptr::read_unaligned((entry + 8) as *const u16),
                        };
                        info.override_count += 1;
                    }
                }
                MADT_LOCAL_APIC_ADDRESS_OVERRIDE => {
                    info.lapic_address = core::ptr::read_unaligned((entry + 4) as *const u64);
                }
                _ => {}
            }
        }
        entry += len;
    }

    unsafe { *MADT_VALID.0.get() = true };
    log_madt(info);
    Ok(())
}

fn log_madt(info: &MadtInfo) {
    klog!("[ACPI] MADT parsed");
    klog!("  Local APIC      : {:#x}", info.lapic_address);
    klog!("  CPUs            : {}", info.cpu_count);
    for i in 0..info.ioapic_count {
        let io = &info.ioapics[i];
        klog!("  I/O APIC        : id {} at {:#x}, GSI base {}", io.id, io.address, io.gsi_base);
    }
    for i in 0..info.override_count {
        let o = &info.overrides[i];
        klog!("  IRQ override    : ISA IRQ {} -> GSI {} ({}, {})",
            o.isa_irq,
            o.gsi,
            if o.flags & POLARITY_MASK == POLARITY_ACTIVE_LOW { "active low" } else { "active high" },
            if o.flags & TRIGGER_MASK == TRIGGER_LEVEL { "level" } else { "edge" },
        );
    }
}

pub fn isa_irq_to_gsi(isa_irq: u8) -> (u32, u16) {
    if let Some(info) = madt() {
        for i in 0..info.override_count {
            if info.overrides[i].isa_irq == isa_irq {
                return (info.overrides[i].gsi, info.overrides[i].flags);
            }
        }
    }
    (isa_irq as u32, 0)
}

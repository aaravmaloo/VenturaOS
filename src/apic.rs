use core::sync::atomic::{AtomicUsize, AtomicU32, Ordering};
use crate::acpi;
use crate::klog;
use crate::platform;

// Used only when ACPI provides no MADT
pub const LAPIC_DEFAULT_BASE: usize = 0xFEE0_0000;
pub const IOAPIC_DEFAULT_BASE: usize = 0xFEC0_0000;

static LAPIC_BASE: AtomicUsize = AtomicUsize::new(LAPIC_DEFAULT_BASE);
static IOAPIC_BASE: AtomicUsize = AtomicUsize::new(IOAPIC_DEFAULT_BASE);
static IOAPIC_GSI_BASE: AtomicU32 = AtomicU32::new(0);

pub const LAPIC_ID: u32 = 0x0020;
pub const LAPIC_VER: u32 = 0x0030;
pub const LAPIC_TPR: u32 = 0x0080;
pub const LAPIC_EOI: u32 = 0x00B0;
pub const LAPIC_LDR: u32 = 0x00D0;
pub const LAPIC_DFR: u32 = 0x00E0;
pub const LAPIC_SVR: u32 = 0x00F0;
pub const LAPIC_ESR: u32 = 0x0280;
pub const LAPIC_LVT_TIMER: u32 = 0x0320;
pub const LAPIC_LVT_LINT0: u32 = 0x0350;
pub const LAPIC_LVT_LINT1: u32 = 0x0360;
pub const LAPIC_LVT_ERROR: u32 = 0x0370;
pub const LAPIC_TIMER_ICR: u32 = 0x0380;
pub const LAPIC_TIMER_CCR: u32 = 0x0390;
pub const LAPIC_TIMER_DCR: u32 = 0x03E0;

pub const IOAPIC_REGSEL: usize = 0x00;
pub const IOAPIC_IOWIN: usize = 0x10;

const IOAPIC_ACTIVE_LOW: u32 = 1 << 13;
const IOAPIC_LEVEL_TRIGGERED: u32 = 1 << 15;
const IOAPIC_MASKED: u32 = 1 << 16;

pub fn lapic_base() -> usize {
    LAPIC_BASE.load(Ordering::Relaxed)
}

pub fn ioapic_base() -> usize {
    IOAPIC_BASE.load(Ordering::Relaxed)
}

// Must run before vmm::init maps the APIC MMIO
pub fn configure_from_acpi() {
    let madt = match acpi::madt() {
        Some(m) => m,
        None => {
            klog!("[APIC WARN] No MADT, using default LAPIC {:#x} / I/O APIC {:#x}", LAPIC_DEFAULT_BASE, IOAPIC_DEFAULT_BASE);
            return;
        }
    };

    if madt.lapic_address != 0 {
        LAPIC_BASE.store(madt.lapic_address as usize, Ordering::Relaxed);
    }

    // Ventura drives the I/O APIC that owns GSI 0 (the ISA IRQs)
    for i in 0..madt.ioapic_count {
        if madt.ioapics[i].gsi_base == 0 {
            IOAPIC_BASE.store(madt.ioapics[i].address as usize, Ordering::Relaxed);
            IOAPIC_GSI_BASE.store(0, Ordering::Relaxed);
        }
    }
}

#[inline(always)]
pub unsafe fn lapic_read(reg: u32) -> u32 {
    let ptr = (lapic_base() + reg as usize) as *const u32;
    core::ptr::read_volatile(ptr)
}

#[inline(always)]
pub unsafe fn lapic_write(reg: u32, value: u32) {
    let ptr = (lapic_base() + reg as usize) as *mut u32;
    core::ptr::write_volatile(ptr, value);
}

#[inline(always)]
pub unsafe fn ioapic_read(reg: u8) -> u32 {
    let regsel = (ioapic_base() + IOAPIC_REGSEL) as *mut u32;
    let win = (ioapic_base() + IOAPIC_IOWIN) as *mut u32;
    core::ptr::write_volatile(regsel, reg as u32);
    core::ptr::read_volatile(win)
}

#[inline(always)]
pub unsafe fn ioapic_write(reg: u8, value: u32) {
    let regsel = (ioapic_base() + IOAPIC_REGSEL) as *mut u32;
    let win = (ioapic_base() + IOAPIC_IOWIN) as *mut u32;
    core::ptr::write_volatile(regsel, reg as u32);
    core::ptr::write_volatile(win, value);
}

pub unsafe fn disable_pic() {
    platform::outb(0x21, 0xFF);
    platform::outb(0xA1, 0xFF);
}

pub unsafe fn init_lapic() {
    let mut apic_base = platform::rdmsr(platform::IA32_APIC_BASE_MSR);
    apic_base |= 1 << 11;
    platform::wrmsr(platform::IA32_APIC_BASE_MSR, apic_base);

    lapic_write(LAPIC_DFR, 0xFFFF_FFFF);

    let id = (lapic_read(LAPIC_ID) >> 24) & 0xFF;
    lapic_write(LAPIC_LDR, id << 24);

    lapic_write(LAPIC_TPR, 0);

    lapic_write(LAPIC_LVT_TIMER, 0x0001_0000);
    lapic_write(LAPIC_LVT_LINT0, 0x0001_0000);
    lapic_write(LAPIC_LVT_LINT1, 0x0001_0000);
    lapic_write(LAPIC_LVT_ERROR, 0x0001_0000);

    lapic_write(LAPIC_SVR, 0x100 | 0xFF);

    lapic_write(LAPIC_EOI, 0);
}

pub unsafe fn eoi() {
    lapic_write(LAPIC_EOI, 0);
}

pub unsafe fn init_ioapic() {
    let ver = ioapic_read(0x01);
    let max_entries = ((ver >> 16) & 0xFF) + 1;
    let gsi_base = IOAPIC_GSI_BASE.load(Ordering::Relaxed);

    // 1. Mask every redirection entry, GSI n -> vector 0x20 + n
    for i in 0..max_entries {
        write_redirection(i, 0x20 + i as u8, 0, IOAPIC_MASKED);
    }

    // 2. Re-route ISA IRQs that ACPI moved to another GSI or polarity/trigger mode
    if let Some(madt) = acpi::madt() {
        for i in 0..madt.override_count {
            let o = madt.overrides[i];
            if o.gsi >= gsi_base && o.gsi - gsi_base < max_entries {
                write_redirection(o.gsi - gsi_base, 0x20 + o.isa_irq, 0, IOAPIC_MASKED | inti_flags(o.flags));
            }
        }
    }
}

fn inti_flags(flags: u16) -> u32 {
    let mut bits = 0;
    if flags & acpi::POLARITY_MASK == acpi::POLARITY_ACTIVE_LOW {
        bits |= IOAPIC_ACTIVE_LOW;
    }
    if flags & acpi::TRIGGER_MASK == acpi::TRIGGER_LEVEL {
        bits |= IOAPIC_LEVEL_TRIGGERED;
    }
    bits
}

unsafe fn write_redirection(pin: u32, vector: u8, dest_lapic_id: u8, bits: u32) {
    let reg_low = (0x10 + 2 * pin) as u8;
    let reg_high = (0x11 + 2 * pin) as u8;
    ioapic_write(reg_low, vector as u32 | bits);
    ioapic_write(reg_high, (dest_lapic_id as u32) << 24);
}

fn isa_irq_pin(irq: u8) -> u32 {
    let (gsi, _) = acpi::isa_irq_to_gsi(irq);
    gsi - IOAPIC_GSI_BASE.load(Ordering::Relaxed)
}

pub unsafe fn route_irq(irq: u8, vector: u8, dest_lapic_id: u8, masked: bool) {
    let (_, flags) = acpi::isa_irq_to_gsi(irq);
    let mut bits = inti_flags(flags);
    if masked {
        bits |= IOAPIC_MASKED;
    }
    write_redirection(isa_irq_pin(irq), vector, dest_lapic_id, bits);
}

pub unsafe fn unmask_irq(irq: u8) {
    let reg_low = (0x10 + 2 * isa_irq_pin(irq)) as u8;
    let low = ioapic_read(reg_low) & !IOAPIC_MASKED;
    ioapic_write(reg_low, low);
}

pub unsafe fn mask_irq(irq: u8) {
    let reg_low = (0x10 + 2 * isa_irq_pin(irq)) as u8;
    let low = ioapic_read(reg_low) | IOAPIC_MASKED;
    ioapic_write(reg_low, low);
}

pub unsafe fn start_lapic_timer(vector: u8, initial_count: u32, divide_cfg: u32) {
    lapic_write(LAPIC_TIMER_DCR, divide_cfg);
    lapic_write(LAPIC_LVT_TIMER, 0x0002_0000 | (vector as u32));
    lapic_write(LAPIC_TIMER_ICR, initial_count);
}

use crate::platform;

pub const LAPIC_DEFAULT_BASE: usize = 0xFEE0_0000;
pub const IOAPIC_DEFAULT_BASE: usize = 0xFEC0_0000;

pub const LAPIC_ID: u32        = 0x0020;
pub const LAPIC_VER: u32       = 0x0030;
pub const LAPIC_TPR: u32       = 0x0080;
pub const LAPIC_EOI: u32       = 0x00B0;
pub const LAPIC_LDR: u32       = 0x00D0;
pub const LAPIC_DFR: u32       = 0x00E0;
pub const LAPIC_SVR: u32       = 0x00F0;
pub const LAPIC_ESR: u32       = 0x0280;
pub const LAPIC_LVT_TIMER: u32 = 0x0320;
pub const LAPIC_LVT_LINT0: u32 = 0x0350;
pub const LAPIC_LVT_LINT1: u32 = 0x0360;
pub const LAPIC_LVT_ERROR: u32 = 0x0370;
pub const LAPIC_TIMER_ICR: u32 = 0x0380;
pub const LAPIC_TIMER_CCR: u32 = 0x0390;
pub const LAPIC_TIMER_DCR: u32 = 0x03E0;

pub const IOAPIC_REGSEL: usize = 0x00;
pub const IOAPIC_IOWIN: usize  = 0x10;

#[inline(always)]
pub unsafe fn lapic_read(reg: u32) -> u32 {
    let ptr = (LAPIC_DEFAULT_BASE + reg as usize) as *const u32;
    core::ptr::read_volatile(ptr)
}

#[inline(always)]
pub unsafe fn lapic_write(reg: u32, value: u32) {
    let ptr = (LAPIC_DEFAULT_BASE + reg as usize) as *mut u32;
    core::ptr::write_volatile(ptr, value);
}

#[inline(always)]
pub unsafe fn ioapic_read(reg: u8) -> u32 {
    let regsel = (IOAPIC_DEFAULT_BASE + IOAPIC_REGSEL) as *mut u32;
    let win = (IOAPIC_DEFAULT_BASE + IOAPIC_IOWIN) as *mut u32;
    core::ptr::write_volatile(regsel, reg as u32);
    core::ptr::read_volatile(win)
}

#[inline(always)]
pub unsafe fn ioapic_write(reg: u8, value: u32) {
    let regsel = (IOAPIC_DEFAULT_BASE + IOAPIC_REGSEL) as *mut u32;
    let win = (IOAPIC_DEFAULT_BASE + IOAPIC_IOWIN) as *mut u32;
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

    for i in 0..max_entries as u8 {
        route_irq(i, 0x20 + i, 0, true);
    }
}

pub unsafe fn route_irq(irq: u8, vector: u8, dest_lapic_id: u8, masked: bool) {
    let reg_low = 0x10 + 2 * irq;
    let reg_high = 0x11 + 2 * irq;

    let mut low: u32 = vector as u32;
    if masked {
        low |= 1 << 16;
    }

    let high: u32 = (dest_lapic_id as u32) << 24;

    ioapic_write(reg_low, low);
    ioapic_write(reg_high, high);
}

pub unsafe fn unmask_irq(irq: u8) {
    let reg_low = 0x10 + 2 * irq;
    let low = ioapic_read(reg_low) & !(1 << 16);
    ioapic_write(reg_low, low);
}

pub unsafe fn mask_irq(irq: u8) {
    let reg_low = 0x10 + 2 * irq;
    let low = ioapic_read(reg_low) | (1 << 16);
    ioapic_write(reg_low, low);
}

pub unsafe fn start_lapic_timer(vector: u8, initial_count: u32, divide_cfg: u32) {
    lapic_write(LAPIC_TIMER_DCR, divide_cfg);
    lapic_write(LAPIC_LVT_TIMER, 0x0002_0000 | (vector as u32));
    lapic_write(LAPIC_TIMER_ICR, initial_count);
}

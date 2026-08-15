use core::sync::atomic::{AtomicU64, Ordering};
use crate::idt;
use crate::klog;

pub const TIMER_IRQ: u8 = 0;
pub const TIMER_VECTOR: u8 = 32;

// Divide by 16 (0x03 in APIC Timer DCR)
pub const TIMER_DIVIDE_16: u32 = 0x03;
// Initial count for periodic interval in virtualization/QEMU (~100 Hz)
pub const TIMER_INITIAL_COUNT: u32 = 0x0010_0000;

static TICKS: AtomicU64 = AtomicU64::new(0);

pub fn current_ticks() -> u64 {
    TICKS.load(Ordering::Relaxed)
}

fn timer_irq_handler(_irq: u8) {
    let ticks = TICKS.fetch_add(1, Ordering::Relaxed) + 1;

    // Controlled diagnostic interval: print every 100 ticks
    if ticks % 100 == 0 {
        klog!("[TIMER] tick: {}", ticks);
    }
}

pub fn init() {
    let _ = idt::register_irq(TIMER_IRQ, timer_irq_handler);

    unsafe {
        crate::apic::start_lapic_timer(TIMER_VECTOR, TIMER_INITIAL_COUNT, TIMER_DIVIDE_16);
    }

    klog!("[TIMER] initialized (Local APIC periodic mode)");
}

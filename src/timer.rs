use core::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use crate::apic;
use crate::idt;
use crate::klog;
use crate::platform;

pub const TIMER_IRQ: u8 = 0;
pub const TIMER_VECTOR: u8 = 32;
pub const TIMER_HZ: u64 = 100;

// Divide by 16 (0x03 in APIC Timer DCR)
pub const TIMER_DIVIDE_16: u32 = 0x03;
// Fallback when PIT calibration fails (~100 Hz under QEMU)
pub const TIMER_FALLBACK_COUNT: u32 = 0x0010_0000;

const PIT_FREQUENCY_HZ: u64 = 1_193_182;
const PIT_CALIBRATION_HZ: u64 = 20; // 50 ms window
const PIT_CHANNEL2_DATA: u16 = 0x42;
const PIT_COMMAND: u16 = 0x43;
const PIT_GATE_PORT: u16 = 0x61;
const PIT_WAIT_LIMIT: u64 = 50_000_000;

static TICKS: AtomicU64 = AtomicU64::new(0);
static INITIAL_COUNT: AtomicU32 = AtomicU32::new(TIMER_FALLBACK_COUNT);

pub fn current_ticks() -> u64 {
    TICKS.load(Ordering::Relaxed)
}

pub fn uptime_ms() -> u64 {
    current_ticks() * 1000 / TIMER_HZ
}

fn timer_irq_handler(_irq: u8) {
    let ticks = TICKS.fetch_add(1, Ordering::Relaxed) + 1;

    // Controlled diagnostic interval: print every 100 ticks
    if ticks % 100 == 0 {
        klog!("[TIMER] tick: {}", ticks);
    }
}

// Must run with interrupts disabled
unsafe fn calibrate_lapic_timer() -> Option<u64> {
    // 1. Gate channel 2 on, speaker off
    let gate = (platform::inb(PIT_GATE_PORT) & !0x02) | 0x01;
    platform::outb(PIT_GATE_PORT, gate);

    // 2. Channel 2, lobyte/hibyte, mode 1 (hardware one-shot), binary
    let reload = (PIT_FREQUENCY_HZ / PIT_CALIBRATION_HZ) as u16;
    platform::outb(PIT_COMMAND, 0xB2);
    platform::outb(PIT_CHANNEL2_DATA, (reload & 0xFF) as u8);
    platform::outb(PIT_CHANNEL2_DATA, (reload >> 8) as u8);

    // 3. Restart the one-shot with a gate low -> high edge
    let gate = platform::inb(PIT_GATE_PORT) & !0x01;
    platform::outb(PIT_GATE_PORT, gate);
    platform::outb(PIT_GATE_PORT, gate | 0x01);

    // 4. Free-run the LAPIC timer (masked, one-shot) until the PIT output goes high
    apic::lapic_write(apic::LAPIC_TIMER_DCR, TIMER_DIVIDE_16);
    apic::lapic_write(apic::LAPIC_LVT_TIMER, 0x0001_0000 | TIMER_VECTOR as u32);
    apic::lapic_write(apic::LAPIC_TIMER_ICR, u32::MAX);

    let mut waited = 0u64;
    while platform::inb(PIT_GATE_PORT) & 0x20 == 0 {
        waited += 1;
        if waited > PIT_WAIT_LIMIT {
            apic::lapic_write(apic::LAPIC_TIMER_ICR, 0);
            return None;
        }
    }

    let elapsed = u32::MAX - apic::lapic_read(apic::LAPIC_TIMER_CCR);
    apic::lapic_write(apic::LAPIC_TIMER_ICR, 0);

    if elapsed == 0 {
        return None;
    }
    Some(elapsed as u64 * PIT_CALIBRATION_HZ)
}

pub fn init() {
    let _ = idt::register_irq(TIMER_IRQ, timer_irq_handler);

    let counts_per_sec = unsafe { calibrate_lapic_timer() };
    match counts_per_sec {
        Some(rate) => {
            let initial = (rate / TIMER_HZ).clamp(1, u32::MAX as u64) as u32;
            INITIAL_COUNT.store(initial, Ordering::Relaxed);
            klog!("[TIMER] calibrated against PIT: {} LAPIC counts/s (divide 16)", rate);
        }
        None => {
            klog!("[TIMER WARN] PIT calibration failed, using fallback count {:#x}", TIMER_FALLBACK_COUNT);
        }
    }

    unsafe {
        apic::start_lapic_timer(TIMER_VECTOR, INITIAL_COUNT.load(Ordering::Relaxed), TIMER_DIVIDE_16);
    }

    klog!("[TIMER] initialized (Local APIC periodic mode, {} Hz, initial count {})",
        TIMER_HZ, INITIAL_COUNT.load(Ordering::Relaxed));
}

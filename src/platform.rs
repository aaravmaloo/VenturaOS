use core::arch::asm;

pub const COM1_PORT: u16 = 0x3F8;
pub const COM2_PORT: u16 = 0x2F8;

pub const IA32_APIC_BASE_MSR: u32 = 0x1B;

#[inline(always)]
pub fn hlt() {
    unsafe {
        asm!("hlt", options(nomem, nostack, preserves_flags));
    }
}

#[inline(never)]
pub fn halt() -> ! {
    loop {
        hlt();
    }
}

#[inline(always)]
pub fn sti() {
    unsafe {
        asm!("sti", options(nomem, nostack, preserves_flags));
    }
}

#[inline(always)]
pub fn cli() {
    unsafe {
        asm!("cli", options(nomem, nostack, preserves_flags));
    }
}

#[inline(always)]
pub fn are_interrupts_enabled() -> bool {
    let rflags: u64;
    unsafe {
        asm!("pushfq; pop {}", out(reg) rflags, options(nomem, preserves_flags));
    }
    (rflags & (1 << 9)) != 0
}

#[inline(always)]
pub fn without_interrupts<F, R>(f: F) -> R
where
    F: FnOnce() -> R,
{
    let enabled = are_interrupts_enabled();
    if enabled {
        cli();
    }
    let res = f();
    if enabled {
        sti();
    }
    res
}

#[inline(always)]
pub unsafe fn inb(port: u16) -> u8 {
    let value: u8;
    asm!("in al, dx", in("dx") port, out("al") value, options(nomem, nostack, preserves_flags));
    value
}

#[inline(always)]
pub unsafe fn outb(port: u16, value: u8) {
    asm!("out dx, al", in("dx") port, in("al") value, options(nomem, nostack, preserves_flags));
}

#[inline(always)]
pub unsafe fn inw(port: u16) -> u16 {
    let value: u16;
    asm!("in ax, dx", in("dx") port, out("ax") value, options(nomem, nostack, preserves_flags));
    value
}

#[inline(always)]
pub unsafe fn outw(port: u16, value: u16) {
    asm!("out dx, ax", in("dx") port, in("ax") value, options(nomem, nostack, preserves_flags));
}

#[inline(always)]
pub unsafe fn inl(port: u16) -> u32 {
    let value: u32;
    asm!("in eax, dx", in("dx") port, out("eax") value, options(nomem, nostack, preserves_flags));
    value
}

#[inline(always)]
pub unsafe fn outl(port: u16, value: u32) {
    asm!("out dx, eax", in("dx") port, in("eax") value, options(nomem, nostack, preserves_flags));
}

#[inline(always)]
pub unsafe fn rdmsr(msr: u32) -> u64 {
    let low: u32;
    let high: u32;
    asm!("rdmsr", in("ecx") msr, out("eax") low, out("edx") high, options(nomem, nostack, preserves_flags));
    ((high as u64) << 32) | (low as u64)
}

#[inline(always)]
pub unsafe fn wrmsr(msr: u32, value: u64) {
    let low = value as u32;
    let high = (value >> 32) as u32;
    asm!("wrmsr", in("ecx") msr, in("eax") low, in("edx") high, options(nomem, nostack, preserves_flags));
}

#[inline(always)]
pub unsafe fn read_cr3() -> u64 {
    let cr3: u64;
    asm!("mov {}, cr3", out(reg) cr3, options(nomem, nostack, preserves_flags));
    cr3
}

#[inline(never)]
pub unsafe fn write_cr3(val: u64) {
    // NOTE: Do NOT use `nomem` or `preserves_flags` here.
    // Writing CR3 changes the TLB/page-table context for ALL subsequent memory
    // accesses.  Lying to LLVM with `nomem` would allow it to reorder any load
    // or store across this instruction, which is undefined behaviour.
    // `inline(never)` gives an extra serialisation barrier at the call boundary.
    asm!(
        "mov cr3, {}",
        in(reg) val,
        options(nostack),   // no nomem, no preserves_flags
    );
}

/// Enable the NXE (No-Execute Enable) bit in the EFER MSR.
///
/// This MUST be called before any page table entry with bit 63 (NO_EXECUTE)
/// is live.  Without NXE=1 the CPU treats bit 63 as a reserved bit and raises
/// a #PF(RSVD) on the very first page-table walk of such an entry, causing an
/// immediate triple fault after CR3 is switched.
#[inline(always)]
pub unsafe fn enable_nxe() {
    const IA32_EFER: u32 = 0xC000_0080;
    const NXE_BIT: u64   = 1 << 11;
    let current = rdmsr(IA32_EFER);
    if current & NXE_BIT == 0 {
        wrmsr(IA32_EFER, current | NXE_BIT);
    }
}

#[inline(always)]
pub unsafe fn read_cr2() -> u64 {
    let cr2: u64;
    asm!("mov {}, cr2", out(reg) cr2, options(nomem, nostack, preserves_flags));
    cr2
}

#[inline(always)]
pub unsafe fn invlpg(vaddr: u64) {
    asm!("invlpg [{}]", in(reg) vaddr, options(nostack, preserves_flags));
}

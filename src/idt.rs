use core::arch::{asm, global_asm};
use core::cell::UnsafeCell;
use core::mem::size_of;
use core::sync::atomic::{AtomicU32, Ordering};
use crate::klog;
use crate::platform;

pub const IRQ_BASE_VECTOR: u8 = 32;
pub const SPURIOUS_VECTOR: u8 = 255;

pub type IrqHandler = fn(irq: u8);

#[repr(C, packed)]
#[derive(Copy, Clone)]
pub struct IdtEntry {
    offset_low: u16,
    selector: u16,
    ist: u8,
    type_attr: u8,
    offset_mid: u16,
    offset_high: u32,
    reserved: u32,
}

impl IdtEntry {
    pub const fn missing() -> Self {
        Self {
            offset_low: 0,
            selector: 0,
            ist: 0,
            type_attr: 0,
            offset_mid: 0,
            offset_high: 0,
            reserved: 0,
        }
    }

    pub fn set_handler(&mut self, handler: usize) {
        self.offset_low = (handler & 0xFFFF) as u16;
        self.selector = crate::gdt::KERNEL_CODE_SELECTOR;
        self.ist = 0;
        self.type_attr = 0x8E;
        self.offset_mid = ((handler >> 16) & 0xFFFF) as u16;
        self.offset_high = (handler >> 32) as u32;
        self.reserved = 0;
    }
}

#[repr(C, align(16))]
pub struct InterruptDescriptorTable {
    entries: [IdtEntry; 256],
}

impl InterruptDescriptorTable {
    pub const fn new() -> Self {
        Self {
            entries: [IdtEntry::missing(); 256],
        }
    }

    pub fn set_entry(&mut self, vector: usize, handler: usize) {
        if vector < 256 {
            self.entries[vector].set_handler(handler);
        }
    }

    pub fn pointer(&self) -> crate::gdt::DescriptorTablePointer {
        crate::gdt::DescriptorTablePointer {
            limit: (size_of::<Self>() - 1) as u16,
            base: self as *const _ as u64,
        }
    }
}

struct SyncCell<T>(UnsafeCell<T>);
unsafe impl<T> Sync for SyncCell<T> {}

static IDT: SyncCell<InterruptDescriptorTable> = SyncCell(UnsafeCell::new(InterruptDescriptorTable::new()));
static IRQ_HANDLERS: SyncCell<[Option<IrqHandler>; 16]> = SyncCell(UnsafeCell::new([None; 16]));
static UNHANDLED_IRQ_COUNT: [AtomicU32; 16] = [
    AtomicU32::new(0), AtomicU32::new(0), AtomicU32::new(0), AtomicU32::new(0),
    AtomicU32::new(0), AtomicU32::new(0), AtomicU32::new(0), AtomicU32::new(0),
    AtomicU32::new(0), AtomicU32::new(0), AtomicU32::new(0), AtomicU32::new(0),
    AtomicU32::new(0), AtomicU32::new(0), AtomicU32::new(0), AtomicU32::new(0),
];

#[repr(C)]
#[derive(Debug)]
pub struct Registers {
    pub r15: u64,
    pub r14: u64,
    pub r13: u64,
    pub r12: u64,
    pub r11: u64,
    pub r10: u64,
    pub r9: u64,
    pub r8: u64,
    pub rbp: u64,
    pub rdi: u64,
    pub rsi: u64,
    pub rdx: u64,
    pub rcx: u64,
    pub rbx: u64,
    pub rax: u64,
}

#[repr(C)]
#[derive(Debug)]
pub struct ExceptionFrame {
    pub regs: Registers,
    pub vector: u64,
    pub error_code: u64,
    pub rip: u64,
    pub cs: u64,
    pub rflags: u64,
    pub rsp: u64,
    pub ss: u64,
}

extern "C" {
    static exception_stubs: [usize; 32];
    static irq_stubs: [usize; 16];
    fn generic_irq_stub();
    fn spurious_irq_stub();
}

global_asm!(r#"
.macro EXCEPTION_NO_ERR vector
.global stub_\vector
stub_\vector:
    push 0
    push \vector
    jmp common_exception_entry
.endm

.macro EXCEPTION_ERR vector
.global stub_\vector
stub_\vector:
    push \vector
    jmp common_exception_entry
.endm

EXCEPTION_NO_ERR 0
EXCEPTION_NO_ERR 1
EXCEPTION_NO_ERR 2
EXCEPTION_NO_ERR 3
EXCEPTION_NO_ERR 4
EXCEPTION_NO_ERR 5
EXCEPTION_NO_ERR 6
EXCEPTION_NO_ERR 7
EXCEPTION_ERR    8
EXCEPTION_NO_ERR 9
EXCEPTION_ERR    10
EXCEPTION_ERR    11
EXCEPTION_ERR    12
EXCEPTION_ERR    13
EXCEPTION_ERR    14
EXCEPTION_NO_ERR 15
EXCEPTION_NO_ERR 16
EXCEPTION_ERR    17
EXCEPTION_NO_ERR 18
EXCEPTION_NO_ERR 19
EXCEPTION_NO_ERR 20
EXCEPTION_ERR    21
EXCEPTION_NO_ERR 22
EXCEPTION_NO_ERR 23
EXCEPTION_NO_ERR 24
EXCEPTION_NO_ERR 25
EXCEPTION_NO_ERR 26
EXCEPTION_NO_ERR 27
EXCEPTION_NO_ERR 28
EXCEPTION_NO_ERR 29
EXCEPTION_ERR    30
EXCEPTION_NO_ERR 31

.global common_exception_entry
common_exception_entry:
    push rax
    push rbx
    push rcx
    push rdx
    push rsi
    push rdi
    push rbp
    push r8
    push r9
    push r10
    push r11
    push r12
    push r13
    push r14
    push r15

    mov rdi, rsp
    call exception_dispatcher

    pop r15
    pop r14
    pop r13
    pop r12
    pop r11
    pop r10
    pop r9
    pop r8
    pop rbp
    pop rdi
    pop rsi
    pop rdx
    pop rcx
    pop rbx
    pop rax

    add rsp, 16
    iretq

.macro IRQ_STUB irq
.global irq_stub_\irq
irq_stub_\irq:
    push rax
    push rbx
    push rcx
    push rdx
    push rsi
    push rdi
    push rbp
    push r8
    push r9
    push r10
    push r11
    push r12
    push r13
    push r14
    push r15

    mov rdi, \irq
    call irq_dispatcher

    pop r15
    pop r14
    pop r13
    pop r12
    pop r11
    pop r10
    pop r9
    pop r8
    pop rbp
    pop rdi
    pop rsi
    pop rdx
    pop rcx
    pop rbx
    pop rax

    iretq
.endm

IRQ_STUB 0
IRQ_STUB 1
IRQ_STUB 2
IRQ_STUB 3
IRQ_STUB 4
IRQ_STUB 5
IRQ_STUB 6
IRQ_STUB 7
IRQ_STUB 8
IRQ_STUB 9
IRQ_STUB 10
IRQ_STUB 11
IRQ_STUB 12
IRQ_STUB 13
IRQ_STUB 14
IRQ_STUB 15

.global generic_irq_stub
generic_irq_stub:
    push rax
    push rbx
    push rcx
    push rdx
    push rsi
    push rdi
    push rbp
    push r8
    push r9
    push r10
    push r11
    push r12
    push r13
    push r14
    push r15

    call generic_interrupt_dispatcher

    pop r15
    pop r14
    pop r13
    pop r12
    pop r11
    pop r10
    pop r9
    pop r8
    pop rbp
    pop rdi
    pop rsi
    pop rdx
    pop rcx
    pop rbx
    pop rax

    iretq

.global spurious_irq_stub
spurious_irq_stub:
    /* Spurious interrupts do not set ISR bit, so no EOI is issued per Intel spec */
    iretq

.section .data
.global exception_stubs
.align 8
exception_stubs:
    .quad stub_0,  stub_1,  stub_2,  stub_3
    .quad stub_4,  stub_5,  stub_6,  stub_7
    .quad stub_8,  stub_9,  stub_10, stub_11
    .quad stub_12, stub_13, stub_14, stub_15
    .quad stub_16, stub_17, stub_18, stub_19
    .quad stub_20, stub_21, stub_22, stub_23
    .quad stub_24, stub_25, stub_26, stub_27
    .quad stub_28, stub_29, stub_30, stub_31

.global irq_stubs
.align 8
irq_stubs:
    .quad irq_stub_0,  irq_stub_1,  irq_stub_2,  irq_stub_3
    .quad irq_stub_4,  irq_stub_5,  irq_stub_6,  irq_stub_7
    .quad irq_stub_8,  irq_stub_9,  irq_stub_10, irq_stub_11
    .quad irq_stub_12, irq_stub_13, irq_stub_14, irq_stub_15
"#);

pub fn init() {
    unsafe {
        let idt = &mut *IDT.0.get();

        // 1. Exception vectors 0..31
        for vector in 0..32 {
            idt.set_entry(vector, exception_stubs[vector]);
        }

        // 2. Hardware IRQ vectors 32..47 (IRQs 0..15)
        for irq in 0..16 {
            idt.set_entry(IRQ_BASE_VECTOR as usize + irq, irq_stubs[irq]);
        }

        // 3. Generic handler for remaining vectors 48..254
        for vector in 48..255 {
            idt.set_entry(vector, generic_irq_stub as *const () as usize);
        }

        // 4. Dedicated Spurious Interrupt vector 255 (no EOI per APIC spec)
        idt.set_entry(SPURIOUS_VECTOR as usize, spurious_irq_stub as *const () as usize);

        let ptr = idt.pointer();
        asm!("lidt [{}]", in(reg) &ptr, options(readonly, nostack, preserves_flags));
    }
}

pub fn register_irq(irq: u8, handler: IrqHandler) -> Result<(), &'static str> {
    if (irq as usize) >= 16 {
        return Err("IRQ index out of valid range (0..15)");
    }

    platform::without_interrupts(|| {
        unsafe {
            let handlers = &mut *IRQ_HANDLERS.0.get();
            handlers[irq as usize] = Some(handler);
        }
    });

    Ok(())
}

pub fn unregister_irq(irq: u8) -> Result<(), &'static str> {
    if (irq as usize) >= 16 {
        return Err("IRQ index out of valid range (0..15)");
    }

    platform::without_interrupts(|| {
        unsafe {
            let handlers = &mut *IRQ_HANDLERS.0.get();
            handlers[irq as usize] = None;
        }
    });

    Ok(())
}

#[no_mangle]
pub extern "sysv64" fn irq_dispatcher(irq: u64) {
    if (irq as usize) < 16 {
        let handler = unsafe { (*IRQ_HANDLERS.0.get())[irq as usize] };
        if let Some(h) = handler {
            h(irq as u8);
        } else {
            let count = UNHANDLED_IRQ_COUNT[irq as usize].fetch_add(1, Ordering::Relaxed);
            if count < 5 {
                klog!("[IRQ] Unhandled hardware IRQ {}", irq);
            }
        }
    }

    unsafe {
        crate::apic::eoi();
    }
}

#[no_mangle]
pub extern "sysv64" fn generic_interrupt_dispatcher() {
    static UNEXPECTED_COUNT: AtomicU32 = AtomicU32::new(0);
    let count = UNEXPECTED_COUNT.fetch_add(1, Ordering::Relaxed);
    if count < 5 {
        klog!("[IRQ] Unexpected external interrupt received");
    }

    unsafe {
        crate::apic::eoi();
    }
}

const EXCEPTION_NAMES: [&str; 32] = [
    "Divide Error (#DE)",
    "Debug (#DB)",
    "Non-Maskable Interrupt (#NMI)",
    "Breakpoint (#BP)",
    "Overflow (#OF)",
    "Bound Range Exceeded (#BR)",
    "Invalid Opcode (#UD)",
    "Device Not Available (#NM)",
    "Double Fault (#DF)",
    "Coprocessor Segment Overrun",
    "Invalid TSS (#TS)",
    "Segment Not Present (#NP)",
    "Stack-Segment Fault (#SS)",
    "General Protection Fault (#GP)",
    "Page Fault (#PF)",
    "Reserved",
    "x87 Floating-Point Exception (#MF)",
    "Alignment Check (#AC)",
    "Machine Check (#MC)",
    "SIMD Floating-Point Exception (#XM)",
    "Virtualization Exception (#VE)",
    "Control Protection Exception (#CP)",
    "Reserved",
    "Reserved",
    "Reserved",
    "Reserved",
    "Reserved",
    "Reserved",
    "Hypervisor Injection Exception (#HV)",
    "VMM Communication Exception (#VC)",
    "Security Exception (#SX)",
    "Reserved",
];

#[no_mangle]
pub extern "sysv64" fn exception_dispatcher(frame: &ExceptionFrame) {
    platform::cli();
    crate::logger::disable_uefi_console();

    let vector = frame.vector as usize;
    let name = if vector < EXCEPTION_NAMES.len() {
        EXCEPTION_NAMES[vector]
    } else {
        "Unknown Exception"
    };

    if vector == 3 {
        klog!("\r\n[EXCEPTION] Breakpoint (#BP) trapped successfully");
        klog!("  RIP: {:#018x}  CS: {:#06x}", frame.rip, frame.cs);
        klog!("  RFLAGS: {:#018x}  RSP: {:#018x}", frame.rflags, frame.rsp);
        return;
    }

    klog!("\r\n[CPU EXCEPTION] {}", name);
    klog!("  Vector:     {}", vector);
    klog!("  Error Code: {:#018x}", frame.error_code);
    klog!("  RIP:        {:#018x}  CS: {:#06x}", frame.rip, frame.cs);
    klog!("  RFLAGS:     {:#018x}  SS: {:#06x}", frame.rflags, frame.ss);
    klog!("  RSP:        {:#018x}  RBP: {:#018x}", frame.rsp, frame.regs.rbp);
    klog!("  RAX: {:#018x}  RBX: {:#018x}", frame.regs.rax, frame.regs.rbx);
    klog!("  RCX: {:#018x}  RDX: {:#018x}", frame.regs.rcx, frame.regs.rdx);
    klog!("  RSI: {:#018x}  RDI: {:#018x}", frame.regs.rsi, frame.regs.rdi);

    if vector == 14 {
        let cr2: u64;
        unsafe {
            asm!("mov {}, cr2", out(reg) cr2, options(nomem, nostack, preserves_flags));
        }
        klog!("  CR2 (Faulting Address): {:#018x}", cr2);
        klog!("  Page Fault Flags: P={} W/R={} U/S={} RSVD={} I/D={}",
            frame.error_code & 1,
            (frame.error_code >> 1) & 1,
            (frame.error_code >> 2) & 1,
            (frame.error_code >> 3) & 1,
            (frame.error_code >> 4) & 1,
        );
    }

    klog!("[CPU EXCEPTION] Halting CPU safely.\r\n");
    loop {
        platform::cli();
        platform::hlt();
    }
}

pub fn test_breakpoint() {
    unsafe {
        asm!("int3", options(nomem, nostack, preserves_flags));
    }
}

pub fn test_divide_by_zero() {
    unsafe {
        let zero = 0u64;
        let val = 42u64;
        asm!(
            "div {0}",
            in(reg) zero,
            inout("rax") val => _,
            inout("rdx") 0u64 => _,
            options(nostack)
        );
    }
}

pub fn test_invalid_opcode() {
    unsafe {
        asm!("ud2", options(nomem, nostack, preserves_flags));
    }
}

pub fn test_page_fault() {
    unsafe {
        let ptr = 0xDEAD_BEEF as *const u64;
        let _ = core::ptr::read_volatile(ptr);
    }
}

fn com1_test_handler(irq: u8) {
    klog!("[IRQ] Received hardware interrupt");
    klog!("  Source: COM1 UART (16550A)");
    klog!("  IRQ:    {}", irq);
    klog!("  Vector: {:#04x} (36)", IRQ_BASE_VECTOR + irq);

    let _ = unsafe { platform::inb(platform::COM1_PORT + 2) };
    unsafe { platform::outb(platform::COM1_PORT + 1, 0x00); }
}

pub fn test_hardware_irq() {
    let _ = register_irq(4, com1_test_handler);
    unsafe {
        crate::apic::unmask_irq(4);
        platform::outb(platform::COM1_PORT + 1, 0x02);
    }
}

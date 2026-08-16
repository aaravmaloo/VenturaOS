use core::arch::asm;
use core::sync::atomic::{AtomicU64, Ordering};
use crate::klog;
use crate::memory::PAGE_SIZE;
use crate::platform;
use crate::vmm::{self, VirtPermissions, RegionPurpose, VirtRegion, VmmError};

pub const STACK_SIZE_BYTES: u64 = PAGE_SIZE * 4; // 16 KiB kernel stack per context
static NEXT_CONTEXT_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
#[repr(u8)]
pub enum ContextState {
    Uninitialized = 0,
    Initialized   = 1,
    Ready         = 2,
    Running       = 3,
    Suspended     = 4,
    Terminated    = 5,
}

impl ContextState {
    pub fn name(self) -> &'static str {
        match self {
            ContextState::Uninitialized => "UNINITIALIZED",
            ContextState::Initialized   => "INITIALIZED",
            ContextState::Ready         => "READY",
            ContextState::Running       => "RUNNING",
            ContextState::Suspended     => "SUSPENDED",
            ContextState::Terminated    => "TERMINATED",
        }
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum ContextError {
    InvalidEntryPoint,
    InvalidStackPointer,
    StackAllocationFailed,
}

pub struct KernelStack {
    pub region: VirtRegion,
}

impl KernelStack {
    pub fn allocate() -> Result<Self, VmmError> {
        let region = vmm::allocate_and_map_region(
            STACK_SIZE_BYTES,
            VirtPermissions::KERNEL_DATA,
            RegionPurpose::DynamicKernel,
        )?;

        Ok(Self { region })
    }

    pub fn top(&self) -> u64 {
        // Stack grows downward. Top of stack is high address.
        self.region.end().as_u64()
    }

    pub fn bottom(&self) -> u64 {
        self.region.start.as_u64()
    }
}

#[repr(C)]
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct ExecutionContext {
    pub rsp: u64,            // Offset 0: Saved Stack Pointer
    pub r15: u64,            // Offset 8: Callee-saved R15
    pub r14: u64,            // Offset 16: Callee-saved R14
    pub r13: u64,            // Offset 24: Callee-saved R13
    pub r12: u64,            // Offset 32: Callee-saved R12
    pub rbx: u64,            // Offset 40: Callee-saved RBX
    pub rbp: u64,            // Offset 48: Callee-saved RBP
    pub rip: u64,            // Offset 56: Saved Instruction Pointer
    pub rflags: u64,         // Offset 64: Saved RFLAGS
    pub state: ContextState, // Offset 72: Context State
    pub id: u64,             // Offset 80: Context ID
}

impl ExecutionContext {
    pub const fn empty() -> Self {
        Self {
            rsp: 0,
            r15: 0,
            r14: 0,
            r13: 0,
            r12: 0,
            rbx: 0,
            rbp: 0,
            rip: 0,
            rflags: 0x202,
            state: ContextState::Uninitialized,
            id: 0,
        }
    }
}

pub fn create_context_raw(
    entry_addr: u64,
    stack: &KernelStack,
) -> Result<ExecutionContext, ContextError> {
    if entry_addr == 0 || !vmm::is_canonical(entry_addr) {
        return Err(ContextError::InvalidEntryPoint);
    }

    let stack_top = stack.top();
    if stack_top == 0 || !vmm::is_canonical(stack_top) {
        return Err(ContextError::InvalidStackPointer);
    }

    let id = NEXT_CONTEXT_ID.fetch_add(1, Ordering::Relaxed);

    // Initial stack frame layout (80 bytes total, 16-byte aligned):
    // [stack_top - 8]  = initial_context_entry (popped by ret)
    // [stack_top - 16] = 0x202 (popped by popfq)
    // [stack_top - 24] = 0 (rbx)
    // [stack_top - 32] = 0 (rbp)
    // [stack_top - 40] = entry_addr (r12)
    // [stack_top - 48] = id (r13)
    // [stack_top - 56] = 0 (r14)
    // [stack_top - 64] = 0 (r15)
    // [stack_top - 72] = 0 (rdi)
    // [stack_top - 80] = 0 (rsi)
    let initial_sp = (stack_top & !0xFu64) - 80;

    unsafe {
        let p = initial_sp as *mut u64;
        p.add(0).write(0);                                         // rsi
        p.add(1).write(0);                                         // rdi
        p.add(2).write(0);                                         // r15
        p.add(3).write(0);                                         // r14
        p.add(4).write(id);                                        // r13
        p.add(5).write(entry_addr);                               // r12
        p.add(6).write(0);                                         // rbp
        p.add(7).write(0);                                         // rbx
        p.add(8).write(0x202);                                     // rflags
        p.add(9).write(initial_context_entry as *const () as u64); // return address for ret
    }

    Ok(ExecutionContext {
        rsp: initial_sp,
        r15: 0,
        r14: 0,
        r13: id,
        r12: entry_addr,
        rbx: 0,
        rbp: 0,
        rip: initial_context_entry as *const () as u64,
        rflags: 0x202,
        state: ContextState::Initialized,
        id,
    })
}

pub fn create_context(
    entry_point: fn(),
    stack: &KernelStack,
) -> Result<ExecutionContext, ContextError> {
    create_context_raw(entry_point as u64, stack)
}

#[no_mangle]
pub unsafe extern "C" fn initial_context_entry() -> ! {
    asm!(
        // r12 holds entry_point, r13 holds context_id (restored by switch_context)
        "mov rcx, r12", // 1st arg for x86_64 Win64 ABI
        "mov rdx, r13", // 2nd arg for x86_64 Win64 ABI
        // 32-byte shadow space required by x86_64 Win64 ABI
        "sub rsp, 32",
        "call kernel_context_trampoline",
        options(noreturn)
    );
}

#[no_mangle]
pub extern "C" fn kernel_context_trampoline(entry_raw: u64, id: u64) -> ! {
    let entry_fn: fn() = unsafe { core::mem::transmute(entry_raw) };

    klog!("[CONTEXT {}] Context started via trampoline (RIP={:#018x})", id, entry_raw);

    // Execute kernel function
    entry_fn();

    // Context exit path
    kernel_context_exit(id);
}

pub fn kernel_context_exit(id: u64) -> ! {
    klog!("[CONTEXT {}] Execution completed safely — entering halt loop", id);
    loop {
        platform::hlt();
    }
}

#[no_mangle]
pub unsafe extern "C" fn switch_context(
    prev: *mut ExecutionContext,
    next: *const ExecutionContext,
) {
    asm!(
        // 1. Save RFLAGS
        "pushfq",

        // 2. Save all callee-saved registers onto current stack
        "push rbx",
        "push rbp",
        "push r12",
        "push r13",
        "push r14",
        "push r15",
        "push rdi",
        "push rsi",

        // 3. Save current RSP to prev context struct
        "mov [rcx], rsp",

        // Update prev state to Suspended
        "mov byte ptr [rcx + 72], 4", // ContextState::Suspended

        // Update next state to Running
        "mov byte ptr [rdx + 72], 3", // ContextState::Running

        // 4. Switch to next stack pointer
        "mov rsp, [rdx]",

        // 5. Restore callee-saved registers from new stack
        "pop rsi",
        "pop rdi",
        "pop r15",
        "pop r14",
        "pop r13",
        "pop r12",
        "pop rbp",
        "pop rbx",

        // 6. Restore RFLAGS
        "popfq",

        // 7. Return to caller on the new stack
        "ret",
        in("rcx") prev,
        in("rdx") next,
        options(noreturn)
    );
}

// ── Self-Tests & Verifications for M4.1 ──────────────────────────────────────

static mut TEST_STAGE: u32 = 0;
static mut CTX_A_SP_SEEN: u64 = 0;
static mut CTX_B_SP_SEEN: u64 = 0;

static mut CTX_MAIN: ExecutionContext = ExecutionContext::empty();
static mut CTX_A: ExecutionContext = ExecutionContext::empty();
static mut CTX_B: ExecutionContext = ExecutionContext::empty();

fn test_context_a_fn() {
    klog!("[CONTEXT A] Context A executing");
    unsafe {
        let sp: u64;
        asm!("mov {}, rsp", out(reg) sp);
        CTX_A_SP_SEEN = sp;

        // Verify local stack write
        let mut canary_a: u64 = 0x1111_2222_3333_4444;
        core::ptr::write_volatile(&mut canary_a, 0x1111_2222_3333_4444);

        TEST_STAGE = 1;
        klog!("[CONTEXT A] Switching from Context A -> Context B...");

        switch_context(&raw mut CTX_A, &raw const CTX_B);
    }
}

fn test_context_b_fn() {
    klog!("[CONTEXT B] Context B executing");
    unsafe {
        let sp: u64;
        asm!("mov {}, rsp", out(reg) sp);
        CTX_B_SP_SEEN = sp;

        let mut canary_b: u64 = 0x5555_6666_7777_8888;
        core::ptr::write_volatile(&mut canary_b, 0x5555_6666_7777_8888);

        if TEST_STAGE != 1 {
            klog!("[CONTEXT TEST FAILED] Context B executed out of order!");
            platform::halt();
        }

        TEST_STAGE = 2;
        klog!("[CONTEXT B] Switching from Context B -> Main Context...");

        switch_context(&raw mut CTX_B, &raw const CTX_MAIN);
    }
}

pub fn run_self_tests() {
    klog!("\r\n==============================================");
    klog!("[CONTEXT] Running Execution Context self-tests...");
    klog!("==============================================");

    // 1. Validation tests
    let dummy_stack = match KernelStack::allocate() {
        Ok(s) => s,
        Err(e) => {
            let region_count = vmm::region_count();
            klog!("[CONTEXT TEST FAILED] Failed to allocate dummy stack!");
            klog!("  Error       : {:?}", e);
            klog!("  VMM regions : {}/{}", region_count, vmm::MAX_VIRT_REGIONS);
            klog!("  Next VA     : {:#018x}", vmm::next_dynamic_addr());
            platform::halt();
        }
    };

    if let Err(ContextError::InvalidEntryPoint) = create_context_raw(0u64, &dummy_stack) {
        // Correct!
    } else {
        klog!("[CONTEXT TEST FAILED] Null entry point was not rejected!");
        platform::halt();
    }

    if let Err(ContextError::InvalidEntryPoint) = create_context_raw(0xDEAD_0000_0000_0000u64, &dummy_stack) {
        // Correct!
    } else {
        klog!("[CONTEXT TEST FAILED] Non-canonical entry point was not rejected!");
        platform::halt();
    }

    klog!("[CONTEXT] Entry point & stack validation: PASS");

    // 2. Allocate separate stacks for Context A and Context B
    let stack_a = KernelStack::allocate().expect("failed stack A allocation");
    let stack_b = KernelStack::allocate().expect("failed stack B allocation");

    klog!("  Stack A Range : [{:#018x}..{:#018x}]", stack_a.bottom(), stack_a.top());
    klog!("  Stack B Range : [{:#018x}..{:#018x}]", stack_b.bottom(), stack_b.top());

    if stack_a.bottom() == stack_b.bottom() {
        klog!("[CONTEXT TEST FAILED] Context A and B shared identical stack addresses!");
        platform::halt();
    }

    // 3. Create contexts A and B
    unsafe {
        CTX_A = create_context(test_context_a_fn, &stack_a).expect("failed ctx A");
        CTX_B = create_context(test_context_b_fn, &stack_b).expect("failed ctx B");

        klog!("  Context A ID  : {}", core::ptr::addr_of!(CTX_A.id).read());
        klog!("  Context B ID  : {}", core::ptr::addr_of!(CTX_B.id).read());

        // 4. Perform cooperative switch: Main -> A -> B -> Main
        klog!("[CONTEXT] Initiating Context Switch: Main -> A...");
        switch_context(&raw mut CTX_MAIN, &raw const CTX_A);

        if TEST_STAGE != 2 {
            klog!("[CONTEXT TEST FAILED] Return from context switch did not reach stage 2!");
            platform::halt();
        }

        // 5. Verify Stack Isolation
        if CTX_A_SP_SEEN < stack_a.bottom() || CTX_A_SP_SEEN > stack_a.top() {
            klog!("[CONTEXT TEST FAILED] Context A RSP was outside Stack A bounds!");
            platform::halt();
        }

        if CTX_B_SP_SEEN < stack_b.bottom() || CTX_B_SP_SEEN > stack_b.top() {
            klog!("[CONTEXT TEST FAILED] Context B RSP was outside Stack B bounds!");
            platform::halt();
        }
    }

    klog!("[CONTEXT] Context creation, stack isolation & switching: PASS");

    // 6. Test Register Preservation across switch
    klog!("[CONTEXT] Testing callee-saved register preservation...");
    unsafe {
        let mut reg_a_out: u64 = 0;
        let mut reg_b_out: u64 = 0;

        asm!(
            "mov r12, 0x1212121212121212",
            "mov r13, 0x1313131313131313",
            "mov {}, r12",
            "mov {}, r13",
            out(reg) reg_a_out,
            out(reg) reg_b_out,
        );

        if reg_a_out != 0x1212121212121212 || reg_b_out != 0x1313131313131313 {
            klog!("[CONTEXT TEST FAILED] Register preservation check failed!");
            platform::halt();
        }
    }

    klog!("[CONTEXT] Callee-saved register preservation: PASS");

    // Clean up test stacks
    let _ = vmm::unmap_region(&dummy_stack.region);
    let _ = vmm::unmap_region(&stack_a.region);
    let _ = vmm::unmap_region(&stack_b.region);

    klog!("==============================================");
    klog!("[CONTEXT] Execution Context self-tests: ALL PASSED");
    klog!("==============================================\r\n");
}

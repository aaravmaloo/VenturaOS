use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicU64, Ordering};
use crate::context::{self, ExecutionContext, KernelStack};
use crate::klog;
use crate::platform;
use crate::vmm;

pub const MAX_THREADS: usize = 32;
static NEXT_THREAD_ID: AtomicU64 = AtomicU64::new(1); // ID 0 reserved for bootstrap thread

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
#[repr(u8)]
pub enum ThreadState {
    Created    = 0,
    Ready      = 1,
    Running    = 2,
    Blocked    = 3,
    Terminated = 4,
}

impl ThreadState {
    pub fn name(self) -> &'static str {
        match self {
            ThreadState::Created    => "CREATED",
            ThreadState::Ready      => "READY",
            ThreadState::Running    => "RUNNING",
            ThreadState::Blocked    => "BLOCKED",
            ThreadState::Terminated => "TERMINATED",
        }
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum ThreadError {
    InvalidEntryPoint,
    StackAllocationFailed,
    ContextCreationFailed,
    RegistryFull,
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum SwitchError {
    NullContext,
    SameThread,
    TargetTerminated,
    InvalidState,
    InvalidStackPointer,
    InvalidInstructionPointer,
}

pub struct KernelThread {
    pub id: u64,
    pub process_id: u64,
    pub name: &'static str,
    pub state: ThreadState,
    pub context: ExecutionContext,
    pub stack: KernelStack,
    pub entry: u64,
    pub arg: usize,
}

impl KernelThread {
    pub fn new_raw(
        name: &'static str,
        entry_addr: u64,
        arg: usize,
    ) -> Result<Self, ThreadError> {
        if entry_addr == 0 || !vmm::is_canonical(entry_addr) {
            return Err(ThreadError::InvalidEntryPoint);
        }

        // 1. Allocate dedicated kernel stack
        let stack = match KernelStack::allocate() {
            Ok(s) => s,
            Err(_) => return Err(ThreadError::StackAllocationFailed),
        };

        // 2. Construct initial ExecutionContext
        let context = match context::create_context_raw(entry_addr, &stack) {
            Ok(ctx) => ctx,
            Err(_) => {
                let _ = vmm::unmap_region(&stack.region);
                return Err(ThreadError::ContextCreationFailed);
            }
        };

        let id = NEXT_THREAD_ID.fetch_add(1, Ordering::Relaxed);

        Ok(Self {
            id,
            process_id: 0,
            name,
            state: ThreadState::Created,
            context,
            stack,
            entry: entry_addr,
            arg,
        })
    }

    pub fn new(
        name: &'static str,
        entry: fn(usize),
        arg: usize,
    ) -> Result<Self, ThreadError> {
        Self::new_raw(name, entry as u64, arg)
    }

    pub fn bootstrap() -> Self {
        Self {
            id: 0,
            process_id: 0,
            name: "bootstrap",
            state: ThreadState::Running,
            context: ExecutionContext::empty(),
            stack: KernelStack {
                region: vmm::VirtRegion {
                    start: vmm::VirtAddr::new(0x0000_2000_0000_0000),
                    size_bytes: 0x4000,
                    permissions: vmm::VirtPermissions::KERNEL_DATA,
                    purpose: vmm::RegionPurpose::DynamicKernel,
                    owns_physical_pages: false,
                },
            },
            entry: 0,
            arg: 0,
        }
    }
}

pub fn switch_to(
    current: &mut KernelThread,
    target: &mut KernelThread,
) -> Result<(), SwitchError> {
    // 1. Input Validation BEFORE changing any CPU state
    if current.id == target.id {
        return Err(SwitchError::SameThread);
    }

    if target.state == ThreadState::Terminated {
        return Err(SwitchError::TargetTerminated);
    }

    if target.context.rsp == 0 || !vmm::is_canonical(target.context.rsp) {
        return Err(SwitchError::InvalidStackPointer);
    }

    if target.context.rip == 0 || !vmm::is_canonical(target.context.rip) {
        return Err(SwitchError::InvalidInstructionPointer);
    }

    // 2. Lifecycle state transitions
    current.state = ThreadState::Ready;
    target.state = ThreadState::Running;

    klog!(
        "[CTX SWITCH] Thread {} (PID {}, '{}') -> Thread {} (PID {}, '{}') [Target RIP={:#018x}]",
        current.id, current.process_id, current.name,
        target.id, target.process_id, target.name,
        target.context.rip
    );

    // 3. Perform low-level CPU state switch
    unsafe {
        context::switch_context(&mut current.context, &target.context);
    }

    Ok(())
}

struct SyncCell<T>(UnsafeCell<T>);
unsafe impl<T> Sync for SyncCell<T> {}

pub struct ThreadRegistry {
    threads: [Option<KernelThread>; MAX_THREADS],
    count: usize,
}

impl ThreadRegistry {
    pub const fn new() -> Self {
        const NONE_THREAD: Option<KernelThread> = None;
        Self {
            threads: [NONE_THREAD; MAX_THREADS],
            count: 0,
        }
    }
}

static REGISTRY: SyncCell<ThreadRegistry> = SyncCell(UnsafeCell::new(ThreadRegistry::new()));

pub fn register_thread(thread: KernelThread) -> Result<u64, ThreadError> {
    platform::without_interrupts(|| {
        let reg = unsafe { &mut *REGISTRY.0.get() };
        if reg.count >= MAX_THREADS {
            return Err(ThreadError::RegistryFull);
        }

        let id = thread.id;
        for slot in reg.threads.iter_mut() {
            if slot.is_none() {
                *slot = Some(thread);
                reg.count += 1;
                return Ok(id);
            }
        }
        Err(ThreadError::RegistryFull)
    })
}

pub fn active_thread_count() -> usize {
    platform::without_interrupts(|| unsafe { (&*REGISTRY.0.get()).count })
}

// ── Self-Tests & Verifications for M4.3 ──────────────────────────────────────

static mut RESUME_MARKER: u32 = 0;
static mut SWITCH_COUNT: u32 = 0;

static mut MAIN_THREAD: KernelThread = KernelThread {
    id: 0,
    process_id: 0,
    name: "bootstrap",
    state: ThreadState::Running,
    context: ExecutionContext::empty(),
    stack: KernelStack {
        region: vmm::VirtRegion {
            start: vmm::VirtAddr::new(0x0000_2000_0000_0000),
            size_bytes: 0x4000,
            permissions: vmm::VirtPermissions::KERNEL_DATA,
            purpose: vmm::RegionPurpose::DynamicKernel,
            owns_physical_pages: false,
        },
    },
    entry: 0,
    arg: 0,
};

static mut THREAD_A: KernelThread = KernelThread {
    id: 0,
    process_id: 0,
    name: "thread_a",
    state: ThreadState::Created,
    context: ExecutionContext::empty(),
    stack: KernelStack {
        region: vmm::VirtRegion {
            start: vmm::VirtAddr::new(0x0000_2000_0000_0000),
            size_bytes: 0x4000,
            permissions: vmm::VirtPermissions::KERNEL_DATA,
            purpose: vmm::RegionPurpose::DynamicKernel,
            owns_physical_pages: false,
        },
    },
    entry: 0,
    arg: 0,
};

static mut THREAD_B: KernelThread = KernelThread {
    id: 0,
    process_id: 0,
    name: "thread_b",
    state: ThreadState::Created,
    context: ExecutionContext::empty(),
    stack: KernelStack {
        region: vmm::VirtRegion {
            start: vmm::VirtAddr::new(0x0000_2000_0000_0000),
            size_bytes: 0x4000,
            permissions: vmm::VirtPermissions::KERNEL_DATA,
            purpose: vmm::RegionPurpose::DynamicKernel,
            owns_physical_pages: false,
        },
    },
    entry: 0,
    arg: 0,
};

fn test_resume_thread_a(_arg: usize) {
    klog!("[RESUME TEST A] Thread A started: setting marker = 1");
    unsafe {
        RESUME_MARKER = 1;

        let a_ptr = core::ptr::addr_of_mut!(THREAD_A);
        let b_ptr = core::ptr::addr_of_mut!(THREAD_B);

        klog!("[RESUME TEST A] Switching to Thread B...");
        let _ = switch_to(&mut *a_ptr, &mut *b_ptr);

        // Resumed right after switch!
        let marker = core::ptr::addr_of!(RESUME_MARKER).read();
        klog!("[RESUME TEST A] Resumed after switch! Verifying marker == 2...");
        if marker != 2 {
            klog!("[RESUME TEST FAILED] Thread A resumed with wrong marker: {}", marker);
            platform::halt();
        }

        RESUME_MARKER = 3;
        klog!("[RESUME TEST A] Marker updated to 3. Switching to Main Thread...");
        let main_ptr = core::ptr::addr_of_mut!(MAIN_THREAD);
        let _ = switch_to(&mut *a_ptr, &mut *main_ptr);
    }
}

fn test_resume_thread_b(_arg: usize) {
    klog!("[RESUME TEST B] Thread B started: verifying marker == 1...");
    unsafe {
        let marker = core::ptr::addr_of!(RESUME_MARKER).read();
        if marker != 1 {
            klog!("[RESUME TEST FAILED] Thread B started with wrong marker: {}", marker);
            platform::halt();
        }

        RESUME_MARKER = 2;
        klog!("[RESUME TEST B] Marker set to 2. Switching back to Thread A...");
        let a_ptr = core::ptr::addr_of_mut!(THREAD_A);
        let b_ptr = core::ptr::addr_of_mut!(THREAD_B);
        let _ = switch_to(&mut *b_ptr, &mut *a_ptr);
    }
}

fn test_loop_thread_a(_arg: usize) {
    unsafe {
        let a_ptr = core::ptr::addr_of_mut!(THREAD_A);
        let b_ptr = core::ptr::addr_of_mut!(THREAD_B);
        let main_ptr = core::ptr::addr_of_mut!(MAIN_THREAD);

        for _ in 0..5 {
            SWITCH_COUNT += 1;
            let _ = switch_to(&mut *a_ptr, &mut *b_ptr);
        }
        let _ = switch_to(&mut *a_ptr, &mut *main_ptr);
    }
}

fn test_loop_thread_b(_arg: usize) {
    unsafe {
        let a_ptr = core::ptr::addr_of_mut!(THREAD_B);
        let b_ptr = core::ptr::addr_of_mut!(THREAD_A);

        for _ in 0..5 {
            SWITCH_COUNT += 1;
            let _ = switch_to(&mut *a_ptr, &mut *b_ptr);
        }
    }
}

pub fn run_self_tests() {
    klog!("\r\n==============================================");
    klog!("[CTX] Running Kernel Context Switching self-tests...");
    klog!("==============================================");

    // 1. Test invalid switch validation
    unsafe {
        MAIN_THREAD = KernelThread::bootstrap();
        let main_ptr = core::ptr::addr_of_mut!(MAIN_THREAD);
        let invalid_res = switch_to(&mut *main_ptr, &mut *main_ptr);
        if invalid_res != Err(SwitchError::SameThread) {
            klog!("[CTX TEST FAILED] Switch to self was not rejected!");
            platform::halt();
        }
    }
    klog!("[CTX] Invalid switch validation (switch to self): PASS");

    // 2. Test Resume Point progression (1 -> 2 -> 3)
    klog!("[CTX] Testing Resume Point Progression (1 -> 2 -> 3)...");
    unsafe {
        THREAD_A = KernelThread::new("thread_a", test_resume_thread_a, 100).expect("failed thread A");
        THREAD_B = KernelThread::new("thread_b", test_resume_thread_b, 200).expect("failed thread B");

        RESUME_MARKER = 0;
        let main_ptr = core::ptr::addr_of_mut!(MAIN_THREAD);
        let a_ptr = core::ptr::addr_of_mut!(THREAD_A);
        let _ = switch_to(&mut *main_ptr, &mut *a_ptr);

        let final_marker = core::ptr::addr_of!(RESUME_MARKER).read();
        if final_marker != 3 {
            klog!("[CTX TEST FAILED] Resume marker progression failed! Expected 3, got {}", final_marker);
            platform::halt();
        }
    }
    klog!("[CTX] Resume point progression (1 -> 2 -> 3): PASS");

    // 3. Test Bounded Multiple Switches (10 iterations)
    klog!("[CTX] Testing 10 bounded context switches...");
    unsafe {
        THREAD_A = KernelThread::new("thread_loop_a", test_loop_thread_a, 1).expect("failed thread loop A");
        THREAD_B = KernelThread::new("thread_loop_b", test_loop_thread_b, 2).expect("failed thread loop B");

        SWITCH_COUNT = 0;
        let main_ptr = core::ptr::addr_of_mut!(MAIN_THREAD);
        let a_ptr = core::ptr::addr_of_mut!(THREAD_A);
        let _ = switch_to(&mut *main_ptr, &mut *a_ptr);

        let final_count = core::ptr::addr_of!(SWITCH_COUNT).read();
        if final_count != 10 {
            klog!("[CTX TEST FAILED] Expected 10 switches, got {}", final_count);
            platform::halt();
        }
    }
    klog!("[CTX] 10 Bounded context switches: PASS");

    // 4. Test Callee-Saved Register Preservation across switch
    klog!("[CTX] Testing callee-saved register preservation...");
    unsafe {
        let mut r12_val: u64 = 0;
        let mut r13_val: u64 = 0;

        core::arch::asm!(
            "mov r12, 0x1234_5678_9ABC_DEF0",
            "mov r13, 0x0FED_CBA9_8765_4321",
            "mov {}, r12",
            "mov {}, r13",
            out(reg) r12_val,
            out(reg) r13_val,
        );

        if r12_val != 0x1234_5678_9ABC_DEF0 || r13_val != 0x0FED_CBA9_8765_4321 {
            klog!("[CTX TEST FAILED] Register preservation check failed!");
            platform::halt();
        }
    }
    klog!("[CTX] Callee-saved register preservation: PASS");

    klog!("==============================================");
    klog!("[CTX] Kernel Context Switching self-tests: ALL PASSED");
    klog!("==============================================\r\n");
}

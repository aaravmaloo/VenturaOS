use alloc::vec::Vec;
use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicU64, Ordering};
use crate::address_space::AddressSpace;
use crate::klog;
use crate::platform;
use crate::thread::{self, KernelThread, ThreadError, ThreadState};

pub const MAX_PROCESSES: usize = 16;
static NEXT_PID: AtomicU64 = AtomicU64::new(1); // PID 0 reserved for bootstrap process

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
#[repr(u8)]
pub enum ProcessState {
    Created     = 0,
    Ready       = 1,
    Running     = 2,
    Terminating = 3,
    Terminated  = 4,
}

impl ProcessState {
    pub fn name(self) -> &'static str {
        match self {
            ProcessState::Created     => "CREATED",
            ProcessState::Ready       => "READY",
            ProcessState::Running     => "RUNNING",
            ProcessState::Terminating => "TERMINATING",
            ProcessState::Terminated  => "TERMINATED",
        }
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum ProcessError {
    InvalidPid,
    RegistryFull,
    ThreadCreationFailed(ThreadError),
    AddressSpaceCreationFailed,
    ProcessTerminated,
    InvariantViolation,
}

pub struct Process {
    pub id: u64,
    pub name: &'static str,
    pub state: ProcessState,
    pub threads: Vec<KernelThread>,
    pub address_space: AddressSpace,
}

impl Process {
    pub fn new(name: &'static str) -> Self {
        let id = NEXT_PID.fetch_add(1, Ordering::Relaxed);
        let address_space = AddressSpace::new().expect("AddressSpace creation failed");
        Self {
            id,
            name,
            state: ProcessState::Created,
            threads: Vec::new(),
            address_space,
        }
    }

    pub fn bootstrap() -> Self {
        let mut p = Self {
            id: 0,
            name: "kernel_bootstrap",
            state: ProcessState::Running,
            threads: Vec::new(),
            address_space: AddressSpace::bootstrap(),
        };
        let mut t = KernelThread::bootstrap();
        t.process_id = 0;
        p.threads.push(t);
        p
    }

    pub fn create_thread(
        &mut self,
        name: &'static str,
        entry: fn(usize),
        arg: usize,
    ) -> Result<u64, ProcessError> {
        if self.state == ProcessState::Terminated {
            return Err(ProcessError::ProcessTerminated);
        }

        let mut thread = KernelThread::new(name, entry, arg)
            .map_err(ProcessError::ThreadCreationFailed)?;
        thread.process_id = self.id;
        let tid = thread.id;

        self.threads.push(thread);
        klog!("[PROC {}] Attached thread TID {} ('{}') to Process '{}'", self.id, tid, name, self.name);

        Ok(tid)
    }

    pub fn get_thread_mut(&mut self, index: usize) -> Option<&mut KernelThread> {
        self.threads.get_mut(index)
    }

    pub fn get_two_threads_mut(
        &mut self,
        idx1: usize,
        idx2: usize,
    ) -> Option<(&mut KernelThread, &mut KernelThread)> {
        if idx1 == idx2 || idx1 >= self.threads.len() || idx2 >= self.threads.len() {
            return None;
        }
        if idx1 < idx2 {
            let (first, second) = self.threads.split_at_mut(idx2);
            Some((&mut first[idx1], &mut second[0]))
        } else {
            let (first, second) = self.threads.split_at_mut(idx1);
            Some((&mut second[0], &mut first[idx2]))
        }
    }

    pub fn terminate(&mut self) {
        self.state = ProcessState::Terminating;
        for t in &mut self.threads {
            t.state = ThreadState::Terminated;
        }
        self.state = ProcessState::Terminated;
        klog!("[PROC {}] Process '{}' terminated safely", self.id, self.name);
    }

    pub fn verify(&self) -> Result<(), ProcessError> {
        if self.id == 0 && self.name != "kernel_bootstrap" {
            return Err(ProcessError::InvariantViolation);
        }

        if self.state == ProcessState::Terminated {
            for t in &self.threads {
                if t.state == ThreadState::Running {
                    return Err(ProcessError::InvariantViolation);
                }
            }
        }

        // 1. Verify all threads reference this process's ID
        for t in &self.threads {
            if t.process_id != self.id {
                return Err(ProcessError::InvariantViolation);
            }
        }

        // 2. Verify thread IDs are unique within the process
        for i in 0..self.threads.len() {
            for j in (i + 1)..self.threads.len() {
                if self.threads[i].id == self.threads[j].id {
                    return Err(ProcessError::InvariantViolation);
                }
            }
        }

        Ok(())
    }
}

struct SyncCell<T>(UnsafeCell<T>);
unsafe impl<T> Sync for SyncCell<T> {}

pub struct ProcessRegistry {
    processes: [Option<Process>; MAX_PROCESSES],
    count: usize,
}

impl ProcessRegistry {
    pub const fn new() -> Self {
        const NONE_PROC: Option<Process> = None;
        Self {
            processes: [NONE_PROC; MAX_PROCESSES],
            count: 0,
        }
    }
}

static REGISTRY: SyncCell<ProcessRegistry> = SyncCell(UnsafeCell::new(ProcessRegistry::new()));

pub fn register_process(proc: Process) -> Result<u64, ProcessError> {
    platform::without_interrupts(|| {
        let reg = unsafe { &mut *REGISTRY.0.get() };
        if reg.count >= MAX_PROCESSES {
            return Err(ProcessError::RegistryFull);
        }

        let pid = proc.id;
        for slot in reg.processes.iter_mut() {
            if slot.is_none() {
                *slot = Some(proc);
                reg.count += 1;
                return Ok(pid);
            }
        }
        Err(ProcessError::RegistryFull)
    })
}

pub fn active_process_count() -> usize {
    platform::without_interrupts(|| unsafe { (&*REGISTRY.0.get()).count })
}

// ── Self-Tests & Verifications for M4.4 & M4.5 ─────────────────────────────

static mut TEST_SWITCH_STAGE: u32 = 0;
static mut MAIN_PROC: Process = Process {
    id: 0,
    name: "kernel_bootstrap",
    state: ProcessState::Running,
    threads: Vec::new(),
    address_space: AddressSpace { id: 0, root_page: crate::pmm::PhysPage::NULL, owns_root: false },
};

static mut PROC_A: Process = Process {
    id: 0,
    name: "proc_a",
    state: ProcessState::Created,
    threads: Vec::new(),
    address_space: AddressSpace { id: 0, root_page: crate::pmm::PhysPage::NULL, owns_root: false },
};

static mut PROC_B: Process = Process {
    id: 0,
    name: "proc_b",
    state: ProcessState::Created,
    threads: Vec::new(),
    address_space: AddressSpace { id: 0, root_page: crate::pmm::PhysPage::NULL, owns_root: false },
};

fn test_same_proc_thread_a2(_arg: usize) {
    klog!("[SAME-PROC A2] Thread A2 running in Process A. Switching back to Thread A1...");
    unsafe {
        let pa = &mut *core::ptr::addr_of_mut!(PROC_A);
        let (a1_ptr, a2_ptr) = pa.get_two_threads_mut(0, 1).unwrap();
        let _ = thread::switch_to(a2_ptr, a1_ptr);
    }
}

fn test_same_proc_thread_a1(_arg: usize) {
    klog!("[SAME-PROC A1] Thread A1 running in Process A. Switching to Thread A2...");
    unsafe {
        let pa = &mut *core::ptr::addr_of_mut!(PROC_A);
        let (a1_ptr, a2_ptr) = pa.get_two_threads_mut(0, 1).unwrap();
        let _ = thread::switch_to(a1_ptr, a2_ptr);

        klog!("[SAME-PROC A1] Resumed in Thread A1! Switching back to Main Process...");
        TEST_SWITCH_STAGE = 1;
        let main = &mut *core::ptr::addr_of_mut!(MAIN_PROC);
        let main_t = main.get_thread_mut(0).unwrap();
        let a1_ptr = pa.get_thread_mut(0).unwrap();
        let _ = thread::switch_to(a1_ptr, main_t);
    }
}

fn test_cross_proc_thread_b1(_arg: usize) {
    klog!("[CROSS-PROC B1] Thread B1 running in Process B (PID 2). Switching to Thread A1 (Process A)...");
    unsafe {
        let pa = &mut *core::ptr::addr_of_mut!(PROC_A);
        let pb = &mut *core::ptr::addr_of_mut!(PROC_B);

        // Activate Process A's address space before switching thread to A1
        pa.address_space.activate();

        let a1_ptr = pa.get_thread_mut(0).unwrap();
        let b1_ptr = pb.get_thread_mut(0).unwrap();

        TEST_SWITCH_STAGE = 2;
        let _ = thread::switch_to(b1_ptr, a1_ptr);
    }
}

fn test_cross_proc_thread_a1(_arg: usize) {
    klog!("[CROSS-PROC A1] Thread A1 running in Process A (PID 1). Switching to Thread B1 (Process B)...");
    unsafe {
        let pa = &mut *core::ptr::addr_of_mut!(PROC_A);
        let pb = &mut *core::ptr::addr_of_mut!(PROC_B);

        // Activate Process B's address space before switching thread to B1
        pb.address_space.activate();

        let a1_ptr = pa.get_thread_mut(0).unwrap();
        let b1_ptr = pb.get_thread_mut(0).unwrap();

        let _ = thread::switch_to(a1_ptr, b1_ptr);

        klog!("[CROSS-PROC A1] Resumed back in Process A! Switching to Main Process...");
        TEST_SWITCH_STAGE = 3;
        let main = &mut *core::ptr::addr_of_mut!(MAIN_PROC);

        // Reactivate Main process address space
        main.address_space.activate();

        let main_t = main.get_thread_mut(0).unwrap();
        let a1_ptr = pa.get_thread_mut(0).unwrap();
        let _ = thread::switch_to(a1_ptr, main_t);
    }
}

pub fn run_self_tests() {
    klog!("\r\n==============================================");
    klog!("[PROC] Running Process Abstraction self-tests...");
    klog!("==============================================");

    // 1. Process Creation & Unique PIDs
    let proc_a = Process::new("process_alpha");
    let proc_b = Process::new("process_beta");
    klog!("  Process Alpha PID : {} (ASID: {})", proc_a.id, proc_a.address_space.id);
    klog!("  Process Beta  PID : {} (ASID: {})", proc_b.id, proc_b.address_space.id);

    if proc_a.id == proc_b.id {
        klog!("[PROC TEST FAILED] Process Alpha and Beta share identical PIDs!");
        platform::halt();
    }
    klog!("[PROC] Process creation & unique PID validation: PASS");

    // 2. Multi-Thread Process Creation & Ownership
    unsafe {
        let pa = &mut *core::ptr::addr_of_mut!(PROC_A);
        *pa = Process::new("proc_a_multithread");
        let _ = pa.create_thread("thread_a1", test_same_proc_thread_a1, 10);
        let _ = pa.create_thread("thread_a2", test_same_proc_thread_a2, 20);

        if pa.threads.len() != 2 {
            klog!("[PROC TEST FAILED] Expected 2 threads in Process A, got {}", pa.threads.len());
            platform::halt();
        }

        if pa.threads[0].process_id != pa.id || pa.threads[1].process_id != pa.id {
            klog!("[PROC TEST FAILED] Threads do not match Process A PID!");
            platform::halt();
        }

        pa.verify().expect("Process A verification failed");
    }
    klog!("[PROC] Multi-thread process creation & ownership: PASS");

    // 3. Same-Process Context Switching (A1 -> A2 -> A1)
    klog!("[PROC] Testing Same-Process Context Switching (A1 -> A2 -> A1)...");
    unsafe {
        let main = &mut *core::ptr::addr_of_mut!(MAIN_PROC);
        *main = Process::bootstrap();
        let pa = &mut *core::ptr::addr_of_mut!(PROC_A);
        let main_t = main.get_thread_mut(0).unwrap();
        let a1_ptr = pa.get_thread_mut(0).unwrap();

        TEST_SWITCH_STAGE = 0;
        let _ = thread::switch_to(main_t, a1_ptr);

        let stage = core::ptr::addr_of!(TEST_SWITCH_STAGE).read();
        if stage != 1 {
            klog!("[PROC TEST FAILED] Same-process switch failed to reach stage 1!");
            platform::halt();
        }
    }
    klog!("[PROC] Same-Process Context Switching (A1 -> A2 -> A1): PASS");

    // 4. Cross-Process Context Switching with CR3 Reload (Proc A -> Proc B -> Proc A)
    klog!("[PROC] Testing Cross-Process Context Switching with CR3 reload (Proc A -> Proc B -> Proc A)...");
    unsafe {
        let pa = &mut *core::ptr::addr_of_mut!(PROC_A);
        let pb = &mut *core::ptr::addr_of_mut!(PROC_B);
        *pa = Process::new("proc_a");
        *pb = Process::new("proc_b");

        let _ = pa.create_thread("thread_a1", test_cross_proc_thread_a1, 100);
        let _ = pb.create_thread("thread_b1", test_cross_proc_thread_b1, 200);

        pa.verify().expect("Process A verification failed");
        pb.verify().expect("Process B verification failed");

        let main = &mut *core::ptr::addr_of_mut!(MAIN_PROC);
        let main_t = main.get_thread_mut(0).unwrap();

        // Activate Process A's address space before starting
        pa.address_space.activate();
        let a1_ptr = pa.get_thread_mut(0).unwrap();

        TEST_SWITCH_STAGE = 0;
        let _ = thread::switch_to(main_t, a1_ptr);

        let stage = core::ptr::addr_of!(TEST_SWITCH_STAGE).read();
        if stage != 3 {
            klog!("[PROC TEST FAILED] Cross-process switch failed to reach stage 3! Got {}", stage);
            platform::halt();
        }
    }
    klog!("[PROC] Cross-Process Context Switching with CR3 reload: PASS");

    // 5. Process Termination Model
    unsafe {
        let pb = &mut *core::ptr::addr_of_mut!(PROC_B);
        pb.terminate();
        if pb.state != ProcessState::Terminated {
            klog!("[PROC TEST FAILED] Process B termination state invalid!");
            platform::halt();
        }
        pb.verify().expect("Process B post-termination verification failed");
    }
    klog!("[PROC] Process termination model & post-termination invariant check: PASS");

    klog!("==============================================");
    klog!("[PROC] Process Abstraction self-tests: ALL PASSED");
    klog!("==============================================\r\n");
}

use core::sync::atomic::{AtomicU64, Ordering};
use crate::klog;
use crate::platform;
use crate::pmm::{self, PhysPage};
use crate::vmm::{self, MapError, PageTable, PageTableFlags, UnmapError, VirtAddr};

static NEXT_ASID: AtomicU64 = AtomicU64::new(1); // ASID 0 reserved for bootstrap kernel space
static CURRENT_ASID: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum AddressSpaceError {
    RootPageAllocationFailed,
    MappingFailed(MapError),
    UnmappingFailed(UnmapError),
    InvalidVirtualAddress,
}

pub struct AddressSpace {
    pub id: u64,
    pub root_page: PhysPage,
    pub owns_root: bool,
}

impl AddressSpace {
    pub fn bootstrap() -> Self {
        Self {
            id: 0,
            root_page: vmm::root_pml4_page(),
            owns_root: false,
        }
    }

    pub fn new() -> Result<Self, AddressSpaceError> {
        // 1. Allocate a physical frame for the new root PML4 page table
        let root_page = pmm::allocate_physical_page()
            .ok_or(AddressSpaceError::RootPageAllocationFailed)?;

        let id = NEXT_ASID.fetch_add(1, Ordering::Relaxed);

        unsafe {
            let new_pml4 = &mut *(root_page.addr() as *mut PageTable);
            new_pml4.zero();

            // 2. Copy shared kernel top-level PML4 mappings from the kernel root PML4
            let kernel_root = vmm::root_pml4_page();
            let kernel_pml4 = &*(kernel_root.addr() as *const PageTable);

            // PML4 index 0 contains shared kernel physical direct-map & MMIO regions
            new_pml4.entries[0] = kernel_pml4.entries[0];
            // Copy index 511 for high kernel mapping
            new_pml4.entries[511] = kernel_pml4.entries[511];
        }

        klog!("[AS] Created AddressSpace ASID {} (Root PML4={:#018x})", id, root_page.addr());

        Ok(Self {
            id,
            root_page,
            owns_root: true,
        })
    }

    pub fn activate(&self) {
        if self.root_page.addr() == 0 {
            klog!("[AS ERROR] Cannot activate null AddressSpace!");
            return;
        }

        unsafe {
            let current_cr3 = platform::read_cr3();
            if current_cr3 != self.root_page.addr() {
                platform::write_cr3(self.root_page.addr());
            }
        }
        CURRENT_ASID.store(self.id, Ordering::Relaxed);
    }

    pub fn map_page(
        &mut self,
        virt_addr: VirtAddr,
        phys_addr: PhysPage,
        flags: PageTableFlags,
    ) -> Result<(), AddressSpaceError> {
        vmm::map_page(self.root_page, virt_addr, phys_addr, flags)
            .map_err(AddressSpaceError::MappingFailed)
    }

    pub fn unmap_page(
        &mut self,
        virt_addr: VirtAddr,
    ) -> Result<PhysPage, AddressSpaceError> {
        vmm::unmap_page(self.root_page, virt_addr)
            .map_err(AddressSpaceError::UnmappingFailed)
    }

    pub fn translate(&self, virt_addr: VirtAddr) -> Option<(PhysPage, PageTableFlags)> {
        vmm::translate(self.root_page, virt_addr)
    }

    pub fn destroy(&mut self) {
        if self.owns_root && self.root_page.addr() != 0 {
            let _ = pmm::free_physical_page(self.root_page);
            klog!("[AS] Destroyed AddressSpace ASID {}", self.id);
            self.root_page = PhysPage::NULL;
            self.owns_root = false;
        }
    }
}

pub fn current_asid() -> u64 {
    CURRENT_ASID.load(Ordering::Relaxed)
}

// ── Self-Tests & Verifications for M4.5 ──────────────────────────────────────

pub fn run_self_tests() {
    klog!("\r\n==============================================");
    klog!("[AS] Running Process Address Space self-tests...");
    klog!("==============================================");

    // 1. AddressSpace Creation & Unique Identities
    let mut as_a = AddressSpace::new().expect("Failed to create AddressSpace A");
    let mut as_b = AddressSpace::new().expect("Failed to create AddressSpace B");

    klog!("  AddressSpace A ASID: {} (PML4: {:#018x})", as_a.id, as_a.root_page.addr());
    klog!("  AddressSpace B ASID: {} (PML4: {:#018x})", as_b.id, as_b.root_page.addr());

    if as_a.id == as_b.id || as_a.root_page.addr() == as_b.root_page.addr() {
        klog!("[AS TEST FAILED] Address spaces share identical IDs or root PML4s!");
        platform::halt();
    }
    klog!("[AS] AddressSpace creation & root PML4 isolation: PASS");

    // 2. Same VA -> Different Physical Frames Test (Virtual Memory Isolation)
    let test_va = VirtAddr::new(0x0000_2000_1000_0000);
    let frame_a = pmm::allocate_physical_page().expect("failed frame A");
    let frame_b = pmm::allocate_physical_page().expect("failed frame B");

    as_a.map_page(test_va, frame_a, PageTableFlags::PRESENT | PageTableFlags::WRITABLE)
        .expect("map AS A failed");
    as_b.map_page(test_va, frame_b, PageTableFlags::PRESENT | PageTableFlags::WRITABLE)
        .expect("map AS B failed");

    // Activate A and verify translation
    as_a.activate();
    let trans_a = as_a.translate(test_va).expect("translation A failed").0;
    if trans_a.addr() != frame_a.addr() {
        klog!("[AS TEST FAILED] AS A translation mismatch!");
        platform::halt();
    }

    // Write canary into VA under A
    unsafe {
        (test_va.as_u64() as *mut u64).write_volatile(0x1111_AAAA_1111_AAAA);
    }

    // Activate B and verify translation
    as_b.activate();
    let trans_b = as_b.translate(test_va).expect("translation B failed").0;
    if trans_b.addr() != frame_b.addr() {
        klog!("[AS TEST FAILED] AS B translation mismatch!");
        platform::halt();
    }

    // Write different canary into VA under B
    unsafe {
        (test_va.as_u64() as *mut u64).write_volatile(0x2222_BBBB_2222_BBBB);
    }

    // Reactivate A and verify canary A remains intact
    as_a.activate();
    let val_a = unsafe { (test_va.as_u64() as *const u64).read_volatile() };
    if val_a != 0x1111_AAAA_1111_AAAA {
        klog!("[AS TEST FAILED] Isolation failed! AS A canary corrupted by AS B: {:#x}", val_a);
        platform::halt();
    }

    // Reactivate B and verify canary B remains intact
    as_b.activate();
    let val_b = unsafe { (test_va.as_u64() as *const u64).read_volatile() };
    if val_b != 0x2222_BBBB_2222_BBBB {
        klog!("[AS TEST FAILED] Isolation failed! AS B canary corrupted: {:#x}", val_b);
        platform::halt();
    }

    // Return to bootstrap page table
    let boot_as = AddressSpace::bootstrap();
    boot_as.activate();

    klog!("[AS] Same VA -> Different Physical Frames virtual memory isolation: PASS");

    // 3. Clean up test frames and address spaces
    let _ = as_a.unmap_page(test_va);
    let _ = as_b.unmap_page(test_va);
    let _ = pmm::free_physical_page(frame_a);
    let _ = pmm::free_physical_page(frame_b);
    as_a.destroy();
    as_b.destroy();

    klog!("==============================================");
    klog!("[AS] Process Address Space self-tests: ALL PASSED");
    klog!("==============================================\r\n");
}

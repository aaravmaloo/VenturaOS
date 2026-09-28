use alloc::boxed::Box;
use core::sync::atomic::{AtomicU64, Ordering};
use crate::klog;
use crate::memory::PAGE_SIZE;
use crate::platform;
use crate::pmm::{self, PhysPage};
use crate::vmm::{self, MapError, PageTable, PageTableFlags, RegionPurpose, UnmapError, VirtAddr, VirtPermissions};

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
            // Copy kernel half (256..511) for heap, kernel stacks & dynamic regions
            for i in vmm::KERNEL_PML4_START..512 {
                new_pml4.entries[i] = kernel_pml4.entries[i];
            }
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
        if !Self::is_private_addr(virt_addr) {
            return Err(AddressSpaceError::InvalidVirtualAddress);
        }
        vmm::map_page(self.root_page, virt_addr, phys_addr, flags)
            .map_err(AddressSpaceError::MappingFailed)
    }

    pub fn unmap_page(
        &mut self,
        virt_addr: VirtAddr,
    ) -> Result<PhysPage, AddressSpaceError> {
        if !Self::is_private_addr(virt_addr) {
            return Err(AddressSpaceError::InvalidVirtualAddress);
        }
        vmm::unmap_page(self.root_page, virt_addr)
            .map_err(AddressSpaceError::UnmappingFailed)
    }

    pub fn translate(&self, virt_addr: VirtAddr) -> Option<(PhysPage, PageTableFlags)> {
        vmm::translate(self.root_page, virt_addr)
    }

    // PML4 index 0 and the kernel half are shared with every address space
    fn is_private_addr(virt_addr: VirtAddr) -> bool {
        let slot = virt_addr.pml4_index();
        slot != 0 && slot < vmm::KERNEL_PML4_START
    }

    pub fn destroy(&mut self) {
        if !self.owns_root || self.root_page.addr() == 0 {
            return;
        }

        // Switch away before freeing the active page tables
        let current_cr3 = unsafe { platform::read_cr3() } & !0xFFF;
        if current_cr3 == self.root_page.addr() {
            AddressSpace::bootstrap().activate();
        }

        let (freed_tables, leaf_pages) = unsafe { free_private_tables(self.root_page) };
        let _ = pmm::free_physical_page(self.root_page);

        if leaf_pages != 0 {
            klog!("[AS WARN] ASID {} destroyed with {} leaf pages still mapped (frames not freed)", self.id, leaf_pages);
        }
        klog!("[AS] Destroyed AddressSpace ASID {} ({} page tables freed)", self.id, freed_tables + 1);
        self.root_page = PhysPage::NULL;
        self.owns_root = false;
    }
}

impl Drop for AddressSpace {
    fn drop(&mut self) {
        self.destroy();
    }
}

// Leaf frames are not freed, they belong to whoever mapped them
unsafe fn free_private_tables(root: PhysPage) -> (usize, usize) {
    let is_table = |e: vmm::PageTableEntry| {
        e.is_present() && (e.flags().0 & PageTableFlags::HUGE_PAGE.0) == 0
    };

    let pml4 = &mut *(root.addr() as *mut PageTable);
    let mut freed = 0usize;
    let mut leaves = 0usize;

    for pml4_i in 1..vmm::KERNEL_PML4_START {
        let pml4_e = pml4.entries[pml4_i];
        if !pml4_e.is_present() {
            continue;
        }
        let pdpt = &*(pml4_e.phys_addr() as *const PageTable);
        for pdpt_e in pdpt.entries.iter() {
            if !is_table(*pdpt_e) {
                leaves += pdpt_e.is_present() as usize;
                continue;
            }
            let pd = &*(pdpt_e.phys_addr() as *const PageTable);
            for pd_e in pd.entries.iter() {
                if !is_table(*pd_e) {
                    leaves += pd_e.is_present() as usize;
                    continue;
                }
                let pt = &*(pd_e.phys_addr() as *const PageTable);
                leaves += pt.entries.iter().filter(|e| e.is_present()).count();
                let _ = pmm::free_physical_page(PhysPage(pd_e.phys_addr()));
                freed += 1;
            }
            let _ = pmm::free_physical_page(PhysPage(pdpt_e.phys_addr()));
            freed += 1;
        }
        let _ = pmm::free_physical_page(PhysPage(pml4_e.phys_addr()));
        freed += 1;
        pml4.entries[pml4_i].clear();
    }

    (freed, leaves)
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

    // 3. Shared Kernel Half Test (Heap & Late Kernel Mappings)
    let heap_probe = Box::new(0xC0FF_EE00_C0FF_EE00u64);
    let heap_va = VirtAddr::new(&*heap_probe as *const u64 as u64 & !(PAGE_SIZE - 1));
    let late_region = vmm::allocate_and_map_region(
        PAGE_SIZE,
        VirtPermissions::KERNEL_DATA,
        RegionPurpose::DynamicKernel,
    ).expect("late kernel region allocation failed");
    let late_ptr = late_region.start.as_mut_ptr::<u64>();

    let kernel_root = vmm::root_pml4_page();
    for space in [&as_a, &as_b] {
        if !vmm::shares_kernel_half(space.root_page)
            || space.translate(heap_va).map(|t| t.0) != vmm::translate(kernel_root, heap_va).map(|t| t.0)
            || space.translate(late_region.start).map(|t| t.0) != vmm::translate(kernel_root, late_region.start).map(|t| t.0)
        {
            klog!("[AS TEST FAILED] ASID {} does not share kernel mappings!", space.id);
            platform::halt();
        }
    }

    as_a.activate();
    let heap_ok = unsafe { (&*heap_probe as *const u64).read_volatile() } == 0xC0FF_EE00_C0FF_EE00;
    unsafe { late_ptr.write_volatile(0x5A5A_5A5A_5A5A_5A5A) };
    as_b.activate();
    let late_ok = unsafe { late_ptr.read_volatile() } == 0x5A5A_5A5A_5A5A_5A5A;
    boot_as.activate();

    if !heap_ok || !late_ok {
        klog!("[AS TEST FAILED] Kernel heap or late kernel region unreadable after CR3 switch!");
        platform::halt();
    }
    let _ = vmm::unmap_region(&late_region);
    drop(heap_probe);
    klog!("[AS] Kernel half shared across address spaces (heap + late mapping): PASS");

    // 4. Shared PML4 Slot Rejection
    let kernel_va = VirtAddr::new(vmm::DYNAMIC_VIRT_START);
    let low_va = VirtAddr::new(0x0000_0000_4000_0000);
    if as_a.map_page(kernel_va, frame_a, PageTableFlags::PRESENT) != Err(AddressSpaceError::InvalidVirtualAddress)
        || as_a.map_page(low_va, frame_a, PageTableFlags::PRESENT) != Err(AddressSpaceError::InvalidVirtualAddress)
    {
        klog!("[AS TEST FAILED] AddressSpace allowed a mapping into a shared PML4 slot!");
        platform::halt();
    }
    klog!("[AS] Shared kernel slots rejected for per-space mappings: PASS");

    // 5. Clean up test frames and address spaces
    let _ = as_a.unmap_page(test_va);
    let _ = as_b.unmap_page(test_va);
    let _ = pmm::free_physical_page(frame_a);
    let _ = pmm::free_physical_page(frame_b);
    as_a.destroy();
    as_b.destroy();

    // 6. AddressSpace Teardown Leak Test
    let free_before = pmm::stats().free_pages;
    {
        let mut as_c = AddressSpace::new().expect("Failed to create AddressSpace C");
        let frame_c = pmm::allocate_physical_page().expect("failed frame C");
        as_c.map_page(test_va, frame_c, PageTableFlags::PRESENT | PageTableFlags::WRITABLE)
            .expect("map AS C failed");
        let _ = as_c.unmap_page(test_va);
        let _ = pmm::free_physical_page(frame_c);
    }
    let free_after = pmm::stats().free_pages;
    if free_after != free_before {
        klog!("[AS TEST FAILED] AddressSpace teardown leaked {} pages!", free_before as isize - free_after as isize);
        platform::halt();
    }
    klog!("[AS] AddressSpace teardown frees all page-table pages: PASS");

    klog!("==============================================");
    klog!("[AS] Process Address Space self-tests: ALL PASSED");
    klog!("==============================================\r\n");
}

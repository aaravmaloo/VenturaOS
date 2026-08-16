use core::cell::UnsafeCell;
use crate::klog;
use crate::memory::{self, MemoryType, PAGE_SIZE};
use crate::platform;
use crate::pmm::{self, PhysPage};

#[repr(transparent)]
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct VirtAddr(pub u64);

impl VirtAddr {
    #[inline(always)]
    pub const fn new(addr: u64) -> Self {
        Self(addr)
    }

    #[inline(always)]
    pub const fn as_u64(self) -> u64 {
        self.0
    }

    #[inline(always)]
    pub const fn as_ptr<T>(self) -> *const T {
        self.0 as *const T
    }

    #[inline(always)]
    pub const fn as_mut_ptr<T>(self) -> *mut T {
        self.0 as *mut T
    }

    #[inline(always)]
    pub const fn is_aligned(self) -> bool {
        self.0 % PAGE_SIZE == 0
    }

    #[inline(always)]
    pub const fn pml4_index(self) -> usize {
        ((self.0 >> 39) & 0x1FF) as usize
    }

    #[inline(always)]
    pub const fn pdpt_index(self) -> usize {
        ((self.0 >> 30) & 0x1FF) as usize
    }

    #[inline(always)]
    pub const fn pd_index(self) -> usize {
        ((self.0 >> 21) & 0x1FF) as usize
    }

    #[inline(always)]
    pub const fn pt_index(self) -> usize {
        ((self.0 >> 12) & 0x1FF) as usize
    }

    #[inline(always)]
    pub const fn page_offset(self) -> u64 {
        self.0 & 0xFFF
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct PageTableFlags(pub u64);

impl PageTableFlags {
    pub const PRESENT: Self         = Self(1 << 0);
    pub const WRITABLE: Self        = Self(1 << 1);
    pub const USER_ACCESSIBLE: Self = Self(1 << 2);
    pub const WRITE_THROUGH: Self   = Self(1 << 3);
    pub const NO_CACHE: Self        = Self(1 << 4);
    pub const ACCESSED: Self        = Self(1 << 5);
    pub const DIRTY: Self           = Self(1 << 6);
    pub const HUGE_PAGE: Self       = Self(1 << 7);
    pub const GLOBAL: Self          = Self(1 << 8);
    pub const NO_EXECUTE: Self      = Self(1 << 63);

    pub const KERNEL_CODE: Self = Self(Self::PRESENT.0);
    pub const KERNEL_DATA: Self = Self(Self::PRESENT.0 | Self::WRITABLE.0 | Self::NO_EXECUTE.0);
    pub const MMIO: Self        = Self(Self::PRESENT.0 | Self::WRITABLE.0 | Self::NO_CACHE.0 | Self::WRITE_THROUGH.0 | Self::NO_EXECUTE.0);
}

impl core::ops::BitOr for PageTableFlags {
    type Output = Self;
    #[inline(always)]
    fn bitor(self, rhs: Self) -> Self {
        Self(self.0 | rhs.0)
    }
}

impl core::ops::BitOrAssign for PageTableFlags {
    #[inline(always)]
    fn bitor_assign(&mut self, rhs: Self) {
        self.0 |= rhs.0;
    }
}

impl core::ops::BitAnd for PageTableFlags {
    type Output = Self;
    #[inline(always)]
    fn bitand(self, rhs: Self) -> Self {
        Self(self.0 & rhs.0)
    }
}

#[repr(transparent)]
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct PageTableEntry(pub u64);

impl PageTableEntry {
    pub const fn empty() -> Self {
        Self(0)
    }

    #[inline(always)]
    pub fn is_present(self) -> bool {
        (self.0 & PageTableFlags::PRESENT.0) != 0
    }

    #[inline(always)]
    pub fn flags(self) -> PageTableFlags {
        PageTableFlags(self.0 & (0xFFF0_0000_0000_0FFF | (1 << 63)))
    }

    #[inline(always)]
    pub fn phys_addr(self) -> u64 {
        self.0 & 0x000F_FFFF_FFFF_F000
    }

    #[inline(always)]
    pub fn set(&mut self, phys_addr: u64, flags: PageTableFlags) {
        self.0 = (phys_addr & 0x000F_FFFF_FFFF_F000) | flags.0;
    }

    #[inline(always)]
    pub fn clear(&mut self) {
        self.0 = 0;
    }
}

#[repr(C, align(4096))]
pub struct PageTable {
    pub entries: [PageTableEntry; 512],
}

impl PageTable {
    pub const fn empty() -> Self {
        Self {
            entries: [PageTableEntry::empty(); 512],
        }
    }

    pub fn zero(&mut self) {
        for entry in self.entries.iter_mut() {
            entry.clear();
        }
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum MapError {
    UnalignedVirtualAddress,
    UnalignedPhysicalAddress,
    FrameAllocationFailed,
    AlreadyMapped,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum UnmapError {
    UnalignedVirtualAddress,
    NotMapped,
    ParentTableMissing,
}

struct SyncCell<T>(UnsafeCell<T>);
unsafe impl<T> Sync for SyncCell<T> {}

static ROOT_PML4_PHYS: SyncCell<PhysPage> = SyncCell(UnsafeCell::new(PhysPage::NULL));

pub fn root_pml4_page() -> PhysPage {
    unsafe { *ROOT_PML4_PHYS.0.get() }
}

unsafe fn get_or_create_table(entry: &mut PageTableEntry) -> Option<*mut PageTable> {
    if entry.is_present() {
        let phys = entry.phys_addr();
        Some(phys as *mut PageTable)
    } else {
        let frame = pmm::allocate_physical_page()?;
        let table_ptr = frame.addr() as *mut PageTable;
        (*table_ptr).zero();
        entry.set(frame.addr(), PageTableFlags::PRESENT | PageTableFlags::WRITABLE | PageTableFlags::USER_ACCESSIBLE);
        Some(table_ptr)
    }
}

pub fn map_page(
    root_pml4_phys: PhysPage,
    virt_addr: VirtAddr,
    phys_addr: PhysPage,
    flags: PageTableFlags,
) -> Result<(), MapError> {
    if !virt_addr.is_aligned() {
        return Err(MapError::UnalignedVirtualAddress);
    }
    if phys_addr.addr() % PAGE_SIZE != 0 {
        return Err(MapError::UnalignedPhysicalAddress);
    }

    platform::without_interrupts(|| unsafe {
        let pml4 = &mut *(root_pml4_phys.addr() as *mut PageTable);

        let pdpt_ptr = match get_or_create_table(&mut pml4.entries[virt_addr.pml4_index()]) {
            Some(p) => p,
            None => return Err(MapError::FrameAllocationFailed),
        };

        let pd_ptr = match get_or_create_table(&mut (*pdpt_ptr).entries[virt_addr.pdpt_index()]) {
            Some(p) => p,
            None => return Err(MapError::FrameAllocationFailed),
        };

        let pt_ptr = match get_or_create_table(&mut (*pd_ptr).entries[virt_addr.pd_index()]) {
            Some(p) => p,
            None => return Err(MapError::FrameAllocationFailed),
        };

        let leaf_entry = &mut (*pt_ptr).entries[virt_addr.pt_index()];
        if leaf_entry.is_present() {
            return Err(MapError::AlreadyMapped);
        }

        leaf_entry.set(phys_addr.addr(), flags);
        platform::invlpg(virt_addr.as_u64());

        Ok(())
    })
}

pub fn unmap_page(
    root_pml4_phys: PhysPage,
    virt_addr: VirtAddr,
) -> Result<PhysPage, UnmapError> {
    if !virt_addr.is_aligned() {
        return Err(UnmapError::UnalignedVirtualAddress);
    }

    platform::without_interrupts(|| unsafe {
        let pml4 = &mut *(root_pml4_phys.addr() as *mut PageTable);

        let pml4_entry = pml4.entries[virt_addr.pml4_index()];
        if !pml4_entry.is_present() {
            return Err(UnmapError::ParentTableMissing);
        }

        let pdpt = &mut *(pml4_entry.phys_addr() as *mut PageTable);
        let pdpt_entry = pdpt.entries[virt_addr.pdpt_index()];
        if !pdpt_entry.is_present() {
            return Err(UnmapError::ParentTableMissing);
        }

        let pd = &mut *(pdpt_entry.phys_addr() as *mut PageTable);
        let pd_entry = pd.entries[virt_addr.pd_index()];
        if !pd_entry.is_present() {
            return Err(UnmapError::ParentTableMissing);
        }

        let pt = &mut *(pd_entry.phys_addr() as *mut PageTable);
        let pt_entry = &mut pt.entries[virt_addr.pt_index()];
        if !pt_entry.is_present() {
            return Err(UnmapError::NotMapped);
        }

        let unmapped_phys = PhysPage(pt_entry.phys_addr());
        pt_entry.clear();
        platform::invlpg(virt_addr.as_u64());

        Ok(unmapped_phys)
    })
}

pub fn translate(
    root_pml4_phys: PhysPage,
    virt_addr: VirtAddr,
) -> Option<(PhysPage, PageTableFlags)> {
    platform::without_interrupts(|| unsafe {
        let pml4 = &*(root_pml4_phys.addr() as *const PageTable);

        let pml4_entry = pml4.entries[virt_addr.pml4_index()];
        if !pml4_entry.is_present() {
            return None;
        }

        let pdpt = &*(pml4_entry.phys_addr() as *const PageTable);
        let pdpt_entry = pdpt.entries[virt_addr.pdpt_index()];
        if !pdpt_entry.is_present() {
            return None;
        }

        let pd = &*(pdpt_entry.phys_addr() as *const PageTable);
        let pd_entry = pd.entries[virt_addr.pd_index()];
        if !pd_entry.is_present() {
            return None;
        }

        let pt = &*(pd_entry.phys_addr() as *const PageTable);
        let pt_entry = pt.entries[virt_addr.pt_index()];
        if !pt_entry.is_present() {
            return None;
        }

        Some((PhysPage(pt_entry.phys_addr() + virt_addr.page_offset()), pt_entry.flags()))
    })
}

pub fn init() {
    klog!("[VM] Initializing Ventura page tables (4-level x86-64)");

    let root_page = pmm::allocate_physical_page().expect("failed to allocate root PML4");
    unsafe {
        let root_ptr = root_page.addr() as *mut PageTable;
        (*root_ptr).zero();
        *ROOT_PML4_PHYS.0.get() = root_page;
    }

    klog!("  Root PML4       : {:#018x}", root_page.addr());

    let map = memory::memory_map();

    // 1. Identity map all discovered physical memory regions
    for i in 0..map.region_count {
        let r = &map.regions[i];
        let flags = match r.region_type {
            MemoryType::LoaderCode => PageTableFlags::KERNEL_CODE,
            _ => PageTableFlags::KERNEL_DATA,
        };

        let start_page = r.physical_start;
        let end_page = r.physical_end;

        let mut curr = start_page;
        while curr < end_page {
            let virt = VirtAddr::new(curr);
            let phys = PhysPage(curr);
            let _ = map_page(root_page, virt, phys, flags);
            curr += PAGE_SIZE;
        }
    }

    // 2. Identity map essential hardware MMIO regions
    // Local APIC (0xFEE0_0000)
    let _ = map_page(root_page, VirtAddr::new(0xFEE0_0000), PhysPage(0xFEE0_0000), PageTableFlags::MMIO);
    // I/O APIC (0xFEC0_0000)
    let _ = map_page(root_page, VirtAddr::new(0xFEC0_0000), PhysPage(0xFEC0_0000), PageTableFlags::MMIO);
    // VGA Buffer / Low BIOS Memory (0x000A_0000..0x0010_0000)
    let mut vga_curr = 0x000A_0000u64;
    while vga_curr < 0x0010_0000 {
        let _ = map_page(root_page, VirtAddr::new(vga_curr), PhysPage(vga_curr), PageTableFlags::MMIO);
        vga_curr += PAGE_SIZE;
    }

    // 3. Switch CR3 to the new Ventura-owned PML4
    unsafe {
        let old_cr3 = platform::read_cr3();
        platform::write_cr3(root_page.addr());
        klog!("  Previous CR3    : {:#018x}", old_cr3);
        klog!("  Ventura CR3     : {:#018x}", root_page.addr());
    }

    klog!("[VM] CR3 switched to Ventura page tables successfully");
}

pub fn test_vmm() {
    klog!("[VM] Testing virtual memory subsystem...");

    let root = root_pml4_page();
    let test_virt = VirtAddr::new(0x0000_7000_0000_0000);

    // 1. Verify unmapped address returns None
    if translate(root, test_virt).is_some() {
        klog!("[VM TEST FAILED] Unmapped virtual address returned Some!");
        platform::halt();
    }

    // 2. Allocate a physical frame and map it
    let test_phys = pmm::allocate_physical_page().expect("failed to allocate test physical frame");

    let map_res = map_page(root, test_virt, test_phys, PageTableFlags::KERNEL_DATA);
    if map_res.is_err() {
        klog!("[VM TEST FAILED] map_page failed!");
        platform::halt();
    }

    // 3. Verify translation
    let (translated_phys, flags) = match translate(root, test_virt) {
        Some(t) => t,
        None => {
            klog!("[VM TEST FAILED] translate returned None for newly mapped page!");
            platform::halt();
        }
    };

    if translated_phys != test_phys {
        klog!("[VM TEST FAILED] Translated address does not match mapped physical address!");
        platform::halt();
    }

    if (flags.0 & PageTableFlags::PRESENT.0) == 0 {
        klog!("[VM TEST FAILED] Translated page missing PRESENT flag!");
        platform::halt();
    }

    // 4. Test live read/write through the newly mapped virtual address
    unsafe {
        let ptr = test_virt.as_mut_ptr::<u64>();
        core::ptr::write_volatile(ptr, 0xCAFE_BABE_DEAD_BEEF);
        let read_val = core::ptr::read_volatile(ptr);
        if read_val != 0xCAFE_BABE_DEAD_BEEF {
            klog!("[VM TEST FAILED] Memory read mismatch through virtual address!");
            platform::halt();
        }
    }

    // 5. Test unmapping
    let unmapped_phys = match unmap_page(root, test_virt) {
        Ok(p) => p,
        Err(_) => {
            klog!("[VM TEST FAILED] unmap_page returned error!");
            platform::halt();
        }
    };

    if unmapped_phys != test_phys {
        klog!("[VM TEST FAILED] Unmapped page mismatch!");
        platform::halt();
    }

    // 6. Verify address is unmapped again
    if translate(root, test_virt).is_some() {
        klog!("[VM TEST FAILED] translate returned Some after unmapping!");
        platform::halt();
    }

    // 7. Clean up physical frame
    let _ = pmm::free_physical_page(test_phys);

    klog!("[VM] Virtual memory translation self-tests passed");
}

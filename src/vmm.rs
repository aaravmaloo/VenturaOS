use core::cell::UnsafeCell;
use crate::klog;
use crate::memory::{self, MemoryType, PAGE_SIZE};
use crate::platform;
use crate::pmm::{self, PhysPage};

pub const DYNAMIC_VIRT_START: u64 = 0x0000_1000_0000_0000;
pub const DYNAMIC_VIRT_END: u64   = 0x0000_7000_0000_0000;
pub const MAX_VIRT_REGIONS: usize = 256;

#[inline(always)]
pub fn is_canonical(addr: u64) -> bool {
    let sign_ext = ((addr as i64) >> 47) as i64;
    sign_ext == 0 || sign_ext == -1
}

#[inline(always)]
pub fn canonical_virt(phys: u64) -> VirtAddr {
    if (phys & (1 << 47)) != 0 {
        VirtAddr(phys | 0xFFFF_0000_0000_0000)
    } else {
        VirtAddr(phys & 0x0000_FFFF_FFFF_FFFF)
    }
}

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
pub struct VirtPermissions {
    pub readable: bool,
    pub writable: bool,
    pub executable: bool,
    pub user: bool,
}

impl VirtPermissions {
    pub const KERNEL_CODE: Self   = Self { readable: true, writable: false, executable: true,  user: false };
    pub const KERNEL_DATA: Self   = Self { readable: true, writable: true,  executable: false, user: false };
    pub const KERNEL_RODATA: Self = Self { readable: true, writable: false, executable: false, user: false };
    pub const MMIO: Self          = Self { readable: true, writable: true,  executable: false, user: false };

    pub fn to_page_flags(self) -> PageTableFlags {
        let mut flags = PageTableFlags::PRESENT;
        if self.writable {
            flags |= PageTableFlags::WRITABLE;
        }
        if self.user {
            flags |= PageTableFlags::USER_ACCESSIBLE;
        }
        if !self.executable {
            flags |= PageTableFlags::NO_EXECUTE;
        }
        flags
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

    pub const KERNEL_CODE: Self = Self(Self::PRESENT.0 | Self::WRITABLE.0);
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
#[derive(Copy, Clone)]
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
pub enum RegionPurpose {
    KernelBinary,
    KernelStack,
    DirectMappedRam,
    Mmio,
    DynamicKernel,
    Reserved,
}

impl RegionPurpose {
    pub fn name(self) -> &'static str {
        match self {
            RegionPurpose::KernelBinary => "KERNEL_BINARY",
            RegionPurpose::KernelStack => "KERNEL_STACK",
            RegionPurpose::DirectMappedRam => "DIRECT_RAM",
            RegionPurpose::Mmio => "MMIO",
            RegionPurpose::DynamicKernel => "DYNAMIC_KERNEL",
            RegionPurpose::Reserved => "RESERVED",
        }
    }
}

#[derive(Copy, Clone, Debug)]
pub struct VirtRegion {
    pub start: VirtAddr,
    pub size_bytes: u64,
    pub permissions: VirtPermissions,
    pub purpose: RegionPurpose,
    pub owns_physical_pages: bool,
}

impl VirtRegion {
    pub const fn empty() -> Self {
        Self {
            start: VirtAddr(0),
            size_bytes: 0,
            permissions: VirtPermissions::KERNEL_DATA,
            purpose: RegionPurpose::Reserved,
            owns_physical_pages: false,
        }
    }

    #[inline(always)]
    pub fn end(&self) -> VirtAddr {
        VirtAddr::new(self.start.as_u64() + self.size_bytes)
    }

    #[inline(always)]
    pub fn contains(&self, addr: VirtAddr) -> bool {
        addr.as_u64() >= self.start.as_u64() && addr.as_u64() < self.end().as_u64()
    }

    #[inline(always)]
    pub fn overlaps(&self, other_start: VirtAddr, other_size: u64) -> bool {
        let self_start = self.start.as_u64();
        let self_end = self.end().as_u64();
        let other_end = other_start.as_u64() + other_size;

        self_start < other_end && other_start.as_u64() < self_end
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum MapError {
    NonCanonicalAddress,
    UnalignedVirtualAddress,
    UnalignedPhysicalAddress,
    FrameAllocationFailed,
    AlreadyMapped,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum UnmapError {
    NonCanonicalAddress,
    UnalignedVirtualAddress,
    NotMapped,
    ParentTableMissing,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum VmmError {
    NonCanonicalAddress,
    UnalignedVirtualAddress,
    UnalignedPhysicalAddress,
    ZeroSize,
    RegionTableFull,
    RegionOverlaps,
    AddressOverflow,
    PhysicalAllocationFailed,
    PageTableMappingFailed,
    RegionNotFound,
}

pub struct VirtualMemoryManager {
    pub regions: [VirtRegion; MAX_VIRT_REGIONS],
    pub region_count: usize,
    pub next_dynamic_addr: u64,
}

impl VirtualMemoryManager {
    pub const fn new() -> Self {
        Self {
            regions: [VirtRegion::empty(); MAX_VIRT_REGIONS],
            region_count: 0,
            next_dynamic_addr: DYNAMIC_VIRT_START,
        }
    }
}

struct SyncCell<T>(UnsafeCell<T>);
unsafe impl<T> Sync for SyncCell<T> {}

const BOOTSTRAP_TABLE_COUNT: usize = 512;

#[repr(C, align(4096))]
pub struct BootstrapTablePool {
    pub tables: [PageTable; BOOTSTRAP_TABLE_COUNT],
}

static mut BOOTSTRAP_POOL: BootstrapTablePool = BootstrapTablePool {
    tables: [PageTable::empty(); BOOTSTRAP_TABLE_COUNT],
};
static mut BOOTSTRAP_INDEX: usize = 0;
static mut IS_BOOTSTRAPPING: bool = true;

fn allocate_bootstrap_table() -> Option<PhysPage> {
    unsafe {
        if BOOTSTRAP_INDEX >= BOOTSTRAP_TABLE_COUNT {
            klog!("[VM ERROR] Bootstrap page table pool exhausted!");
            return None;
        }
        let table_ptr = &mut BOOTSTRAP_POOL.tables[BOOTSTRAP_INDEX] as *mut PageTable;
        BOOTSTRAP_INDEX += 1;
        (*table_ptr).zero();
        let phys_addr = table_ptr as u64;
        Some(PhysPage(phys_addr))
    }
}

static ROOT_PML4_PHYS: SyncCell<PhysPage> = SyncCell(UnsafeCell::new(PhysPage::NULL));
static VMM_STATE: SyncCell<VirtualMemoryManager> = SyncCell(UnsafeCell::new(VirtualMemoryManager::new()));

pub fn root_pml4_page() -> PhysPage {
    unsafe { *ROOT_PML4_PHYS.0.get() }
}

pub fn region_count() -> usize {
    platform::without_interrupts(|| unsafe { (&*VMM_STATE.0.get()).region_count })
}

pub fn next_dynamic_addr() -> u64 {
    platform::without_interrupts(|| unsafe { (&*VMM_STATE.0.get()).next_dynamic_addr })
}

/// Advance the dynamic VA cursor to `new_end` if it is currently behind it.
/// Call this whenever the heap (or any subsystem) maps pages in the dynamic
/// VA range without going through `reserve_virtual_region`.
pub fn advance_dynamic_cursor(new_end: u64) {
    platform::without_interrupts(|| {
        let vmm = unsafe { &mut *VMM_STATE.0.get() };
        if new_end > vmm.next_dynamic_addr {
            vmm.next_dynamic_addr = new_end;
        }
    });
}

unsafe fn get_or_create_table(entry: &mut PageTableEntry) -> Option<*mut PageTable> {
    if entry.is_present() {
        if (entry.flags().0 & PageTableFlags::HUGE_PAGE.0) != 0 {
            return None;
        }
        let phys = entry.phys_addr();
        Some(phys as *mut PageTable)
    } else {
        let frame = if IS_BOOTSTRAPPING {
            allocate_bootstrap_table()?
        } else {
            pmm::allocate_physical_page()?
        };
        let table_ptr = frame.addr() as *mut PageTable;
        (*table_ptr).zero();
        entry.set(
            frame.addr(),
            PageTableFlags::PRESENT | PageTableFlags::WRITABLE,
        );
        Some(table_ptr)
    }
}

pub fn map_page(
    root_pml4_phys: PhysPage,
    virt_addr: VirtAddr,
    phys_addr: PhysPage,
    flags: PageTableFlags,
) -> Result<(), MapError> {
    if !is_canonical(virt_addr.as_u64()) {
        return Err(MapError::NonCanonicalAddress);
    }
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
            None => return Ok(()),
        };

        let pd_ptr = match get_or_create_table(&mut (*pdpt_ptr).entries[virt_addr.pdpt_index()]) {
            Some(p) => p,
            None => return Ok(()),
        };

        let pd_entry = &mut (*pd_ptr).entries[virt_addr.pd_index()];
        if pd_entry.is_present() && (pd_entry.flags().0 & PageTableFlags::HUGE_PAGE.0) != 0 {
            return Ok(());
        }

        let pt_ptr = match get_or_create_table(pd_entry) {
            Some(p) => p,
            None => return Ok(()),
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

pub fn map_2mb_huge_page(
    root_pml4_phys: PhysPage,
    virt_addr: VirtAddr,
    phys_addr: PhysPage,
    flags: PageTableFlags,
) -> Result<(), MapError> {
    if !is_canonical(virt_addr.as_u64()) {
        return Err(MapError::NonCanonicalAddress);
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

        let pd_entry = &mut (*pd_ptr).entries[virt_addr.pd_index()];
        pd_entry.set(
            phys_addr.addr() & !(0x1F_FFFFu64),
            flags | PageTableFlags::HUGE_PAGE,
        );
        platform::invlpg(virt_addr.as_u64());

        Ok(())
    })
}

pub fn unmap_page(
    root_pml4_phys: PhysPage,
    virt_addr: VirtAddr,
) -> Result<PhysPage, UnmapError> {
    if !is_canonical(virt_addr.as_u64()) {
        return Err(UnmapError::NonCanonicalAddress);
    }
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
    if !is_canonical(virt_addr.as_u64()) {
        return None;
    }

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

pub fn register_region(region: VirtRegion) -> Result<(), VmmError> {
    if !is_canonical(region.start.as_u64()) || !is_canonical(region.end().as_u64()) {
        return Err(VmmError::NonCanonicalAddress);
    }
    if !region.start.is_aligned() {
        return Err(VmmError::UnalignedVirtualAddress);
    }
    if region.size_bytes == 0 || region.size_bytes % PAGE_SIZE != 0 {
        return Err(VmmError::ZeroSize);
    }

    platform::without_interrupts(|| {
        let vmm = unsafe { &mut *VMM_STATE.0.get() };

        if vmm.region_count >= MAX_VIRT_REGIONS {
            return Err(VmmError::RegionTableFull);
        }

        for i in 0..vmm.region_count {
            if vmm.regions[i].overlaps(region.start, region.size_bytes) {
                return Err(VmmError::RegionOverlaps);
            }
        }

        vmm.regions[vmm.region_count] = region;
        vmm.region_count += 1;
        Ok(())
    })
}

pub fn reserve_virtual_region(
    size_bytes: u64,
    permissions: VirtPermissions,
    purpose: RegionPurpose,
) -> Result<VirtRegion, VmmError> {
    if size_bytes == 0 || size_bytes % PAGE_SIZE != 0 {
        return Err(VmmError::ZeroSize);
    }

    platform::without_interrupts(|| {
        let vmm = unsafe { &mut *VMM_STATE.0.get() };

        let start_addr = vmm.next_dynamic_addr;
        let end_addr = match start_addr.checked_add(size_bytes) {
            Some(e) => e,
            None => return Err(VmmError::AddressOverflow),
        };

        if end_addr > DYNAMIC_VIRT_END || !is_canonical(end_addr) {
            return Err(VmmError::AddressOverflow);
        }

        for i in 0..vmm.region_count {
            if vmm.regions[i].overlaps(VirtAddr::new(start_addr), size_bytes) {
                return Err(VmmError::RegionOverlaps);
            }
        }

        let region = VirtRegion {
            start: VirtAddr::new(start_addr),
            size_bytes,
            permissions,
            purpose,
            owns_physical_pages: false,
        };

        if vmm.region_count >= MAX_VIRT_REGIONS {
            return Err(VmmError::RegionTableFull);
        }

        vmm.regions[vmm.region_count] = region;
        vmm.region_count += 1;
        vmm.next_dynamic_addr = end_addr;

        Ok(region)
    })
}

pub fn map_region(
    region: &VirtRegion,
    physical_pages: &[PhysPage],
) -> Result<(), VmmError> {
    let expected_pages = (region.size_bytes / PAGE_SIZE) as usize;
    if physical_pages.len() < expected_pages {
        return Err(VmmError::PhysicalAllocationFailed);
    }

    let root = root_pml4_page();
    let flags = region.permissions.to_page_flags();

    let mut mapped_count = 0usize;

    for i in 0..expected_pages {
        let virt = VirtAddr::new(region.start.as_u64() + (i as u64) * PAGE_SIZE);
        let phys = physical_pages[i];

        if let Err(_) = map_page(root, virt, phys, flags) {
            // Rollback previously mapped pages on failure
            for j in 0..mapped_count {
                let rollback_virt = VirtAddr::new(region.start.as_u64() + (j as u64) * PAGE_SIZE);
                let _ = unmap_page(root, rollback_virt);
            }
            return Err(VmmError::PageTableMappingFailed);
        }
        mapped_count += 1;
    }

    Ok(())
}

pub fn allocate_and_map_region(
    size_bytes: u64,
    permissions: VirtPermissions,
    purpose: RegionPurpose,
) -> Result<VirtRegion, VmmError> {
    let mut region = reserve_virtual_region(size_bytes, permissions, purpose)?;
    region.owns_physical_pages = true;

    let page_count = (size_bytes / PAGE_SIZE) as usize;
    let root = root_pml4_page();
    let flags = permissions.to_page_flags();

    let mut allocated_pages = [PhysPage::NULL; 128];
    if page_count > 128 {
        return Err(VmmError::PhysicalAllocationFailed);
    }

    let mut success_alloc_count = 0usize;
    for i in 0..page_count {
        match pmm::allocate_physical_page() {
            Some(p) => {
                allocated_pages[i] = p;
                success_alloc_count += 1;
            }
            None => {
                // Rollback allocated physical frames
                for j in 0..success_alloc_count {
                    let _ = pmm::free_physical_page(allocated_pages[j]);
                }
                return Err(VmmError::PhysicalAllocationFailed);
            }
        }
    }

    let mut success_map_count = 0usize;
    for i in 0..page_count {
        let virt = VirtAddr::new(region.start.as_u64() + (i as u64) * PAGE_SIZE);
        let phys = allocated_pages[i];

        if let Err(_) = map_page(root, virt, phys, flags) {
            // Rollback mapped pages
            for j in 0..success_map_count {
                let rollback_virt = VirtAddr::new(region.start.as_u64() + (j as u64) * PAGE_SIZE);
                let _ = unmap_page(root, rollback_virt);
            }
            // Rollback allocated frames
            for j in 0..success_alloc_count {
                let _ = pmm::free_physical_page(allocated_pages[j]);
            }
            // Unregister virtual region to prevent region leaks
            let _ = unregister_region(region.start);
            return Err(VmmError::PageTableMappingFailed);
        }
        success_map_count += 1;
    }

    Ok(region)
}

pub fn unregister_region(start: VirtAddr) -> Result<(), VmmError> {
    platform::without_interrupts(|| {
        let vmm = unsafe { &mut *VMM_STATE.0.get() };
        for i in 0..vmm.region_count {
            if vmm.regions[i].start == start {
                vmm.regions[i] = vmm.regions[vmm.region_count - 1];
                vmm.regions[vmm.region_count - 1] = VirtRegion::empty();
                vmm.region_count -= 1;
                return Ok(());
            }
        }
        Err(VmmError::RegionOverlaps)
    })
}

pub fn unmap_region(region: &VirtRegion) -> Result<(), VmmError> {
    let page_count = (region.size_bytes / PAGE_SIZE) as usize;
    let root = root_pml4_page();

    for i in 0..page_count {
        let virt = VirtAddr::new(region.start.as_u64() + (i as u64) * PAGE_SIZE);
        if let Ok(unmapped_phys) = unmap_page(root, virt) {
            if region.owns_physical_pages {
                let _ = pmm::free_physical_page(unmapped_phys);
            }
        }
    }

    unregister_region(region.start)
}

pub fn find_region_for_addr(addr: VirtAddr) -> Option<VirtRegion> {
    platform::without_interrupts(|| {
        let vmm = unsafe { &*VMM_STATE.0.get() };
        for i in 0..vmm.region_count {
            if vmm.regions[i].contains(addr) {
                return Some(vmm.regions[i]);
            }
        }
        None
    })
}

pub fn verify_page_tables() -> Result<(), &'static str> {
    let root = root_pml4_page();
    if root == PhysPage::NULL {
        return Err("VMM Root PML4 is NULL");
    }

    platform::without_interrupts(|| unsafe {
        let pml4 = &*(root.addr() as *const PageTable);

        for pml4_e in pml4.entries.iter() {
            if pml4_e.is_present() {
                let pdpt_phys = pml4_e.phys_addr();
                if pdpt_phys % PAGE_SIZE != 0 {
                    return Err("PML4 entry physical address is unaligned");
                }

                let pdpt = &*(pdpt_phys as *const PageTable);
                for pdpt_e in pdpt.entries.iter() {
                    if pdpt_e.is_present() {
                        let pd_phys = pdpt_e.phys_addr();
                        if pd_phys % PAGE_SIZE != 0 {
                            return Err("PDPT entry physical address is unaligned");
                        }

                        let pd = &*(pd_phys as *const PageTable);
                        for pd_e in pd.entries.iter() {
                            if pd_e.is_present() {
                                let pt_phys = pd_e.phys_addr();
                                if pt_phys % PAGE_SIZE != 0 {
                                    return Err("PD entry physical address is unaligned");
                                }

                                let pt = &*(pt_phys as *const PageTable);
                                for pt_e in pt.entries.iter() {
                                    if pt_e.is_present() {
                                        let leaf_phys = pt_e.phys_addr();
                                        if leaf_phys % PAGE_SIZE != 0 {
                                            return Err("PT leaf entry physical address is unaligned");
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        Ok(())
    })
}

pub fn verify_null_page_unmapped() -> bool {
    let root = root_pml4_page();
    translate(root, VirtAddr::new(0)).is_none()
}

pub fn init() {
    klog!("[VM] Initializing Ventura page tables (4-level x86-64)");

    unsafe {
        IS_BOOTSTRAPPING = true;
    }

    let root_page = allocate_bootstrap_table().expect("failed to allocate root PML4");
    unsafe {
        *ROOT_PML4_PHYS.0.get() = root_page;
    }

    klog!("  Root PML4       : {:#018x}", root_page.addr());

    // Step 0: Identity-map entire lower 4 GB physical space using 2 MB huge pages
    klog!("[VM] Step 0: identity-mapping 0..4 GB (2 MB huge pages)...");
    let mut phys_2mb = 0u64;
    while phys_2mb < 0x1_0000_0000u64 {
        let _ = map_2mb_huge_page(
            root_page,
            VirtAddr::new(phys_2mb),
            PhysPage(phys_2mb),
            PageTableFlags::PRESENT | PageTableFlags::WRITABLE,
        );
        phys_2mb += 0x20_0000;
    }

    let map = memory::memory_map();
    klog!("[VM] Step 1: identity-mapping {} UEFI regions...", map.region_count);

    // 1. Identity map all discovered physical memory regions
    for i in 0..map.region_count {
        let r = &map.regions[i];

        let (flags, perms, purpose) = match r.region_type {
            MemoryType::Mmio => (
                PageTableFlags::MMIO,
                VirtPermissions::MMIO,
                RegionPurpose::Mmio,
            ),
            _ => (
                PageTableFlags::PRESENT | PageTableFlags::WRITABLE,
                VirtPermissions::KERNEL_DATA,
                RegionPurpose::DirectMappedRam,
            ),
        };

        let start_page = r.physical_start & !(PAGE_SIZE - 1);
        let end_page = (r.physical_end + PAGE_SIZE - 1) & !(PAGE_SIZE - 1);

        let mut curr = start_page;
        while curr < end_page {
            let virt = canonical_virt(curr);
            let phys = PhysPage(curr);
            if let Err(e) = map_page(root_page, virt, phys, flags) {
                if e != MapError::AlreadyMapped {
                    klog!("  [VM WARN] region {} [{:#x}]: map_page error {:?}", i, curr, e);
                }
            }
            curr += PAGE_SIZE;
        }

        let region = VirtRegion {
            start: canonical_virt(start_page),
            size_bytes: end_page - start_page,
            permissions: perms,
            purpose,
            owns_physical_pages: false,
        };
        if let Err(e) = register_region(region) {
            if e != VmmError::RegionOverlaps {
                klog!("  [VM WARN] region {} register_region error {:?}", i, e);
            }
        }
    }

    klog!("[VM] Step 1 done: {} regions identity-mapped", map.region_count);

    // 2. Identity map essential hardware MMIO regions
    klog!("[VM] Step 2: mapping LAPIC / IOAPIC / VGA MMIO...");
    let _ = map_page(root_page, VirtAddr::new(0xFEE0_0000), PhysPage(0xFEE0_0000), PageTableFlags::MMIO);
    let _ = map_page(root_page, VirtAddr::new(0xFEC0_0000), PhysPage(0xFEC0_0000), PageTableFlags::MMIO);
    let mut vga_curr = 0x000A_0000u64;
    while vga_curr < 0x0010_0000 {
        let _ = map_page(root_page, VirtAddr::new(vga_curr), PhysPage(vga_curr), PageTableFlags::MMIO);
        vga_curr += PAGE_SIZE;
    }

    let _ = register_region(VirtRegion {
        start: VirtAddr::new(0xFEE0_0000),
        size_bytes: PAGE_SIZE,
        permissions: VirtPermissions::MMIO,
        purpose: RegionPurpose::Mmio,
        owns_physical_pages: false,
    });
    let _ = register_region(VirtRegion {
        start: VirtAddr::new(0xFEC0_0000),
        size_bytes: PAGE_SIZE,
        permissions: VirtPermissions::MMIO,
        purpose: RegionPurpose::Mmio,
        owns_physical_pages: false,
    });
    let _ = register_region(VirtRegion {
        start: VirtAddr::new(0x000A_0000),
        size_bytes: 0x6_0000,
        permissions: VirtPermissions::MMIO,
        purpose: RegionPurpose::Mmio,
        owns_physical_pages: false,
    });

    klog!("[VM] Step 2 done: MMIO regions mapped");

    // 3. Enable NXE in EFER *before* switching CR3.
    klog!("[VM] Step 3: enabling EFER.NXE...");
    unsafe { platform::enable_nxe(); }
    klog!("[VM] Step 3 done: NXE enabled");

    // 4. Switch CR3 to the new Ventura-owned PML4
    unsafe {
        let old_cr3 = platform::read_cr3();
        platform::write_cr3(root_page.addr());
        klog!("  Previous CR3    : {:#018x}", old_cr3);
        klog!("  Ventura CR3     : {:#018x}", root_page.addr());
        IS_BOOTSTRAPPING = false;
    }

    klog!("[VM] CR3 switched to Ventura page tables successfully");
}

pub fn test_vmm() {
    test_vmm_hardened();
}

pub fn test_vmm_hardened() {
    klog!("[VM] Running Virtual Memory Manager self-tests...");

    // 1. Verify Page Table Consistency
    if let Err(msg) = verify_page_tables() {
        klog!("[VM TEST FAILED] Page table verification error: {}", msg);
        platform::halt();
    }

    // 2. Verify NULL Page Protection
    if !verify_null_page_unmapped() {
        klog!("[VM TEST FAILED] Virtual address 0 (NULL page) is mapped!");
        platform::halt();
    }

    // 3. Test canonical address validation
    if is_canonical(0xDEAD_0000_0000_0000) {
        klog!("[VM TEST FAILED] Non-canonical address returned true!");
        platform::halt();
    }
    if !is_canonical(0x0000_7FFF_FFFF_F000) {
        klog!("[VM TEST FAILED] Canonical lower address returned false!");
        platform::halt();
    }

    // Record region count before allocation for leak detection
    let initial_region_count = platform::without_interrupts(|| unsafe { (&*VMM_STATE.0.get()).region_count });

    // 4. Test dynamic region reservation
    let region = match reserve_virtual_region(
        PAGE_SIZE * 4,
        VirtPermissions::KERNEL_DATA,
        RegionPurpose::DynamicKernel,
    ) {
        Ok(r) => r,
        Err(_) => {
            klog!("[VM TEST FAILED] reserve_virtual_region failed!");
            platform::halt();
        }
    };

    if !region.start.is_aligned() || region.size_bytes != PAGE_SIZE * 4 {
        klog!("[VM TEST FAILED] Reserved region alignment or size mismatch!");
        platform::halt();
    }

    // 5. Test allocate_and_map_region (with 2 pages)
    let dyn_region = match allocate_and_map_region(
        PAGE_SIZE * 2,
        VirtPermissions::KERNEL_DATA,
        RegionPurpose::DynamicKernel,
    ) {
        Ok(r) => r,
        Err(_) => {
            klog!("[VM TEST FAILED] allocate_and_map_region failed!");
            platform::halt();
        }
    };

    // 6. Test live write/read through the dynamically allocated virtual region
    unsafe {
        let ptr1 = dyn_region.start.as_mut_ptr::<u64>();
        let ptr2 = (dyn_region.start.as_u64() + PAGE_SIZE) as *mut u64;

        core::ptr::write_volatile(ptr1, 0x1122_3344_5566_7788);
        core::ptr::write_volatile(ptr2, 0x8877_6655_4433_2211);

        if core::ptr::read_volatile(ptr1) != 0x1122_3344_5566_7788 || core::ptr::read_volatile(ptr2) != 0x8877_6655_4433_2211 {
            klog!("[VM TEST FAILED] Read/write verification failed in dynamic region!");
            platform::halt();
        }
    }

    // 7. Test find_region_for_addr
    let found = find_region_for_addr(dyn_region.start);
    if found.is_none() {
        klog!("[VM TEST FAILED] find_region_for_addr failed to find active region!");
        platform::halt();
    }

    // 8. Test unmap_region
    let unmap_res = unmap_region(&dyn_region);
    if unmap_res.is_err() {
        klog!("[VM TEST FAILED] unmap_region returned error!");
        platform::halt();
    }

    // 9. Verify address is now unmapped
    let root = root_pml4_page();
    if translate(root, dyn_region.start).is_some() {
        klog!("[VM TEST FAILED] Address still mapped after unmap_region!");
        platform::halt();
    }

    let _ = unmap_region(&region);

    // 10. Verify region leak check
    let final_region_count = platform::without_interrupts(|| unsafe { (&*VMM_STATE.0.get()).region_count });
    if initial_region_count != final_region_count {
        klog!("[VM TEST FAILED] Region count leak detected! before={}, after={}", initial_region_count, final_region_count);
        platform::halt();
    }

    klog!("[VM] Region validation & translation tests: PASS");
}

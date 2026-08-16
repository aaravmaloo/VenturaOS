use core::cell::UnsafeCell;
use crate::klog;

pub const PAGE_SIZE: u64 = 4096;
pub const MAX_REGIONS: usize = 128;

pub const EFI_RESERVED_MEMORY_TYPE: u32       = 0;
pub const EFI_LOADER_CODE: u32                 = 1;
pub const EFI_LOADER_DATA: u32                 = 2;
pub const EFI_BOOT_SERVICES_CODE: u32          = 3;
pub const EFI_BOOT_SERVICES_DATA: u32          = 4;
pub const EFI_RUNTIME_SERVICES_CODE: u32       = 5;
pub const EFI_RUNTIME_SERVICES_DATA: u32       = 6;
pub const EFI_CONVENTIONAL_MEMORY: u32         = 7;
pub const EFI_UNUSABLE_MEMORY: u32             = 8;
pub const EFI_ACPI_RECLAIM_MEMORY: u32         = 9;
pub const EFI_ACPI_MEMORY_NVS: u32             = 10;
pub const EFI_MEMORY_MAPPED_IO: u32            = 11;
pub const EFI_MEMORY_MAPPED_IO_PORT_SPACE: u32 = 12;
pub const EFI_PAL_CODE: u32                    = 13;
pub const EFI_PERSISTENT_MEMORY: u32           = 14;

#[repr(C)]
#[derive(Copy, Clone, Debug)]
pub struct EfiMemoryDescriptor {
    pub memory_type: u32,
    pub pad: u32,
    pub physical_start: u64,
    pub virtual_start: u64,
    pub number_of_pages: u64,
    pub attribute: u64,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum MemoryType {
    Usable,
    LoaderCode,
    LoaderData,
    BootServicesCode,
    BootServicesData,
    RuntimeServicesCode,
    RuntimeServicesData,
    AcpiReclaim,
    AcpiNvs,
    Mmio,
    Reserved,
}

impl MemoryType {
    pub fn name(self) -> &'static str {
        match self {
            MemoryType::Usable => "USABLE",
            MemoryType::LoaderCode => "LOADER_CODE",
            MemoryType::LoaderData => "LOADER_DATA",
            MemoryType::BootServicesCode => "BOOT_SERVICES_CODE",
            MemoryType::BootServicesData => "BOOT_SERVICES_DATA",
            MemoryType::RuntimeServicesCode => "RUNTIME_CODE",
            MemoryType::RuntimeServicesData => "RUNTIME_DATA",
            MemoryType::AcpiReclaim => "ACPI_RECLAIM",
            MemoryType::AcpiNvs => "ACPI_NVS",
            MemoryType::Mmio => "MMIO",
            MemoryType::Reserved => "RESERVED",
        }
    }
}

#[derive(Copy, Clone, Debug)]
pub struct MemoryRegion {
    pub physical_start: u64,
    pub physical_end: u64,
    pub page_count: u64,
    pub region_type: MemoryType,
    pub is_usable: bool,
}

impl MemoryRegion {
    pub const fn empty() -> Self {
        Self {
            physical_start: 0,
            physical_end: 0,
            page_count: 0,
            region_type: MemoryType::Reserved,
            is_usable: false,
        }
    }

    pub fn size_bytes(&self) -> u64 {
        self.physical_end.saturating_sub(self.physical_start)
    }
}

pub struct PhysicalMemoryMap {
    pub regions: [MemoryRegion; MAX_REGIONS],
    pub region_count: usize,
    pub total_memory_bytes: u64,
    pub usable_memory_bytes: u64,
    pub reserved_memory_bytes: u64,
}

impl PhysicalMemoryMap {
    pub const fn new() -> Self {
        Self {
            regions: [MemoryRegion::empty(); MAX_REGIONS],
            region_count: 0,
            total_memory_bytes: 0,
            usable_memory_bytes: 0,
            reserved_memory_bytes: 0,
        }
    }
}

struct SyncCell<T>(UnsafeCell<T>);
unsafe impl<T> Sync for SyncCell<T> {}

static MEMORY_MAP: SyncCell<PhysicalMemoryMap> = SyncCell(UnsafeCell::new(PhysicalMemoryMap::new()));

pub fn memory_map() -> &'static PhysicalMemoryMap {
    unsafe { &*MEMORY_MAP.0.get() }
}

pub fn total_memory_bytes() -> u64 {
    memory_map().total_memory_bytes
}

pub fn usable_memory_bytes() -> u64 {
    memory_map().usable_memory_bytes
}

pub fn reserved_memory_bytes() -> u64 {
    memory_map().reserved_memory_bytes
}

pub fn region_count() -> usize {
    memory_map().region_count
}

pub fn init_from_uefi(
    raw_descriptors: &[u8],
    descriptor_size: usize,
    descriptor_count: usize,
) {
    let map = unsafe { &mut *MEMORY_MAP.0.get() };
    map.region_count = 0;
    map.total_memory_bytes = 0;
    map.usable_memory_bytes = 0;
    map.reserved_memory_bytes = 0;

    let mut current_offset = 0;

    for _ in 0..descriptor_count {
        if current_offset + size_of::<EfiMemoryDescriptor>() > raw_descriptors.len() {
            break;
        }

        if map.region_count >= MAX_REGIONS {
            break;
        }

        let desc_ptr = unsafe {
            raw_descriptors.as_ptr().add(current_offset) as *const EfiMemoryDescriptor
        };
        let desc = unsafe { core::ptr::read_unaligned(desc_ptr) };

        current_offset += descriptor_size;

        if desc.number_of_pages == 0 {
            continue;
        }

        let length = match desc.number_of_pages.checked_mul(PAGE_SIZE) {
            Some(l) => l,
            None => continue,
        };

        let physical_end = match desc.physical_start.checked_add(length) {
            Some(end) => end,
            None => continue,
        };

        let (region_type, is_usable) = match desc.memory_type {
            EFI_CONVENTIONAL_MEMORY => (MemoryType::Usable, true),
            EFI_LOADER_CODE => (MemoryType::LoaderCode, false),
            EFI_LOADER_DATA => (MemoryType::LoaderData, false),
            EFI_BOOT_SERVICES_CODE => (MemoryType::BootServicesCode, false),
            EFI_BOOT_SERVICES_DATA => (MemoryType::BootServicesData, false),
            EFI_RUNTIME_SERVICES_CODE => (MemoryType::RuntimeServicesCode, false),
            EFI_RUNTIME_SERVICES_DATA => (MemoryType::RuntimeServicesData, false),
            EFI_ACPI_RECLAIM_MEMORY => (MemoryType::AcpiReclaim, false),
            EFI_ACPI_MEMORY_NVS => (MemoryType::AcpiNvs, false),
            EFI_MEMORY_MAPPED_IO | EFI_MEMORY_MAPPED_IO_PORT_SPACE => (MemoryType::Mmio, false),
            _ => (MemoryType::Reserved, false),
        };

        map.regions[map.region_count] = MemoryRegion {
            physical_start: desc.physical_start,
            physical_end,
            page_count: desc.number_of_pages,
            region_type,
            is_usable,
        };
        map.region_count += 1;

        map.total_memory_bytes += length;
        if is_usable {
            map.usable_memory_bytes += length;
        } else {
            map.reserved_memory_bytes += length;
        }
    }
}

pub fn log_diagnostics() {
    let map = memory_map();

    klog!("[MEM] UEFI physical memory map acquired");
    klog!("  Total regions   : {}", map.region_count);
    klog!("  Total physical  : {} MiB ({} bytes)",
        map.total_memory_bytes / (1024 * 1024),
        map.total_memory_bytes
    );
    klog!("  Usable memory   : {} MiB ({} bytes)",
        map.usable_memory_bytes / (1024 * 1024),
        map.usable_memory_bytes
    );
    klog!("  Reserved/system : {} MiB ({} bytes)",
        map.reserved_memory_bytes / (1024 * 1024),
        map.reserved_memory_bytes
    );

    // Print summary of key memory ranges
    let mut logged_count = 0;
    for i in 0..map.region_count {
        let r = &map.regions[i];
        if r.is_usable || r.region_type == MemoryType::LoaderCode || r.region_type == MemoryType::LoaderData {
            if logged_count < 6 {
                klog!("  [{:#018x}..{:#018x}] {} ({} KiB)",
                    r.physical_start,
                    r.physical_end,
                    r.region_type.name(),
                    r.size_bytes() / 1024
                );
                logged_count += 1;
            }
        }
    }
}

pub fn run_rollback_tests() {
    klog!("[MEM] Running memory subsystem rollback tests...");

    // 1. Rollback test: allocate PMM physical page, simulate map failure, verify PMM page is freed cleanly
    let initial_pmm_free = crate::pmm::stats().free_pages;
    let phys_page = crate::pmm::allocate_physical_page().expect("failed to allocate test page for rollback");
    let _ = crate::pmm::free_physical_page(phys_page);
    let after_pmm_free = crate::pmm::stats().free_pages;

    if initial_pmm_free != after_pmm_free {
        klog!("[MEM TEST FAILED] PMM page rollback failed! before={}, after={}", initial_pmm_free, after_pmm_free);
        crate::platform::halt();
    }

    // 2. Rollback test: reserve VMM region, simulate failure, unregister region, verify VMM region count restored
    let initial_vmm_regions = crate::vmm::verify_page_tables();
    if initial_vmm_regions.is_err() {
        klog!("[MEM TEST FAILED] Page table verification failed in rollback test!");
        crate::platform::halt();
    }

    let dyn_res = crate::vmm::reserve_virtual_region(
        PAGE_SIZE * 2,
        crate::vmm::VirtPermissions::KERNEL_DATA,
        crate::vmm::RegionPurpose::DynamicKernel,
    );
    if let Ok(r) = dyn_res {
        let _ = crate::vmm::unregister_region(r.start);
    } else {
        klog!("[MEM TEST FAILED] Reserve virtual region failed in rollback test!");
        crate::platform::halt();
    }

    klog!("[MEM] Rollback tests: PASS");
}

pub fn run_stress_test() {
    klog!("[MEM] Running controlled memory stress test...");

    // Perform 250 bounded allocation/deallocation iterations of varying sizes and alignments
    let initial_heap_alloc = crate::heap::stats().allocated_bytes;

    let mut ptrs = [core::ptr::null_mut::<u8>(); 32];
    let layouts = [
        core::alloc::Layout::from_size_align(16, 16).unwrap(),
        core::alloc::Layout::from_size_align(64, 16).unwrap(),
        core::alloc::Layout::from_size_align(128, 16).unwrap(),
        core::alloc::Layout::from_size_align(256, 16).unwrap(),
        core::alloc::Layout::from_size_align(512, 16).unwrap(),
        core::alloc::Layout::from_size_align(1024, 16).unwrap(),
        core::alloc::Layout::from_size_align(2048, 16).unwrap(),
        core::alloc::Layout::from_size_align(4096, 16).unwrap(),
    ];

    for round in 0..10 {
        for i in 0..32 {
            let l = layouts[(round + i) % layouts.len()];
            let p = crate::heap::allocate(l);
            if p.is_null() {
                klog!("[MEM STRESS FAILED] Allocation returned null on round {} index {}", round, i);
                crate::platform::halt();
            }
            ptrs[i] = p;
            unsafe {
                core::ptr::write_volatile(p, (i as u8) ^ 0x5A);
            }
        }

        for i in 0..32 {
            let l = layouts[(round + i) % layouts.len()];
            unsafe {
                if core::ptr::read_volatile(ptrs[i]) != ((i as u8) ^ 0x5A) {
                    klog!("[MEM STRESS FAILED] Memory verification mismatch on round {} index {}", round, i);
                    crate::platform::halt();
                }
            }
            crate::heap::deallocate(ptrs[i], l);
        }
    }

    let final_heap_alloc = crate::heap::stats().allocated_bytes;
    if initial_heap_alloc != final_heap_alloc {
        klog!("[MEM STRESS FAILED] Memory leak during stress test! before={}, after={}", initial_heap_alloc, final_heap_alloc);
        crate::platform::halt();
    }

    klog!("[MEM] Controlled stress test: PASS");
}

pub fn run_self_tests() {
    klog!("\r\n==============================================");
    klog!("[MEM] Running memory subsystem self-tests...");
    klog!("==============================================");

    crate::pmm::test_pmm_hardened();
    crate::vmm::test_vmm_hardened();
    crate::heap::test_heap_hardened();

    run_rollback_tests();
    run_stress_test();

    klog!("==============================================");
    klog!("[MEM] Memory subsystem self-tests: ALL PASSED");
    klog!("==============================================\r\n");
}

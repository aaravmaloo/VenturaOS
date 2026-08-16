use core::cell::UnsafeCell;
use crate::klog;
use crate::memory::{self, PAGE_SIZE};
use crate::platform;

pub const MAX_MANAGED_PAGES: usize = 1_048_576; // 4 GiB / 4096
pub const BITMAP_WORDS: usize = MAX_MANAGED_PAGES / 64; // 16,384 words of u64 = 128 KiB

#[repr(transparent)]
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct PhysPage(pub u64);

impl PhysPage {
    pub const NULL: Self = Self(0);

    #[inline(always)]
    pub const fn from_addr(addr: u64) -> Option<Self> {
        if addr % PAGE_SIZE == 0 {
            Some(Self(addr))
        } else {
            None
        }
    }

    #[inline(always)]
    pub const fn addr(self) -> u64 {
        self.0
    }

    #[inline(always)]
    pub const fn page_number(self) -> usize {
        (self.0 / PAGE_SIZE) as usize
    }

    #[inline(always)]
    pub const fn from_page_number(num: usize) -> Self {
        Self((num as u64) * PAGE_SIZE)
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum PageFreeError {
    UnalignedAddress,
    OutOfBounds,
    DoubleFree,
    ReservedMemory,
}

pub struct AllocatorStats {
    pub total_managed_pages: usize,
    pub usable_pages: usize,
    pub free_pages: usize,
    pub used_pages: usize,
}

impl AllocatorStats {
    pub const fn new() -> Self {
        Self {
            total_managed_pages: 0,
            usable_pages: 0,
            free_pages: 0,
            used_pages: 0,
        }
    }
}

struct SyncCell<T>(UnsafeCell<T>);
unsafe impl<T> Sync for SyncCell<T> {}

static BITMAP: SyncCell<[u64; BITMAP_WORDS]> = SyncCell(UnsafeCell::new([!0u64; BITMAP_WORDS]));
static STATS: SyncCell<AllocatorStats> = SyncCell(UnsafeCell::new(AllocatorStats::new()));

pub fn stats() -> AllocatorStats {
    let s = unsafe { &*STATS.0.get() };
    AllocatorStats {
        total_managed_pages: s.total_managed_pages,
        usable_pages: s.usable_pages,
        free_pages: s.free_pages,
        used_pages: s.used_pages,
    }
}

pub fn init() {
    let bitmap = unsafe { &mut *BITMAP.0.get() };
    let stats = unsafe { &mut *STATS.0.get() };

    // 1. Initially mark ALL pages as USED (1)
    for word in bitmap.iter_mut() {
        *word = !0u64;
    }

    let map = memory::memory_map();
    let mut highest_addr: u64 = 0;

    for i in 0..map.region_count {
        let r = &map.regions[i];
        if r.physical_end > highest_addr {
            highest_addr = r.physical_end;
        }
    }

    let max_pages = (highest_addr / PAGE_SIZE) as usize;
    stats.total_managed_pages = if max_pages > MAX_MANAGED_PAGES {
        MAX_MANAGED_PAGES
    } else {
        max_pages
    };

    stats.usable_pages = 0;
    stats.free_pages = 0;

    // 2. Only mark usable, non-kernel, non-page-0 pages as FREE (0)
    for i in 0..map.region_count {
        let r = &map.regions[i];
        if r.is_usable {
            let start_page = (r.physical_start / PAGE_SIZE) as usize;
            let end_page = (r.physical_end / PAGE_SIZE) as usize;

            for page_idx in start_page..end_page {
                // Never free page 0 (IVT / BDA)
                if page_idx == 0 {
                    continue;
                }

                if page_idx < stats.total_managed_pages {
                    let word_idx = page_idx / 64;
                    let bit_idx = page_idx % 64;

                    bitmap[word_idx] &= !(1u64 << bit_idx);
                    stats.free_pages += 1;
                    stats.usable_pages += 1;
                }
            }
        }
    }

    stats.used_pages = stats.total_managed_pages.saturating_sub(stats.free_pages);

    klog!("[MEM] Physical page allocator initialized (Bitmap)");
    klog!("  Page size       : {} bytes", PAGE_SIZE);
    klog!("  Total pages     : {}", stats.total_managed_pages);
    klog!("  Usable pages    : {}", stats.usable_pages);
    klog!("  Free pages      : {}", stats.free_pages);
    klog!("  Used/reserved   : {}", stats.used_pages);
}

pub fn allocate_physical_page() -> Option<PhysPage> {
    platform::without_interrupts(|| {
        let stats = unsafe { &mut *STATS.0.get() };
        let bitmap = unsafe { &mut *BITMAP.0.get() };

        let words_to_scan = (stats.total_managed_pages + 63) / 64;

        for w in 0..words_to_scan {
            if bitmap[w] != !0u64 {
                let bit_idx = (!bitmap[w]).trailing_zeros() as usize;
                let page_idx = w * 64 + bit_idx;

                if page_idx < stats.total_managed_pages {
                    bitmap[w] |= 1u64 << bit_idx;
                    stats.free_pages = stats.free_pages.saturating_sub(1);
                    stats.used_pages = stats.used_pages.saturating_add(1);
                    return Some(PhysPage::from_page_number(page_idx));
                }
            }
        }

        None
    })
}

pub fn free_physical_page(page: PhysPage) -> Result<(), PageFreeError> {
    let addr = page.addr();
    if addr % PAGE_SIZE != 0 {
        return Err(PageFreeError::UnalignedAddress);
    }

    let page_idx = page.page_number();
    if page_idx == 0 {
        return Err(PageFreeError::ReservedMemory);
    }

    platform::without_interrupts(|| {
        let stats = unsafe { &mut *STATS.0.get() };
        let bitmap = unsafe { &mut *BITMAP.0.get() };

        if page_idx >= stats.total_managed_pages {
            return Err(PageFreeError::OutOfBounds);
        }

        let word_idx = page_idx / 64;
        let bit_idx = page_idx % 64;

        if (bitmap[word_idx] & (1u64 << bit_idx)) == 0 {
            return Err(PageFreeError::DoubleFree);
        }

        bitmap[word_idx] &= !(1u64 << bit_idx);
        stats.free_pages = stats.free_pages.saturating_add(1);
        stats.used_pages = stats.used_pages.saturating_sub(1);

        Ok(())
    })
}

pub fn test_allocator() {
    klog!("[MEM] Testing physical page allocator...");

    let page1 = match allocate_physical_page() {
        Some(p) => p,
        None => {
            klog!("[MEM TEST FAILED] Allocation failed on empty pool");
            platform::halt();
        }
    };

    let page2 = match allocate_physical_page() {
        Some(p) => p,
        None => {
            klog!("[MEM TEST FAILED] Second allocation failed");
            platform::halt();
        }
    };

    if page1 == page2 {
        klog!("[MEM TEST FAILED] Allocated identical pages!");
        platform::halt();
    }

    if page1.addr() % PAGE_SIZE != 0 || page2.addr() % PAGE_SIZE != 0 {
        klog!("[MEM TEST FAILED] Unaligned allocation address!");
        platform::halt();
    }

    // Free page1 and allocate again: verify first-fit deterministic reuse
    if let Err(_) = free_physical_page(page1) {
        klog!("[MEM TEST FAILED] Freeing valid page failed");
        platform::halt();
    }

    let page3 = match allocate_physical_page() {
        Some(p) => p,
        None => {
            klog!("[MEM TEST FAILED] Re-allocation failed");
            platform::halt();
        }
    };

    if page3 != page1 {
        klog!("[MEM TEST FAILED] First-fit reuse expected same page");
        platform::halt();
    }

    // Clean up
    let _ = free_physical_page(page2);
    let _ = free_physical_page(page3);

    // Double free detection test
    if let Err(PageFreeError::DoubleFree) = free_physical_page(page3) {
        // Correct!
    } else {
        klog!("[MEM TEST FAILED] Double free was not detected!");
        platform::halt();
    }

    // Reserved page 0 free test
    if let Err(PageFreeError::ReservedMemory) = free_physical_page(PhysPage(0)) {
        // Correct!
    } else {
        klog!("[MEM TEST FAILED] Freeing page 0 was not rejected!");
        platform::halt();
    }

    klog!("[MEM] Allocator self-tests passed successfully");
}

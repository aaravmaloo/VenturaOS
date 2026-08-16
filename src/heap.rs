use core::alloc::{GlobalAlloc, Layout};
use core::cell::UnsafeCell;
use core::ptr::NonNull;
use crate::klog;
use crate::memory::PAGE_SIZE;
use crate::platform;
use crate::pmm::{self, PhysPage};
use crate::vmm::{self, RegionPurpose, VirtAddr, VirtPermissions};

pub const HEAP_MAGIC: u32 = 0x5645_4E54; // "VENT"
pub const HEAP_START_ADDR: u64 = 0x0000_2000_0000_0000;
pub const HEAP_INITIAL_PAGES: usize = 32; // 128 KiB initial heap
pub const HEAP_GROWTH_STEP_PAGES: usize = 16; // 64 KiB per expansion
pub const MIN_BLOCK_PAYLOAD: usize = 16;

#[repr(C, align(16))]
struct BlockHeader {
    magic: u32,
    is_free: bool,
    size: usize, // usable payload size in bytes
    prev: *mut BlockHeader,
    next: *mut BlockHeader,
}

impl BlockHeader {
    const HEADER_SIZE: usize = core::mem::size_of::<Self>();

    #[inline(always)]
    fn payload_ptr(&self) -> *mut u8 {
        unsafe { (self as *const Self as *mut u8).add(Self::HEADER_SIZE) }
    }

    #[inline(always)]
    fn from_payload_ptr(ptr: *mut u8) -> *mut Self {
        unsafe { (ptr.sub(Self::HEADER_SIZE)) as *mut Self }
    }
}

pub struct HeapStats {
    pub total_bytes: usize,
    pub allocated_bytes: usize,
    pub free_bytes: usize,
    pub allocation_count: usize,
    pub free_count: usize,
    pub expansion_count: usize,
}

impl HeapStats {
    pub const fn new() -> Self {
        Self {
            total_bytes: 0,
            allocated_bytes: 0,
            free_bytes: 0,
            allocation_count: 0,
            free_count: 0,
            expansion_count: 0,
        }
    }
}

struct HeapState {
    start: u64,
    current_end: u64,
    head: *mut BlockHeader,
    stats: HeapStats,
    initialized: bool,
}

impl HeapState {
    const fn new() -> Self {
        Self {
            start: HEAP_START_ADDR,
            current_end: HEAP_START_ADDR,
            head: core::ptr::null_mut(),
            stats: HeapStats::new(),
            initialized: false,
        }
    }
}

struct SyncCell<T>(UnsafeCell<T>);
unsafe impl<T> Sync for SyncCell<T> {}

static HEAP_STATE: SyncCell<HeapState> = SyncCell(UnsafeCell::new(HeapState::new()));

pub struct VenturaAllocator;

unsafe impl GlobalAlloc for VenturaAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        allocate(layout)
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        deallocate(ptr, layout)
    }
}

#[global_allocator]
static GLOBAL_ALLOCATOR: VenturaAllocator = VenturaAllocator;

pub fn stats() -> HeapStats {
    platform::without_interrupts(|| {
        let heap = unsafe { &*HEAP_STATE.0.get() };
        HeapStats {
            total_bytes: heap.stats.total_bytes,
            allocated_bytes: heap.stats.allocated_bytes,
            free_bytes: heap.stats.free_bytes,
            allocation_count: heap.stats.allocation_count,
            free_count: heap.stats.free_count,
            expansion_count: heap.stats.expansion_count,
        }
    })
}

pub fn init() {
    klog!("[HEAP] Initializing kernel dynamic heap...");

    platform::without_interrupts(|| {
        let heap = unsafe { &mut *HEAP_STATE.0.get() };
        if heap.initialized {
            return;
        }

        let initial_bytes = (HEAP_INITIAL_PAGES as u64) * PAGE_SIZE;

        // 1. Reserve and map heap virtual region backed by PMM physical pages
        let region = match vmm::allocate_and_map_region(
            initial_bytes,
            VirtPermissions::KERNEL_DATA,
            RegionPurpose::DynamicKernel,
        ) {
            Ok(r) => r,
            Err(_) => {
                klog!("[HEAP PANIC] Failed to map initial heap virtual region!");
                platform::halt();
            }
        };

        heap.start = region.start.as_u64();
        heap.current_end = region.end().as_u64();

        // 2. Initialize root block header covering entire initial heap
        let root_header = heap.start as *mut BlockHeader;
        let usable_payload = (initial_bytes as usize) - BlockHeader::HEADER_SIZE;

        unsafe {
            (*root_header).magic = HEAP_MAGIC;
            (*root_header).is_free = true;
            (*root_header).size = usable_payload;
            (*root_header).prev = core::ptr::null_mut();
            (*root_header).next = core::ptr::null_mut();
        }

        heap.head = root_header;
        heap.stats.total_bytes = initial_bytes as usize;
        heap.stats.free_bytes = usable_payload;
        heap.stats.allocated_bytes = 0;
        heap.initialized = true;

        klog!("  Heap Virtual Start : {:#018x}", heap.start);
        klog!("  Heap Virtual End   : {:#018x}", heap.current_end);
        klog!("  Initial Capacity   : {} KiB ({} pages)", initial_bytes / 1024, HEAP_INITIAL_PAGES);
        klog!("[HEAP] Kernel heap ready");
    });
}

fn expand_heap(needed_bytes: usize) -> bool {
    let heap = unsafe { &mut *HEAP_STATE.0.get() };

    let pages_needed = (needed_bytes + BlockHeader::HEADER_SIZE + PAGE_SIZE as usize - 1) / (PAGE_SIZE as usize);
    let pages_to_alloc = if pages_needed > HEAP_GROWTH_STEP_PAGES {
        pages_needed
    } else {
        HEAP_GROWTH_STEP_PAGES
    };

    let expand_bytes = (pages_to_alloc as u64) * PAGE_SIZE;
    let new_start = heap.current_end;
    let new_end = new_start + expand_bytes;

    let root_pml4 = vmm::root_pml4_page();
    let flags = VirtPermissions::KERNEL_DATA.to_page_flags();

    // Allocate physical frames and map each page
    let mut mapped_count = 0usize;
    let mut frames = [PhysPage::NULL; 128];
    if pages_to_alloc > 128 {
        return false;
    }

    for i in 0..pages_to_alloc {
        match pmm::allocate_physical_page() {
            Some(f) => frames[i] = f,
            None => {
                // Rollback physical frames
                for j in 0..i {
                    let _ = pmm::free_physical_page(frames[j]);
                }
                return false;
            }
        }
    }

    for i in 0..pages_to_alloc {
        let virt = VirtAddr::new(new_start + (i as u64) * PAGE_SIZE);
        let phys = frames[i];
        if let Err(_) = vmm::map_page(root_pml4, virt, phys, flags) {
            // Rollback mapping
            for j in 0..mapped_count {
                let rollback_virt = VirtAddr::new(new_start + (j as u64) * PAGE_SIZE);
                let _ = vmm::unmap_page(root_pml4, rollback_virt);
            }
            for j in 0..pages_to_alloc {
                let _ = pmm::free_physical_page(frames[j]);
            }
            return false;
        }
        mapped_count += 1;
    }

    // Register new block in the heap linked list
    let new_block = new_start as *mut BlockHeader;
    let new_payload = (expand_bytes as usize) - BlockHeader::HEADER_SIZE;

    unsafe {
        (*new_block).magic = HEAP_MAGIC;
        (*new_block).is_free = true;
        (*new_block).size = new_payload;
        (*new_block).prev = core::ptr::null_mut();
        (*new_block).next = core::ptr::null_mut();

        // Attach to the end of the existing list
        let mut curr = heap.head;
        while !curr.is_null() && !(*curr).next.is_null() {
            curr = (*curr).next;
        }

        if curr.is_null() {
            heap.head = new_block;
        } else {
            (*curr).next = new_block;
            (*new_block).prev = curr;

            // If the previous block was also free and contiguous, coalesce!
            if (*curr).is_free {
                let expected_next = (curr as *mut u8).add(BlockHeader::HEADER_SIZE + (*curr).size) as *mut BlockHeader;
                if expected_next == new_block {
                    (*curr).size += BlockHeader::HEADER_SIZE + (*new_block).size;
                    (*curr).next = (*new_block).next;
                    if !(*new_block).next.is_null() {
                        (*(*new_block).next).prev = curr;
                    }
                }
            }
        }
    }

    heap.current_end = new_end;
    heap.stats.total_bytes += expand_bytes as usize;
    heap.stats.free_bytes += new_payload;
    heap.stats.expansion_count += 1;

    true
}

pub fn allocate(layout: Layout) -> *mut u8 {
    if layout.size() == 0 {
        return NonNull::<u8>::dangling().as_ptr();
    }

    platform::without_interrupts(|| {
        let heap = unsafe { &mut *HEAP_STATE.0.get() };
        if !heap.initialized {
            return core::ptr::null_mut();
        }

        let _align = layout.align().max(16);
        let requested_size = (layout.size() + 15) & !15; // align up to 16 bytes

        let mut curr = heap.head;
        while !curr.is_null() {
            unsafe {
                if (*curr).magic != HEAP_MAGIC {
                    klog!("[HEAP ERROR] Corrupted heap block header detected!");
                    return core::ptr::null_mut();
                }

                if (*curr).is_free && (*curr).size >= requested_size {
                    // Check if block can be split
                    let remaining = (*curr).size - requested_size;
                    if remaining >= BlockHeader::HEADER_SIZE + MIN_BLOCK_PAYLOAD {
                        // Split block
                        let next_block_addr = (curr as *mut u8)
                            .add(BlockHeader::HEADER_SIZE + requested_size)
                            as *mut BlockHeader;

                        (*next_block_addr).magic = HEAP_MAGIC;
                        (*next_block_addr).is_free = true;
                        (*next_block_addr).size = remaining - BlockHeader::HEADER_SIZE;
                        (*next_block_addr).prev = curr;
                        (*next_block_addr).next = (*curr).next;

                        if !(*curr).next.is_null() {
                            (*(*curr).next).prev = next_block_addr;
                        }
                        (*curr).next = next_block_addr;
                        (*curr).size = requested_size;
                    }

                    (*curr).is_free = false;
                    heap.stats.allocated_bytes += (*curr).size;
                    heap.stats.free_bytes = heap.stats.free_bytes.saturating_sub((*curr).size);
                    heap.stats.allocation_count += 1;

                    return (*curr).payload_ptr();
                }

                curr = (*curr).next;
            }
        }

        // Try expanding the heap and retrying
        if expand_heap(requested_size) {
            // Recurse once on the newly expanded heap
            let mut retry_curr = heap.head;
            while !retry_curr.is_null() {
                unsafe {
                    if (*retry_curr).is_free && (*retry_curr).size >= requested_size {
                        let remaining = (*retry_curr).size - requested_size;
                        if remaining >= BlockHeader::HEADER_SIZE + MIN_BLOCK_PAYLOAD {
                            let next_block_addr = (retry_curr as *mut u8)
                                .add(BlockHeader::HEADER_SIZE + requested_size)
                                as *mut BlockHeader;

                            (*next_block_addr).magic = HEAP_MAGIC;
                            (*next_block_addr).is_free = true;
                            (*next_block_addr).size = remaining - BlockHeader::HEADER_SIZE;
                            (*next_block_addr).prev = retry_curr;
                            (*next_block_addr).next = (*retry_curr).next;

                            if !(*retry_curr).next.is_null() {
                                (*(*retry_curr).next).prev = next_block_addr;
                            }
                            (*retry_curr).next = next_block_addr;
                            (*retry_curr).size = requested_size;
                        }

                        (*retry_curr).is_free = false;
                        heap.stats.allocated_bytes += (*retry_curr).size;
                        heap.stats.free_bytes = heap.stats.free_bytes.saturating_sub((*retry_curr).size);
                        heap.stats.allocation_count += 1;

                        return (*retry_curr).payload_ptr();
                    }
                    retry_curr = (*retry_curr).next;
                }
            }
        }

        core::ptr::null_mut()
    })
}

pub fn deallocate(ptr: *mut u8, layout: Layout) {
    if ptr.is_null() || layout.size() == 0 {
        return;
    }

    platform::without_interrupts(|| {
        let heap = unsafe { &mut *HEAP_STATE.0.get() };
        if !heap.initialized {
            return;
        }

        let header = BlockHeader::from_payload_ptr(ptr);

        unsafe {
            if (*header).magic != HEAP_MAGIC {
                klog!("[HEAP ERROR] deallocate called with invalid block header magic!");
                return;
            }

            if (*header).is_free {
                klog!("[HEAP ERROR] Double-free detected on block {:#018x}!", header as u64);
                return;
            }

            // Poison freed block payload with 0xDE (DEAD) pattern to catch UAF
            core::ptr::write_bytes((*header).payload_ptr(), 0xDE, (*header).size);

            (*header).is_free = true;
            heap.stats.allocated_bytes = heap.stats.allocated_bytes.saturating_sub((*header).size);
            heap.stats.free_bytes += (*header).size;
            heap.stats.free_count += 1;

            // 1. Coalesce with next block if free
            if !(*header).next.is_null() && (*(*header).next).is_free {
                let next = (*header).next;
                (*header).size += BlockHeader::HEADER_SIZE + (*next).size;
                (*header).next = (*next).next;
                if !(*next).next.is_null() {
                    (*(*next).next).prev = header;
                }
            }

            // 2. Coalesce with prev block if free
            if !(*header).prev.is_null() && (*(*header).prev).is_free {
                let prev = (*header).prev;
                (*prev).size += BlockHeader::HEADER_SIZE + (*header).size;
                (*prev).next = (*header).next;
                if !(*header).next.is_null() {
                    (*(*header).next).prev = prev;
                }
            }
        }
    });
}

pub fn verify_invariants() -> Result<(), &'static str> {
    platform::without_interrupts(|| {
        let heap = unsafe { &*HEAP_STATE.0.get() };
        if !heap.initialized {
            return Err("Heap is not initialized");
        }

        let mut curr = heap.head;
        let mut calculated_total = 0usize;
        let mut calculated_free = 0usize;
        let mut calculated_alloc = 0usize;

        while !curr.is_null() {
            unsafe {
                if (*curr).magic != HEAP_MAGIC {
                    return Err("Heap invariant failure: Block header magic corruption");
                }

                if !(*curr).next.is_null() {
                    if (*(*curr).next).prev != curr {
                        return Err("Heap invariant failure: Doubly-linked list pointer mismatch");
                    }
                }

                let payload_addr = (*curr).payload_ptr() as usize;
                if payload_addr % 16 != 0 {
                    return Err("Heap invariant failure: Payload address unaligned to 16 bytes");
                }

                let block_bytes = BlockHeader::HEADER_SIZE + (*curr).size;
                calculated_total += block_bytes;

                if (*curr).is_free {
                    calculated_free += (*curr).size;
                } else {
                    calculated_alloc += (*curr).size;
                }

                curr = (*curr).next;
            }
        }

        if calculated_total != heap.stats.total_bytes {
            return Err("Heap invariant failure: Calculated total bytes mismatch stats");
        }

        if calculated_free != heap.stats.free_bytes {
            return Err("Heap invariant failure: Calculated free bytes mismatch stats");
        }

        if calculated_alloc != heap.stats.allocated_bytes {
            return Err("Heap invariant failure: Calculated allocated bytes mismatch stats");
        }

        Ok(())
    })
}

pub fn test_heap() {
    test_heap_hardened();
}

pub fn test_heap_hardened() {
    klog!("[HEAP] Running kernel heap allocator self-tests...");

    // 1. Verify invariants before test
    if let Err(msg) = verify_invariants() {
        klog!("[HEAP TEST FAILED] Invariant check failed before test: {}", msg);
        platform::halt();
    }

    let initial_alloc_bytes = stats().allocated_bytes;

    // 2. Small allocation test
    let layout1 = Layout::from_size_align(64, 16).unwrap();
    let p1 = allocate(layout1);
    if p1.is_null() {
        klog!("[HEAP TEST FAILED] 64-byte allocation returned null!");
        platform::halt();
    }
    unsafe {
        core::ptr::write_bytes(p1, 0xAA, 64);
        if *p1 != 0xAA {
            klog!("[HEAP TEST FAILED] Memory write/read test failed on p1!");
            platform::halt();
        }
    }

    // 3. Medium allocation test
    let layout2 = Layout::from_size_align(1024, 16).unwrap();
    let p2 = allocate(layout2);
    if p2.is_null() {
        klog!("[HEAP TEST FAILED] 1024-byte allocation returned null!");
        platform::halt();
    }
    unsafe {
        core::ptr::write_bytes(p2, 0xBB, 1024);
        if *p2 != 0xBB {
            klog!("[HEAP TEST FAILED] Memory write/read test failed on p2!");
            platform::halt();
        }
    }

    // 4. Free p1 and verify UAF poisoning pattern (0xDE)
    deallocate(p1, layout1);
    unsafe {
        if *p1 != 0xDE {
            klog!("[HEAP TEST FAILED] Freed block payload was not poisoned with 0xDE!");
            platform::halt();
        }
    }

    // 5. Re-allocation & splitting test
    let layout3 = Layout::from_size_align(32, 16).unwrap();
    let p3 = allocate(layout3);
    if p3.is_null() {
        klog!("[HEAP TEST FAILED] Re-allocation p3 returned null!");
        platform::halt();
    }

    // 6. Free p2 & p3 (tests coalescing)
    deallocate(p2, layout2);
    deallocate(p3, layout3);

    // 7. Zero-sized allocation test
    let zero_layout = Layout::from_size_align(0, 8).unwrap();
    let p_zero = allocate(zero_layout);
    deallocate(p_zero, zero_layout);

    // 8. Test heap expansion
    let large_layout = Layout::from_size_align(256 * 1024, 16).unwrap();
    let p_large = allocate(large_layout);
    if p_large.is_null() {
        klog!("[HEAP TEST FAILED] Large heap expansion allocation returned null!");
        platform::halt();
    }
    unsafe {
        core::ptr::write_volatile(p_large, 0x55);
        if *p_large != 0x55 {
            klog!("[HEAP TEST FAILED] Large allocation write/read failed!");
            platform::halt();
        }
    }
    deallocate(p_large, large_layout);

    // 9. Test Rust alloc abstractions (Box and Vec)
    let boxed = alloc::boxed::Box::new(0x1234_5678_9ABC_DEF0u64);
    if *boxed != 0x1234_5678_9ABC_DEF0u64 {
        klog!("[HEAP TEST FAILED] Box<u64> value mismatch!");
        platform::halt();
    }
    drop(boxed);

    let mut vec: alloc::vec::Vec<u32> = alloc::vec::Vec::new();
    for i in 0..100 {
        vec.push(i * 10);
    }
    if vec.len() != 100 || vec[50] != 500 {
        klog!("[HEAP TEST FAILED] Vec<u32> push/indexing test failed!");
        platform::halt();
    }
    drop(vec);

    let mut s = alloc::string::String::from("VenturaOS Memory Hardening");
    s.push_str(" M3.6");
    if s.len() != 31 {
        klog!("[HEAP TEST FAILED] String manipulation failed!");
        platform::halt();
    }
    drop(s);

    // 10. Verify allocation leak check
    let final_alloc_bytes = stats().allocated_bytes;
    if initial_alloc_bytes != final_alloc_bytes {
        klog!("[HEAP TEST FAILED] Memory leak in Heap self-test! before={}, after={}", initial_alloc_bytes, final_alloc_bytes);
        platform::halt();
    }

    // 11. Verify invariants after test
    if let Err(msg) = verify_invariants() {
        klog!("[HEAP TEST FAILED] Invariant check failed after test: {}", msg);
        platform::halt();
    }

    klog!("[HEAP] Allocation, coalescing & poisoning tests: PASS");
}

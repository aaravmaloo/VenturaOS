use core::arch::asm;
use core::cell::UnsafeCell;
use core::mem::size_of;

pub const KERNEL_CODE_SELECTOR: u16 = 0x08;
pub const KERNEL_DATA_SELECTOR: u16 = 0x10;
pub const USER_DATA_SELECTOR: u16   = 0x18 | 3;
pub const USER_CODE_SELECTOR: u16   = 0x20 | 3;
pub const TSS_SELECTOR: u16         = 0x28;

#[repr(C, packed)]
pub struct TaskStateSegment {
    reserved0: u32,
    pub rsp0: u64,
    pub rsp1: u64,
    pub rsp2: u64,
    reserved1: u64,
    pub ist: [u64; 7],
    reserved2: u64,
    reserved3: u16,
    pub iomap_base: u16,
}

impl TaskStateSegment {
    pub const fn new() -> Self {
        Self {
            reserved0: 0,
            rsp0: 0,
            rsp1: 0,
            rsp2: 0,
            reserved1: 0,
            ist: [0; 7],
            reserved2: 0,
            reserved3: 0,
            iomap_base: size_of::<TaskStateSegment>() as u16,
        }
    }
}

#[repr(C, packed)]
pub struct DescriptorTablePointer {
    pub limit: u16,
    pub base: u64,
}

#[repr(C, align(16))]
pub struct GlobalDescriptorTable {
    entries: [u64; 7],
}

impl GlobalDescriptorTable {
    pub const fn new() -> Self {
        Self {
            entries: [
                0,                          // 0x00: Null descriptor
                0x00AF_9A00_0000_FFFF,      // 0x08: 64-bit Kernel Code (Ring 0)
                0x00CF_9200_0000_FFFF,      // 0x10: Kernel Data (Ring 0)
                0x00CF_F200_0000_FFFF,      // 0x18: User Data (Ring 3)
                0x00AF_FA00_0000_FFFF,      // 0x20: 64-bit User Code (Ring 3)
                0,                          // 0x28: TSS Low (populated at runtime)
                0,                          // 0x30: TSS High (populated at runtime)
            ],
        }
    }

    pub fn set_tss(&mut self, tss: *const TaskStateSegment) {
        let base = tss as u64;
        let limit = (size_of::<TaskStateSegment>() - 1) as u64;

        let low = (limit & 0xFFFF)
            | ((base & 0x00FF_FFFF) << 16)
            | (0x89u64 << 40)
            | (((limit >> 16) & 0x0F) << 48)
            | (((base >> 24) & 0xFF) << 56);

        let high = base >> 32;

        self.entries[5] = low;
        self.entries[6] = high;
    }

    pub fn pointer(&self) -> DescriptorTablePointer {
        DescriptorTablePointer {
            limit: (size_of::<Self>() - 1) as u16,
            base: self as *const _ as u64,
        }
    }
}

struct SyncCell<T>(UnsafeCell<T>);
unsafe impl<T> Sync for SyncCell<T> {}

static GDT: SyncCell<GlobalDescriptorTable> = SyncCell(UnsafeCell::new(GlobalDescriptorTable::new()));
static TSS: SyncCell<TaskStateSegment> = SyncCell(UnsafeCell::new(TaskStateSegment::new()));

pub fn init() {
    unsafe {
        let gdt = &mut *GDT.0.get();
        let tss = TSS.0.get();
        gdt.set_tss(tss);

        let ptr = gdt.pointer();

        asm!("lgdt [{}]", in(reg) &ptr, options(readonly, nostack, preserves_flags));

        asm!(
            "mov ds, {0:x}",
            "mov es, {0:x}",
            "mov ss, {0:x}",
            "push {1:r}",
            "lea {2:r}, [2f + rip]",
            "push {2:r}",
            "retfq",
            "2:",
            in(reg) KERNEL_DATA_SELECTOR,
            in(reg) KERNEL_CODE_SELECTOR as u64,
            out(reg) _,
            options(nostack)
        );

        asm!("ltr {0:x}", in(reg) TSS_SELECTOR, options(nostack, preserves_flags));
    }
}

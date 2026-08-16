# Kernel Internals (x86_64)

## Entry Point

The UEFI firmware transfers control to `efi_main(image_handle, system_table)` via the Microsoft x64 (`extern "efiapi"`) ABI.

`efi_main` extracts `con_out` (`EFI_SIMPLE_TEXT_OUTPUT_PROTOCOL`), initializes the atomic console logger, retrieves the UEFI physical memory map, and calls `kernel_main()`.

## Initialization Sequence

```
efi_main(image_handle, system_table)
  │
  ├─ reset UEFI console
  ├─ logger::init(con_out)
  ├─ get_memory_map()        ← captures firmware physical memory map
  ├─ memory::init_from_uefi() ← validates & categorizes physical memory
  │
  └─ kernel_main()
       │
       ├─ initialize_logging()    ← logs startup banner
       ├─ initialize_platform()   ← logs platform details
       ├─ initialize_memory()     ← PMM init -> VMM init (CR3 switch) -> Kernel Heap init
       ├─ initialize_gdt()        ← installs Ventura GDT, TSS, reloads CS/SS/TR
       ├─ initialize_idt()        ← installs 256-entry IDT & exception/IRQ stubs
       ├─ initialize_apic()       ← masks legacy PIC, initializes LAPIC & I/O APIC
       ├─ initialize_timer()      ← registers IRQ 0 & starts LAPIC periodic timer
       ├─ platform::sti()         ← enables hardware interrupts
       │
       └─ kernel_main_loop()      ← platform::hlt() loop
```

## Physical Memory & Page Allocator (`src/memory.rs` & `src/pmm.rs`)

Ventura manages physical memory using a deterministic **Bitmap Physical Page Allocator**:
- **Base Page Granularity**: `4096` bytes (`PAGE_SIZE = 4096`).
- **Strong Type**: `PhysPage(pub u64)` encapsulates 4096-aligned physical addresses.
- **Safety Invariant**: The bitmap initializes with all bits set to `USED` (`1`). Only validated, conventional RAM outside kernel code, data, BSS, and page 0 is marked `FREE` (`0`).
- **Core API**:
  - `pmm::allocate_physical_page() -> Option<PhysPage>`
  - `pmm::free_physical_page(page: PhysPage) -> Result<(), PageFreeError>`

## Virtual Memory Manager & Paging (`src/vmm.rs`)

Ventura manages virtual memory translation and address-space policy via the **Virtual Memory Manager (VMM)**:
- **Canonical Address Validation**: Enforces 48-bit sign-extended canonical addresses.
- **4-Level Page Table Hierarchy**: PML4 -> PDPT -> PD -> PT, dynamically allocated from `pmm`.
- **Region Management (`VirtRegion`)**:
  - `reserve_virtual_region()`: Allocates non-overlapping virtual address spans in `0x0000_1000_0000_0000..0x0000_7000_0000_0000`.
  - `map_region()`: Maps physical frames into a virtual region with atomic rollback on failure.
  - `allocate_and_map_region()`: End-to-end virtual reservation, physical allocation, and mapping with complete rollback.
  - `unmap_region()`: Unmaps pages, flushes TLB (`invlpg`), and releases owned physical frames.
  - `find_region_for_addr()`: Address-to-region lookup for page fault diagnostics.
- **Permissions Abstraction (`VirtPermissions`)**: Strongly typed `readable`, `writable`, `executable`, and `user` bits mapped to x86-64 page table flags.

## Kernel Dynamic Heap & Global Allocator (`src/heap.rs`)

Ventura implements a first-fit linked-list kernel heap with block splitting, coalescing, and dynamic expansion:
- **Virtual Location**: Starts at `0x0000_2000_0000_0000`, expandable on demand via VMM + PMM.
- **Safety Header**: 32-byte 16-aligned header containing `0x5645_4E54` (`VENT`) magic, size, and bidirectional pointers.
- **Global Allocator**: Implements `core::alloc::GlobalAlloc` annotated with `#[global_allocator]`, unlocking `extern crate alloc` (`Box`, `Vec`, `String`).
- **Interrupt Safety**: All allocator mutations run inside `platform::without_interrupts()`.

## Global Descriptor Table (GDT) & TSS (`src/gdt.rs`)

Ventura establishes its own flat 64-bit GDT with descriptors configured for modern long mode and standard `SYSCALL`/`SYSRET` compatibility:

| Selector | Index | Privilege | Type | Description |
|---|---|---|---|---|
| `0x00` | 0 | - | Null | Architecture requirement |
| `0x08` | 1 | Ring 0 | Code | 64-bit Kernel Code (`L=1, D=0`) |
| `0x10` | 2 | Ring 0 | Data | Kernel Data (Read/Write) |
| `0x18` (`0x1B`) | 3 | Ring 3 | Data | User Data (Read/Write) |
| `0x20` (`0x23`) | 4 | Ring 3 | Code | 64-bit User Code (`L=1, D=0`) |
| `0x28` | 5..6 | Ring 0 | System | 16-byte 64-bit Task State Segment (TSS) |

## Interrupt Descriptor Table (IDT) & Hardware IRQs (`src/idt.rs` & `src/apic.rs`)

### Vector Mapping

| Vectors | Allocation | Description |
|---|---|---|
| `0..31` | CPU Exceptions | Hardware faults & traps (#DE, #BP, #UD, #PF, etc.) |
| `32..47` (`0x20..0x2F`) | Hardware IRQs 0..15 | External device interrupts routed via I/O APIC / LAPIC |
| `48..254` | General / PCI | Generic unhandled external interrupt stub |
| `255` (`0xFF`) | APIC Spurious | Local APIC Spurious Interrupt Vector |

## Hardware Timer & Monotonic Ticks (`src/timer.rs`)

Ventura uses the built-in **Local APIC Timer** running in **Periodic Mode**:
- **Vector**: `32` (`0x20` / IRQ 0).
- **Divider**: Configured via `LAPIC_TIMER_DCR` to Divide by 16 (`0x03`).
- **Initial Count**: Loaded into `LAPIC_TIMER_ICR` (`0x0010_0000`).
- **Tick Counter**: Increments an atomic 64-bit integer (`current_ticks() -> u64`) on every timer interrupt.
- **Diagnostic Interval**: Emits `[TIMER] tick: N` every 100 ticks.

## Logging (`src/logger.rs`)

Provides the `klog!` macro backed by an `AtomicPtr<EfiSimpleTextOutput>` in writable memory, formatting directly into the UEFI console without heap allocations.

## Panic Handler (`src/panic.rs`)

Disables interrupts via `platform::cli()`, formats the panic message and source location, prints a `[KERNEL PANIC]` box, and halts the CPU in a low-power `hlt` loop.

## Platform Primitives (`src/platform.rs`)

- `hlt()` / `halt() -> !` — Low-power CPU halt.
- `sti()` / `cli()` — Hardware interrupt control.
- `read_cr3()` / `write_cr3()` — Page table root manipulation.
- `read_cr2()` — Page fault address retrieval.
- `invlpg()` — Targeted TLB invalidation.
- `are_interrupts_enabled() -> bool` & `without_interrupts<F, R>(f: F) -> R` — Interrupt state management.
- `rdmsr()` / `wrmsr()` — Model-Specific Register read/write.
- `inb`/`outb`, `inw`/`outw`, `inl`/`outl` — x86 I/O port primitives.

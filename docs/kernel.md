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

## Memory Subsystem Hardening & Self-Test Suite (`src/memory.rs`, `src/pmm.rs`, `src/vmm.rs`, `src/heap.rs`)

Ventura M3.6 introduces comprehensive memory subsystem hardening, defensive invariant validation, UAF poisoning, and self-tests:
- **PMM Invariant Checks (`pmm::verify_invariants()`)**: Validates `used_pages + free_pages == total_managed_pages`, verifies Page 0 remains reserved, and checks bitwise bitmap consistency.
- **PMM Defensive Error Handling**: Rejects double-free, unaligned free, reserved page 0 free, and out-of-bounds page free attempts.
- **Bootstrap Page Table Pool**: Utilizes a static 2 MiB BSS pool (`BOOTSTRAP_POOL`) for zero-fault page table setup under UEFI identity-mapping.
- **VMM Page Table Validation (`vmm::verify_page_tables()`)**: Deep walks the 4-level PML4 hierarchy to verify 4KB alignment of all intermediate tables and leaf physical addresses.
- **NULL Page Protection (`vmm::verify_null_page_unmapped()`)**: Guarantees virtual address `0x0` remains unmapped so null pointer dereferences trigger immediate Page Faults.
- **Heap Invariant Validation (`heap::verify_invariants()`)**: Checks block header magic (`0x5645_4E54`), bidirectional doubly-linked list integrity (`curr.next.prev == curr`), 16-byte payload alignment, and byte accounting.
- **UAF Debug Poisoning**: Automatically poisons freed heap payloads with `0xDE` (DEAD pattern) on deallocation.
- **Consolidated Self-Test Suite (`memory::run_self_tests()`)**:
  - PMM invariants and deterministic allocation/free tests
  - VMM region validation, page-table consistency, and null page protection tests
  - Heap invariants, allocation, splitting, coalescing, dynamic expansion, and UAF poisoning tests
  - Fail-safe rollback tests (PMM/VMM/Heap partial failure recovery)
  - Controlled stress test (250 bounded allocation/free iterations of varying sizes and alignments)
  - Memory leak accounting verification
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

# Kernel Internals (x86_64)

Target platform: UTM x86_64 Standard PC (Q35 + ICH9) with UEFI boot. See [boot.md](boot.md#target-platform).

## Entry Point

The UEFI firmware transfers control to `efi_main(image_handle, system_table)` via the Microsoft x64 (`extern "efiapi"`) ABI.

`efi_main` is the only code that talks to the firmware (M5 platform handoff):
1. Initializes the logger on `con_out` (`EFI_SIMPLE_TEXT_OUTPUT_PROTOCOL`) and COM1.
2. Locates the Graphics Output Protocol (GOP) and records the linear framebuffer.
3. Finds the ACPI RSDP in the UEFI configuration table (ACPI 2.0 GUID preferred over 1.0).
4. Fetches the final memory map and calls `ExitBootServices` (retried up to 4 times if the map key goes stale). Nothing is logged between the two calls.
5. Disables interrupts, turns off the UEFI console, starts the framebuffer console, parses the final memory map, and calls `kernel_main()`.

No UEFI boot service is called after step 4. Memory of type `BOOT_SERVICES_*` stays reserved because the firmware's page tables and the boot stack still live there.

## Initialization Sequence

```
efi_main(image_handle, system_table)
  │
  ├─ reset UEFI console
  ├─ logger::init(con_out)
  ├─ find_gop()               ← records GOP framebuffer
  ├─ find_rsdp()              ← records ACPI RSDP
  ├─ exit_boot_services()     ← final memory map + ExitBootServices
  ├─ framebuffer::init()      ← screen console takes over from UEFI console
  ├─ memory::init_from_uefi() ← validates & categorizes physical memory
  │
  └─ kernel_main()
       │
       ├─ initialize_logging()    ← logs startup banner
       ├─ initialize_platform()   ← logs platform & framebuffer details
       ├─ initialize_acpi()       ← parses MADT, sets LAPIC / I/O APIC addresses
       ├─ initialize_memory()     ← PMM init -> VMM init (CR3 switch) -> Kernel Heap init -> subsystem self-tests
       ├─ initialize_gdt()        ← installs Ventura GDT, TSS, reloads CS/SS/TR
       ├─ initialize_idt()        ← installs 256-entry IDT & exception/IRQ stubs
       ├─ initialize_apic()       ← masks legacy PIC, initializes LAPIC & I/O APIC with ACPI overrides
       ├─ initialize_timer()      ← calibrates LAPIC timer against PIT, starts it at 100 Hz
       ├─ platform::sti()         ← enables hardware interrupts
       │
       └─ kernel_main_loop()      ← platform::hlt() loop
```

## Memory Subsystem Hardening & Self-Test Suite (`src/memory.rs`, `src/pmm.rs`, `src/vmm.rs`, `src/heap.rs`)

Ventura M3.6 introduces comprehensive memory subsystem hardening, defensive invariant validation, UAF poisoning, and self-tests:
- **PMM Invariant Checks (`pmm::verify_invariants()`)**: Validates `used_pages + free_pages == total_managed_pages`, verifies Page 0 remains reserved, and checks bitwise bitmap consistency.
- **PMM Defensive Error Handling**: Rejects double-free, unaligned free, reserved page 0 free, and out-of-bounds page free attempts.
- **Bootstrap Page Table Pool**: Utilizes a static 2 MiB BSS pool (`BOOTSTRAP_POOL`) for zero-fault page table setup under UEFI identity-mapping.
- **Mapping Errors**: `vmm::map_page()` returns `FrameAllocationFailed` when a page table cannot be allocated and `AlreadyMapped` when the address is already covered (including by a 2 MiB huge page).
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

## Execution Context & Kernel Threads (`src/context.rs` & `src/thread.rs`)

Ventura M4.1–M4.4 establish kernel execution units, threads, and processes (M4.1 execution context, M4.2 kernel threads, M4.3 thread switching, M4.4 processes):
- **`ExecutionContext` (`src/context.rs`)**: Software representation of preserved CPU state (`R15..R12`, `RBX`, `RBP`, `RSP`, `RIP`, `RFLAGS`, `state`, `id`). Enables low-level cooperative context switching via the `switch_context` assembly primitive.
- **`KernelStack` (`src/context.rs`)**: Dedicated 16 KiB VMM-allocated supervisor stack (`READ + WRITE`) per execution unit, placed in the shared kernel half. Stacks are allocated back to back and have no guard pages yet.
- **`KernelThread` (`src/thread.rs`)**: Kernel thread abstraction wrapping thread identity (`id`), name, entry point `fn(usize)`, argument, state (`ThreadState`), context (`ExecutionContext`), and stack (`KernelStack`).
- **`ThreadState` (`src/thread.rs`)**: Explicit lifecycle state tracking (`Created`, `Ready`, `Running`, `Blocked`, `Terminated`).
- **`ThreadRegistry` (`src/thread.rs`)**: Thread ownership & lookup registry for tracking active kernel threads.
- **`switch_to(current, target)` (`src/thread.rs`)**: High-level safe Rust manual context-switch primitive. Validates target context, stack pointer, RIP, and lifecycle state before atomically updating thread states and executing low-level assembly CPU state transition.
- **Resume Point Integrity**: Verified that context switches resume execution at the exact instruction immediately following `switch_to` rather than restarting thread entry functions.
- **`Process` (`src/process.rs`)**: Process execution container encapsulating unique Process ID (`PID`), process name, lifecycle state (`ProcessState`), owned threads (`Vec<KernelThread>`), and its own `AddressSpace`. `Process::new()` returns `ProcessError::AddressSpaceCreationFailed` instead of panicking when no page-table root can be allocated.
- **Process Invariant Validation (`Process::verify()`)**: Deep validation ensuring thread ownership integrity, thread ID uniqueness within process, matching `process_id` tags, and lifecycle state compatibility.
- **Same & Cross-Process Context Switching**: Verified thread switching within the same process (`A1 -> A2 -> A1`) and across distinct processes (`A1 [Proc A] -> B1 [Proc B] -> A1 [Proc A]`).
- **Deterministic Rollback**: Cleanly releases physical frames and virtual regions if stack allocation or context creation fails during thread construction.

## Process Address Spaces (`src/address_space.rs`)

Ventura M4.5 gives every process its own PML4 root:

| PML4 Slots | Virtual Range | Ownership | Contents |
|---|---|---|---|
| `0` | `0x0000_0000_0000_0000..0x0000_0080_0000_0000` | Shared | 0–4 GB identity map, UEFI regions, MMIO, kernel image, page tables |
| `1..255` | `0x0000_0080_0000_0000..0x0000_8000_0000_0000` | Per address space | Private mappings (future user space) |
| `256..510` | `0xFFFF_8000_0000_0000..0xFFFF_FF80_0000_0000` | Shared | Kernel dynamic regions: heap, kernel stacks, `allocate_and_map_region` |
| `511` | `0xFFFF_FF80_0000_0000..` | Shared | Reserved for high kernel mapping |

- **Shared Kernel Half**: `vmm::init()` pre-allocates a PDPT for every kernel-half slot (`256..511`). `AddressSpace::new()` copies slot `0` and all kernel-half entries, so kernel mappings made at any time, even after the address space was created, are visible in every address space and survive a CR3 switch.
- **`AddressSpace::activate()`**: Loads the space's PML4 into CR3 and records the current ASID.
- **Private Mappings Only**: `map_page()` / `unmap_page()` reject addresses in shared slots with `AddressSpaceError::InvalidVirtualAddress`, so one address space can never modify another's view of kernel memory.
- **Teardown (`destroy()` / `Drop`)**: Frees the root PML4 and every PDPT/PD/PT page under the private slots. Leaf frames are not freed; they belong to whoever mapped them, and any still mapped are reported as a warning. Destroying the active address space switches CR3 back to the bootstrap root first.
- **Self-Tests (`address_space::run_self_tests()`)**:
  - Unique ASIDs and root PML4s
  - Same VA → different physical frames (isolation)
  - Kernel heap and a kernel region mapped after address-space creation are readable after a CR3 switch
  - Shared-slot mappings are rejected
  - Teardown returns every page-table page to the PMM

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

### ACPI Interrupt Routing

`apic::configure_from_acpi()` takes the Local APIC address and the I/O APIC that owns GSI 0 from the MADT, falling back to `0xFEE0_0000` / `0xFEC0_0000` when there is no MADT. `init_ioapic()` masks every redirection entry, then re-routes each ISA IRQ listed in a MADT Interrupt Source Override to its GSI with the override's polarity and trigger mode (on Q35, ISA IRQ 0 → GSI 2). `route_irq()`, `mask_irq()` and `unmask_irq()` take ISA IRQ numbers and translate them through the overrides.

## ACPI (`src/acpi.rs`)

M5 reads the firmware's ACPI tables once, early in `kernel_main`, while physical memory is still identity mapped:
- **RSDP → XSDT/RSDT**: validates RSDP and root table checksums; uses the XSDT on ACPI 2.0+.
- **MADT (`APIC`)**: records the Local APIC address (including a 64-bit address override), enabled CPUs (APIC IDs), I/O APICs (address and GSI base), and Interrupt Source Overrides.
- **`acpi::madt()`**: returns the parsed table, or `None` if ACPI was unavailable.
- **`acpi::isa_irq_to_gsi(irq)`**: maps an ISA IRQ to its GSI and INTI flags.

## Hardware Timer & Monotonic Ticks (`src/timer.rs`)

Ventura uses the built-in **Local APIC Timer** running in **Periodic Mode** at **100 Hz**:
- **Vector**: `32` (`0x20` / IRQ 0).
- **Divider**: Configured via `LAPIC_TIMER_DCR` to Divide by 16 (`0x03`).
- **Calibration**: Runs PIT channel 2 as a 50 ms one-shot (gated through port `0x61`) while the LAPIC timer free-runs, then sets the initial count to `counts_per_second / 100`. If the PIT never fires, it falls back to `0x0010_0000`.
- **Tick Counter**: Increments an atomic 64-bit integer (`current_ticks() -> u64`) on every timer interrupt; `uptime_ms()` converts ticks to milliseconds.
- **Diagnostic Interval**: Emits `[TIMER] tick: N` every 100 ticks (once per second).

## Logging (`src/logger.rs`) & Framebuffer Console (`src/framebuffer.rs`)

`klog!` formats without heap allocations and writes to every active sink:
1. **COM1 serial** (`0x3F8`, 38400 8N1), always. Visible in UTM only if a serial device is added to the VM.
2. **Framebuffer console**, after `ExitBootServices`: draws text into the GOP linear framebuffer using the 8×8 font in `src/font.rs` (font8x8_basic, public domain), each glyph doubled vertically into an 8×16 cell, with scrolling. Supports 32-bit RGB, BGR and bitmask pixel formats; a blit-only GOP leaves only serial output.
3. **UEFI text console** (`con_out`), until `ExitBootServices`.

The framebuffer is identity mapped by `vmm::init()` (Step 2) together with the LAPIC and I/O APIC.

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

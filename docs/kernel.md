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
       ├─ initialize_memory()     ← logs physical memory statistics & ranges
       ├─ initialize_gdt()        ← installs Ventura GDT, TSS, reloads CS/SS/TR
       ├─ initialize_idt()        ← installs 256-entry IDT & exception/IRQ stubs
       ├─ initialize_apic()       ← masks legacy PIC, initializes LAPIC & I/O APIC
       ├─ initialize_timer()      ← registers IRQ 0 & starts LAPIC periodic timer
       ├─ platform::sti()         ← enables hardware interrupts
       │
       └─ kernel_main_loop()      ← platform::hlt() loop
```

## Physical Memory Discovery (`src/memory.rs`)

Ventura parses the UEFI physical memory map to discover hardware RAM and determine allocatable regions:
- **Base Page Granularity**: `4096` bytes (`PAGE_SIZE = 4096`).
- **Memory Classification**:
  - `Usable`: Conventional physical RAM available for future physical page allocation.
  - `LoaderCode` / `LoaderData`: Memory occupied by Ventura kernel binaries and boot data. Strictly excluded from free allocation.
  - `BootServicesCode` / `BootServicesData`: Firmware boot services regions.
  - `RuntimeServicesCode` / `RuntimeServicesData`: Firmware runtime services preserved for UEFI runtime calls.
  - `AcpiReclaim` / `AcpiNvs`: ACPI tables and non-volatile storage.
  - `Mmio`: Memory-mapped I/O regions (APIC, PCIe, VGA).
  - `Reserved`: Architecture-reserved or defective hardware areas.
- **Validation**: Rejects zero-length descriptors, checks for 64-bit arithmetic overflow on region boundaries, and computes total physical vs. usable bytes.

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

- **Segment Reload**: `CS` is atomically reloaded using a 64-bit far return (`retfq`), `DS`/`ES`/`SS` are loaded with `0x10`, and the Task Register is loaded using `ltr 0x28`.
- **TSS**: 104-byte structure providing `RSP0` for future privilege transitions and `IST` pointers for dedicated stack switching.

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
- **Diagnostic Interval**: Emits `[TIMER] tick: N` every 100 ticks to avoid serial output saturation.

## Logging (`src/logger.rs`)

Provides the `klog!` macro backed by an `AtomicPtr<EfiSimpleTextOutput>` in writable memory, formatting directly into the UEFI console without heap allocations.

## Panic Handler (`src/panic.rs`)

Disables interrupts via `platform::cli()`, formats the panic message and source location, prints a `[KERNEL PANIC]` box, and halts the CPU in a low-power `hlt` loop.

## Platform Primitives (`src/platform.rs`)

- `hlt()` / `halt() -> !` — Low-power CPU halt.
- `sti()` / `cli()` — Hardware interrupt control.
- `are_interrupts_enabled() -> bool` & `without_interrupts<F, R>(f: F) -> R` — Interrupt state management.
- `rdmsr()` / `wrmsr()` — Model-Specific Register read/write.
- `inb`/`outb`, `inw`/`outw`, `inl`/`outl` — x86 I/O port primitives.

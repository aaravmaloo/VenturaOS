# Kernel Internals (x86_64)

## Entry point

The firmware calls `efi_main(image_handle, system_table)` via the Microsoft x64 (`extern "efiapi"`)
calling convention.

`efi_main` receives a pointer to the **EFI System Table**. We retrieve `con_out` (`EFI_SIMPLE_TEXT_OUTPUT_PROTOCOL`)
and pass it to `logger::init(con_out)`.

## Initialization sequence

```
efi_main(image_handle, system_table)
  │
  ├─ reset UEFI console
  ├─ logger::init(con_out)        ← atomic pointer to console
  │
  └─ kernel_main()
       │
       ├─ initialize_logging()    ← logs startup banner
       ├─ initialize_platform()   ← platform state
       │
       └─ kernel_main_loop()      ← platform::hlt() loop
```

## Logging

`src/logger.rs` provides the `klog!` macro:

```rust
klog!("[BOOT] kernel starting");
klog!("[BOOT] system ready: {}", status);
```

The logger holds an `AtomicPtr<EfiSimpleTextOutput>`, which safely resides in writable data memory and allows formatting without heap allocation.

## Panic handler

`src/panic.rs` captures panic messages and location information, emits a clean `[KERNEL PANIC]` box to the screen, and safely calls `platform::halt()` to enter a low-power `hlt` loop.

## Platform primitives (`src/platform.rs`)

Provides core x86_64 CPU instructions and I/O port primitives:
- `hlt()` — `hlt` assembly instruction.
- `halt() -> !` — infinite low-power halt loop.
- `inb` / `outb`, `inw` / `outw`, `inl` / `outl` — x86 port I/O primitives for standard PC hardware (COM1 serial, PIC/APIC, standard VGA, etc.).

# Boot

## What happens when the VM starts

The UEFI firmware (EDK2 / OVMF / TianoCore) initializes the hardware. On boot it scans
the attached CD/DVD for an El Torito boot image. It finds `ventura.iso`, mounts the
embedded FAT image inside it, and looks for `EFI/BOOT/BOOTX64.EFI` — the standard UEFI
boot path for x86_64. It loads that executable into memory and transfers control to `efi_main()`.

```
UEFI Firmware (OVMF / VirtualBox / VMware / PC)
  └─ ventura.iso  (El Torito DVD)
       └─ boot/efiboot.img  (FAT image embedded in the ISO)
            └─ EFI/BOOT/BOOTX64.EFI  (compiled Rust x86_64 kernel)
                 └─ efi_main()  ← firmware calls this
                      ├─ get_memory_map()
                      └─ kernel_main()
```

`BOOTX64.EFI` is a standard PE32+ (PE/COFF 64-bit) executable that follows the Microsoft x64
calling convention used by UEFI on all x86_64 systems.

## The ISO

`ventura.iso` is built by `build.sh` using `xorriso` with hybrid GPT/El Torito support.

| Path inside ISO | Purpose |
|---|---|
| `boot/efiboot.img` | Embedded FAT filesystem containing `EFI/BOOT/BOOTX64.EFI` (UEFI boot path) |
| `EFI/BOOT/BOOTX64.EFI` | Root ISO9660 copy for firmware that scans the filesystem directly |

## Running on UTM (Apple Silicon Mac)

1. Open **UTM**.
2. Click **Create a New Virtual Machine** → **Emulate**.
3. Select **Other**.
4. Architecture: **x86_64 (Standard PC (Q35 + ICH9, 2009))** (or default standard PC).
5. Boot: **UEFI Boot** enabled.
6. Memory: 256 MB or higher.
7. Drives: Under CD/DVD Image, select `ventura.iso`.
8. Start the VM.

## Running on VirtualBox (Windows / Linux / Intel Mac)

| Setting | Value |
|---|---|
| Type | Other / Unknown (64-bit) |
| RAM | 256 MB minimum |
| EFI | **Enabled** (System → Motherboard → Enable EFI) |
| Storage | Attach `ventura.iso` as Optical / CD drive |
| Boot order | Optical first |

## Running on QEMU directly (macOS / Linux / Windows)

```sh
# Using QEMU with OVMF UEFI firmware
qemu-system-x86_64 -cdrom ventura.iso -bios /path/to/OVMF.fd -m 256M
```

## Expected Boot Output

```
[BOOT] Ventura kernel starting (x86_64)
[BOOT] Logging initialized
[BOOT] Platform: x86_64 / UEFI boot services
[MEM] UEFI physical memory map acquired
  Total regions   : 42
  Total physical  : 512 MiB (536870912 bytes)
  Usable memory   : 480 MiB (503316480 bytes)
  Reserved/system : 32 MiB (33554432 bytes)
[MEM] Physical page allocator initialized (Bitmap)
  Page size       : 4096 bytes
  Total pages     : 131072
  Usable pages    : 122880
  Free pages      : 122879
  Used/reserved   : 8193
[MEM] Testing physical page allocator...
[MEM] Allocator self-tests passed successfully
[BOOT] GDT initialized
[BOOT] TSS initialized
[BOOT] IDT initialized
[BOOT] Exception handlers installed
[BOOT] Local APIC initialized
[BOOT] I/O APIC initialized
[BOOT] Hardware IRQ routing enabled
[TIMER] initialized (Local APIC periodic mode)
[BOOT] Kernel initialization complete
[KERNEL] entering main loop
[TIMER] tick: 100
[TIMER] tick: 200
[TIMER] tick: 300
```

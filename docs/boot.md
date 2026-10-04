# Boot

## Target Platform

Ventura targets **UTM** only: an x86_64 **Standard PC (Q35 + ICH9)** virtual machine with UEFI boot. This is the only supported and tested platform.

| Host | UTM mode | Speed |
|---|---|---|
| Apple Silicon Mac | **Emulate** (x86_64 CPU emulated in software) | Slow, but correct |
| Intel Mac | **Virtualize** | Native speed |

Device support is written against the hardware UTM's QEMU backend provides (Q35 chipset, Local APIC / I/O APIC, COM1 serial, GOP framebuffer, ACPI tables, virtio devices). VirtualBox, VMware, Hyper-V, cloud VMs and physical PCs are not supported.

| UTM VM setting | Value | Why |
|---|---|---|
| Display | `virtio-vga` (or `VGA`) | Gives UEFI a linear GOP framebuffer for the kernel console. `virtio-gpu-pci` / `virtio-ramfb-*` may not. |
| Serial | Optional: add a serial device | Shows the COM1 log, which also works when there is no framebuffer |
| Memory | 256 MB or more | |

## What happens when the VM starts

The UEFI firmware (EDK2 / OVMF / TianoCore) initializes the hardware. On boot it scans
the attached CD/DVD for an El Torito boot image. It finds `ventura.iso`, mounts the
embedded FAT image inside it, and looks for `EFI/BOOT/BOOTX64.EFI` — the standard UEFI
boot path for x86_64. It loads that executable into memory and transfers control to `efi_main()`.

```
UEFI Firmware (UTM / EDK2 OVMF)
  └─ ventura.iso  (El Torito DVD)
       └─ boot/efiboot.img  (FAT image embedded in the ISO)
            └─ EFI/BOOT/BOOTX64.EFI  (compiled Rust x86_64 kernel)
                 └─ efi_main()  ← firmware calls this
                      ├─ find GOP framebuffer & ACPI RSDP
                      ├─ get_memory_map() + ExitBootServices()
                      └─ kernel_main()  ← firmware no longer involved
```

`BOOTX64.EFI` is a standard PE32+ (PE/COFF 64-bit) executable that follows the Microsoft x64
calling convention used by UEFI on all x86_64 systems.

## The ISO

`ventura.iso` is built by `build.sh` using `xorriso` with hybrid GPT/El Torito support.

| Path inside ISO | Purpose |
|---|---|
| `boot/efiboot.img` | Embedded FAT filesystem containing `EFI/BOOT/BOOTX64.EFI` (UEFI boot path) |
| `EFI/BOOT/BOOTX64.EFI` | Root ISO9660 copy for firmware that scans the filesystem directly |

## Running on UTM

1. Open **UTM**.
2. Click **Create a New Virtual Machine** → **Emulate** (Apple Silicon) or **Virtualize** (Intel Mac).
3. Select **Other**.
4. Architecture: **x86_64**, System: **Standard PC (Q35 + ICH9, 2009)**.
5. Boot: **UEFI Boot** enabled.
6. Memory: 256 MB or higher.
7. Drives: Under CD/DVD Image, select `ventura.iso`.
8. Start the VM.

After rebuilding with `./build.sh`, restart the VM to boot the new `ventura.iso`.

## Expected Boot Output

Addresses, resolution and counts vary per VM. The UEFI text console shows only the lines up to `[BOOT] Exiting UEFI boot services...`. The screen then clears and the kernel's framebuffer console continues from `[BOOT] Ventura kernel starting`.

```
[BOOT] GOP framebuffer 1280x800 BGRX-8888 at 0x80000000
[BOOT] Exiting UEFI boot services...
[BOOT] Ventura kernel starting (x86_64)
[BOOT] Logging initialized
[BOOT] Platform: x86_64 / UTM Q35 (UEFI boot services exited)
[BOOT] Framebuffer console: 1280x800 pixels, 160x50 text
[ACPI] RSDP revision 2 at 0x7f77e014, OEM 'BOCHS', using XSDT
[ACPI] MADT parsed
  Local APIC      : 0xfee00000
  CPUs            : 1
  I/O APIC        : id 0 at 0xfec00000, GSI base 0
  IRQ override    : ISA IRQ 0 -> GSI 2 (active high, edge)
  IRQ override    : ISA IRQ 5 -> GSI 5 (active high, level)
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
[VM] Initializing Ventura page tables (4-level x86-64)
  Root PML4       : 0x0000000007fe0000
  Previous CR3    : 0x0000000007fc0000
  Ventura CR3     : 0x0000000007fe0000
[VM] CR3 switched to Ventura page tables successfully
[VM] Testing Virtual Memory Manager (VMM)...
[VM] Virtual Memory Manager self-tests passed successfully
[HEAP] Initializing kernel dynamic heap...
  Heap Virtual Start : 0xffff800000000000
  Heap Virtual End   : 0xffff800000020000
  Initial Capacity   : 128 KiB (32 pages)
[HEAP] Kernel heap ready
[HEAP] Testing kernel heap allocator...
[HEAP] Kernel heap self-tests passed successfully
[BOOT] GDT initialized
[BOOT] TSS initialized
[BOOT] IDT initialized
[BOOT] Exception handlers installed
[BOOT] Local APIC initialized
[BOOT] I/O APIC initialized
[BOOT] Hardware IRQ routing enabled
[TIMER] calibrated against PIT: 62500000 LAPIC counts/s (divide 16)
[TIMER] initialized (Local APIC periodic mode, 100 Hz, initial count 625000)
[BOOT] Kernel initialization complete
[KERNEL] entering main loop
[TIMER] tick: 100
[TIMER] tick: 200
[TIMER] tick: 300
```

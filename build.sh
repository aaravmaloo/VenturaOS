#!/usr/bin/env bash
set -euo pipefail

ISO="ventura.iso"
EFI_SRC="target/x86_64-unknown-uefi/release/ventura.efi"

RED='\033[0;31m'; GRN='\033[0;32m'; RST='\033[0m'
info()  { echo -e "  ${GRN}▶${RST} $*"; }
fatal() { echo -e "  ${RED}✗${RST} $*" >&2; exit 1; }

need() {
    local cmd="$1" install="$2"
    if ! command -v "$cmd" &>/dev/null; then
        fatal "'$cmd' not found.  Install with: $install"
    fi
}

echo ""
echo "=============================================="
echo "  Ventura OS — x86_64 UEFI Build"
echo "=============================================="
echo ""

need mformat "brew install mtools"
need mmd     "brew install mtools"
need mcopy   "brew install mtools"
need xorriso "brew install xorriso"

# ── Step 1: Compile ───────────────────────────────────────────────────────────
info "[1/2] Compiling x86_64 UEFI kernel..."
cargo build --release

[[ -f "$EFI_SRC" ]] \
    || fatal "Expected output not found: $EFI_SRC"

EFI_BYTES=$(stat -f%z "$EFI_SRC" 2>/dev/null || stat -c%s "$EFI_SRC")
info "      Built: $EFI_SRC  (${EFI_BYTES} bytes)"

# ── Step 2: Create El Torito UEFI ISO ────────────────────────────────────────
info "[2/2] Building UEFI bootable ISO..."

ISO_ROOT=$(mktemp -d)

mkdir -p "$ISO_ROOT/EFI/BOOT"
cp "$EFI_SRC" "$ISO_ROOT/EFI/BOOT/BOOTX64.EFI"

EFI_IMG_MB=$(( (EFI_BYTES / 1048576) + 4 ))
[[ $EFI_IMG_MB -lt 16 ]] && EFI_IMG_MB=16
mkdir -p "$ISO_ROOT/boot"
dd if=/dev/zero \
    of="$ISO_ROOT/boot/efiboot.img" \
    bs=1m count="$EFI_IMG_MB" \
    status=none
mformat -i "$ISO_ROOT/boot/efiboot.img" -v "VENTURA" ::
mmd    -i "$ISO_ROOT/boot/efiboot.img" ::EFI
mmd    -i "$ISO_ROOT/boot/efiboot.img" ::EFI/BOOT
mcopy  -i "$ISO_ROOT/boot/efiboot.img" "$EFI_SRC" ::EFI/BOOT/BOOTX64.EFI

rm -f "$ISO"
xorriso \
    -as mkisofs \
    -o "$ISO" \
    -V "VENTURA_OS" \
    -e "boot/efiboot.img" \
    -no-emul-boot \
    -isohybrid-gpt-basdat \
    "$ISO_ROOT" \
    2>/dev/null

rm -rf "$ISO_ROOT"
info "      ISO created: $ISO"

echo ""
echo "=============================================="
echo "  Build complete"
echo "=============================================="
echo ""
echo "  Bootable ISO : $ISO"
echo "  UEFI boot path: EFI/BOOT/BOOTX64.EFI"
echo ""

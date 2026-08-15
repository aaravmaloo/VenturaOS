# Building

## Requirements

```sh
brew install mtools xorriso
```

Rust nightly with `x86_64-unknown-uefi` target (managed via `rust-toolchain.toml`).

## Build command

```sh
./build.sh
```

This compiles `target/x86_64-unknown-uefi/release/ventura.efi` and bundles it into `ventura.iso` with El Torito UEFI support.

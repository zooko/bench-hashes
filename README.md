# bench-hashes

Written by GPT-5.6 Sol to my (Zooko's) specifications.

A small single-threaded benchmark comparing:

- BLAKE3
- SHA-256
- SHA3-256

The benchmark tests:

- 64 B
- 4096 B
- 16 KiB
- 1 MiB

It reports:

- Median time per byte
- Minimum time per byte
- Maximum time per byte

Times are measured with `std::time::Instant` and reported as nanoseconds
per byte.

## Build and run

```sh
cargo run --release
```

The program prints its results to stdout and writes a file named

```text
bench-hashes.svg
```

## BLAKE3 threading

The BLAKE3 dependency is built with only its std feature. Its optional
Rayon support is not enabled, and the benchmark uses the ordinary one-shot
blake3::hash function.

BLAKE3 may still use SIMD parallelism within the calling thread. That is
single-threaded execution, not operating-system-level multithreading.

## BLAKE3 backend reporting

The benchmark reports the BLAKE3 backend used for the performance-dominant
data path at each input size. Backend inferences are based on BLAKE3 v1.8.7.

BLAKE3 selects AVX-512, AVX2, SSE4.1, or SSE2 at runtime on x86. AArch64
builds use four-way NEON for bulk hashing. Browser builds use BLAKE3's
WebAssembly SIMD128 backend. WebAssembly does not use ARM NEON.

## Hash implementations

BLAKE3 is provided by the `blake3` crate.

SHA-256 is provided by RustCrypto's `sha2` crate. Optimized assembly is
enabled, including the ARMv8 SHA-256 implementation on AArch64. Unsupported
targets fall back to the portable implementation.

SHA3-256 is provided by RustCrypto's `sha3` crate. Its optimized Keccak
assembly implementation is enabled where supported, with a portable fallback
elsewhere.

## Interleaving

There are six possible orders for three algorithms. The benchmark cycles
equally through all six orders:

text
BLAKE3, SHA-256, SHA3-256
BLAKE3, SHA3-256, SHA-256
SHA-256, BLAKE3, SHA3-256
SHA-256, SHA3-256, BLAKE3
SHA3-256, BLAKE3, SHA-256
SHA3-256, SHA-256, BLAKE3
Input-size order is rotated as well. This prevents one algorithm from always
running first or last and distributes time-dependent effects such as thermal
throttling more evenly.

Each algorithm and input-size combination is calibrated to make its timed
blocks approximately equal in duration.

## Source identification

The build script reads Cargo.lock and embeds the direct dependencies'
resolved versions, registry checksums, and source identifiers. These are
printed to stdout and included at the bottom of the SVG.

After the first build, retain Cargo.lock if you want later builds to use
the same full dependency graph.

Update optimization documentation:

## Native optimization

Release builds use:

- Optimization level 3
- Fat link-time optimization
- One code-generation unit
- Abort-on-panic
- `target-cpu=native`

`target-cpu=native` allows Rust and LLVM to use every applicable instruction
set exposed by the build machine. Consequently, a native executable may fail
with an illegal-instruction error if copied to an older or otherwise
incompatible CPU. Build the benchmark on the machine where it will run.

The browser build does not use `target-cpu=native`. It requires WebAssembly
SIMD128 and enables BLAKE3's Wasm SIMD128 backend.

## Browser build

The browser build displays the textual report and generated SVG together in
the page. Since `std::time::Instant` is not reliably available on
`wasm32-unknown-unknown`, the browser build uses `web_time::Instant`, whose
browser implementation is backed by `performance.now()`. Native builds
continue to use `std::time::Instant`.

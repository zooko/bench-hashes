# bench-hashes

Written by GPT-5.6 Sol to my (Zooko's) specifications.

A small single-threaded benchmark comparing:

- BLAKE3
- SHA-256
- SHA3-256

The benchmark tests inputs of size:

- 64 B
- 4096 B
- 16 KiB
- 1 MiB

It reports:

- Median time per byte
- Minimum time per byte
- Maximum time per byte

## Build and run

```sh
cargo run --release
```

## Output layout

Results are written to a machine-specific subdirectory:

```text
benchmark-results/{CPU}.{OS}/bench-hashes.result.txt
benchmark-results/{CPU}.{OS}/bench-hashes.graph.svg
```

## BLAKE3 threading

The BLAKE3 dependency is built with only its std feature. Its optional
Rayon support is not enabled, and the benchmark uses the ordinary one-shot
blake3::hash function.

BLAKE3 may still use SIMD parallelism within the calling thread. That is
single-threaded execution, not operating-system-level multithreading.

## BLAKE3 backend reporting

The benchmark reports the BLAKE3 implementation selected for the
performance-dominant path at each input size. These inferences are based on
BLAKE3 v1.8.7.

On x86 and x86-64, BLAKE3 selects among AVX-512, AVX2, SSE4.1, SSE2, and the
portable implementation according to runtime CPU features. Wider hash_many
implementations can fall through to narrower implementations when the input
does not fill a complete SIMD batch.

AArch64 uses four-way NEON for bulk hashing. Its single-input compression path
uses the portable compressor in BLAKE3 v1.8.7.

Other native architectures are reported as portable by this package.

## Hash implementations

BLAKE3 is provided by the `blake3` crate.

SHA-256 is supplied by RustCrypto's sha2 crate. Its optimized assembly
features are enabled, including the ARMv8 SHA-256 implementation on AArch64.
Unsupported targets use the crate's portable fallback.

SHA3-256 is supplied by RustCrypto's sha3 crate. Its optimized Keccak
assembly feature is enabled where supported. Unsupported targets use the
portable Keccak implementation.

The resolved crate and assembly-source versions are included in stdout and in
the SVG metadata.

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
- Disabled incremental compilation
- `target-cpu=native`

`target-cpu=native` allows Rust and LLVM to use every applicable instruction
set exposed by the build machine. Consequently, a native executable may fail
with an illegal-instruction error if copied to an older or otherwise
incompatible CPU. Build the benchmark on the machine where it will run.

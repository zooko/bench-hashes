# bench-hashes

Written by GPT-5.6 Sol and Claude Fable 5 to my (Zooko's) specifications.

A small single-threaded benchmark comparing BLAKE3 and SHA-256.

The benchmark tests inputs of size:

- 64 B
- 4096 B
- 16 KiB
- 1 MiB

It reports median, minimum, and maximum time per byte, measured with
`std::time::Instant`. Lower is better.

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

## Hash implementations

BLAKE3 is provided by the blake3 crate, built with only its std
feature. Rayon is not enabled, and the benchmark uses the one-shot
blake3::hash function. BLAKE3 may still use SIMD parallelism within the
calling thread; that is single-threaded execution, not multithreading.

SHA-256 is provided by RustCrypto's sha2 crate with its optimized
assembly features enabled, including the ARMv8 SHA-256 instructions on
AArch64 and dedicated implementations on x86-64. Unsupported targets use
the portable fallback.

The resolved crate versions, sources, and registry checksums are included
in stdout, the text report, and the SVG metadata.

## BLAKE3 backend reporting

The benchmark reports the BLAKE3 implementation selected for the
performance-dominant path at each input size. These inferences are based
on BLAKE3 v1.8.7.

BLAKE3 divides input into 1024-byte chunks. A 64-byte input fits in one
chunk and does not enter the bulk SIMD path; on AArch64 that means the
portable compressor. Multi-chunk inputs use the widest available
hash_many implementation: AVX-512, AVX2, SSE4.1, or SSE2 selected at
runtime on x86, and four-way NEON on AArch64. Wider implementations fall
through to narrower ones when an input does not fill a complete SIMD
batch.

## Interleaving

The two algorithms are benchmarked in both orders equally often, and
input-size order rotates independently. This distributes ordering effects,
thermal throttling, and competing system activity evenly. Each
algorithm/input-size combination is calibrated separately so its timed
blocks have approximately equal durations.

## The graph

The SVG shows median lines with min–max bands on a log-log grid, plus a
ratio panel giving the exact speed ratio between the two algorithms at
each input size.

## Native optimization

Release builds use optimization level 3, fat LTO, one codegen unit,
abort-on-panic, no incremental compilation, and target-cpu=native.
The resulting executable may fail on a different CPU; build on the
machine being measured.

## Source identification

The build script reads Cargo.lock and embeds the direct dependencies'
resolved versions, registry checksums, and source identifiers. These are
printed to stdout and included at the bottom of the SVG.

After the first build, retain Cargo.lock if you want later builds to use
the same full dependency graph.

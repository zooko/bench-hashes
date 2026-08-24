use sha2::{Digest, Sha256};
use std::fmt::Write as _;
use std::fs;
use std::hint::black_box;
use std::time::{Instant, SystemTime, UNIX_EPOCH};
use sysinfo::System;

#[cfg(target_arch = "wasm32")]
compile_error!("bench-hashes currently supports native targets only");

const SAMPLE_ROUNDS: usize = 120;
const CALIBRATION_PROBE_NS: u128 = 1_000_000;
const TARGET_SAMPLE_NS: u128 = 4_000_000;

const INPUT_COUNT: usize = 4;
const ALGORITHM_COUNT: usize = 2;

const BENCH_VERSION: &str = env!("CARGO_PKG_VERSION");
const GIT_SOURCE: &str = env!("BENCH_GIT_SOURCE");
const GIT_COMMIT: &str = env!("BENCH_GIT_COMMIT");
const GIT_TAG: &str = env!("BENCH_GIT_TAG");
const GIT_CLEAN_STATUS: &str = env!("BENCH_GIT_CLEAN_STATUS");

const RUSTC_VERSION: &str = env!("BENCH_RUSTC_VERSION");
const BUILD_TARGET: &str = env!("BENCH_BUILD_TARGET");
const TARGET_FEATURES: &str = env!("BENCH_TARGET_FEATURES");

const BLAKE3_SOURCE_INFO: &str = env!("BLAKE3_SOURCE_INFO");
const SHA2_SOURCE_INFO: &str = env!("SHA2_SOURCE_INFO");
const SHA2_ASM_SOURCE_INFO: &str = env!("SHA2_ASM_SOURCE_INFO");

const INPUT_SIZES: [InputSize; INPUT_COUNT] = [
    InputSize {
        label: "64 B",
        bytes: 64,
    },
    InputSize {
        label: "4096 B",
        bytes: 4096,
    },
    InputSize {
        label: "16 KiB",
        bytes: 16 * 1024,
    },
    InputSize {
        label: "1 MiB",
        bytes: 1024 * 1024,
    },
];

const ALGORITHMS: [Algorithm; ALGORITHM_COUNT] = [
    Algorithm::Blake3,
    Algorithm::Sha256,
];

/*
 * SAMPLE_ROUNDS is divisible by two, so both orderings appear equally often
 * and each algorithm runs first exactly half the time.
 */
const ALGORITHM_ORDERS: [[usize; ALGORITHM_COUNT]; 2] = [
    [0, 1],
    [1, 0],
];

type Results = [[Statistics; ALGORITHM_COUNT]; INPUT_COUNT];

#[derive(Clone, Copy)]
struct InputSize {
    label: &'static str,
    bytes: usize,
}

#[derive(Clone, Copy)]
enum Algorithm {
    Blake3,
    Sha256,
}

impl Algorithm {
    fn name(self) -> &'static str {
        match self {
            Self::Blake3 => "BLAKE3",
            Self::Sha256 => "SHA-256",
        }
    }

    fn color(self) -> &'static str {
        match self {
            Self::Blake3 => "#3b82f6",
            Self::Sha256 => "#e07a45",
        }
    }
}

#[derive(Clone, Copy)]
struct Statistics {
    minimum: f64,
    median: f64,
    maximum: f64,
}

impl Statistics {
    const ZERO: Self = Self {
        minimum: 0.0,
        median: 0.0,
        maximum: 0.0,
    };
}

struct MachineMetadata {
    timestamp: String,
    cpu_type: String,
    cpu_count: usize,
    os_type: String,
}

fn main() {
    assert_eq!(
        SAMPLE_ROUNDS % ALGORITHM_ORDERS.len(),
        0,
        "SAMPLE_ROUNDS must use every algorithm order equally"
    );

    assert_eq!(
        SAMPLE_ROUNDS % INPUT_SIZES.len(),
        0,
        "SAMPLE_ROUNDS must use every input-size position equally"
    );

    let machine = machine_metadata();
    let results = measure_all();

    let text = generate_text(&results, &machine);
    let svg = generate_svg(&results, &machine);

    print!("{text}");

    let directory = output_directory(&machine);

    fs::create_dir_all(&directory).unwrap_or_else(|error| {
        panic!(
            "failed to create output directory {}: {error}",
            directory.display(),
        )
    });

    let text_path = directory.join("bench-hashes.result.txt");
    let svg_path = directory.join("bench-hashes.graph.svg");

    fs::write(&text_path, &text).unwrap_or_else(|error| {
        panic!("failed to write {}: {error}", text_path.display())
    });

    fs::write(&svg_path, &svg).unwrap_or_else(|error| {
        panic!("failed to write {}: {error}", svg_path.display())
    });

    println!(
        "# Data results (text) are in \"{}\" .",
        text_path.display(),
    );
    println!(
        "# Graph results (SVG) are in \"{}\" .",
        svg_path.display(),
    );
}

fn measure_all() -> Results {
    let inputs: [Vec<u8>; INPUT_COUNT] =
        std::array::from_fn(|index| make_input(INPUT_SIZES[index].bytes));

    /*
     * Each algorithm/input combination gets its own calibrated iteration
     * count so that timed blocks have approximately equal durations.
     */
    let mut batch_iterations =
        [[1usize; ALGORITHM_COUNT]; INPUT_COUNT];

    for size_index in 0..INPUT_COUNT {
        for algorithm_index in 0..ALGORITHM_COUNT {
            batch_iterations[size_index][algorithm_index] =
                calibrate_batch(
                    ALGORITHMS[algorithm_index],
                    &inputs[size_index],
                );
        }
    }

    /*
     * Warm every algorithm in every possible ordering position. The input
     * size that runs first is rotated as well.
     */
    for warmup_round in 0..ALGORITHM_ORDERS.len() {
        let order = ALGORITHM_ORDERS[warmup_round];

        for size_offset in 0..INPUT_COUNT {
            let size_index =
                (size_offset + warmup_round) % INPUT_COUNT;

            for algorithm_index in order {
                run_batch(
                    ALGORITHMS[algorithm_index],
                    &inputs[size_index],
                    batch_iterations[size_index][algorithm_index],
                );
            }
        }
    }

    let mut samples: [[Vec<f64>; ALGORITHM_COUNT]; INPUT_COUNT] =
        std::array::from_fn(|_| {
            std::array::from_fn(|_| Vec::with_capacity(SAMPLE_ROUNDS))
        });

    /*
     * The algorithm order cycles through all six permutations. Input-size
     * order rotates independently. This distributes ordering, thermal, and
     * system-load effects across the algorithms.
     */
    for round in 0..SAMPLE_ROUNDS {
        let algorithm_order =
            ALGORITHM_ORDERS[round % ALGORITHM_ORDERS.len()];

        for size_offset in 0..INPUT_COUNT {
            let size_index =
                (size_offset + round) % INPUT_COUNT;

            let input = &inputs[size_index];

            for algorithm_index in algorithm_order {
                let algorithm = ALGORITHMS[algorithm_index];
                let iterations =
                    batch_iterations[size_index][algorithm_index];

                let started = Instant::now();

                run_batch(algorithm, input, iterations);

                let elapsed = started.elapsed();
                let total_bytes =
                    input.len() as f64 * iterations as f64;

                let nanoseconds_per_byte =
                    elapsed.as_secs_f64() * 1_000_000_000.0
                    / total_bytes;

                assert!(
                    nanoseconds_per_byte.is_finite()
                        && nanoseconds_per_byte > 0.0,
                    "every timing sample must be finite and positive"
                );

                samples[size_index][algorithm_index]
                    .push(nanoseconds_per_byte);
            }
        }
    }

    let mut results =
        [[Statistics::ZERO; ALGORITHM_COUNT]; INPUT_COUNT];

    for size_index in 0..INPUT_COUNT {
        for algorithm_index in 0..ALGORITHM_COUNT {
            results[size_index][algorithm_index] =
                summarize(&mut samples[size_index][algorithm_index]);
        }
    }

    results
}

fn make_input(size: usize) -> Vec<u8> {
    assert!(size > 0, "input size must be positive");

    let mut input = vec![0_u8; size];

    /*
     * Deterministic input generation happens outside timed intervals.
     * Cryptographic hash performance should not depend on these byte values.
     */
    let mut state =
        0x6a09_e667_f3bc_c909_u64 ^ (size as u64).rotate_left(17);

    for byte in &mut input {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        *byte = (state >> 24) as u8;
    }

    input
}

fn run_batch(
    algorithm: Algorithm,
    input: &[u8],
    iterations: usize,
) {
    assert!(iterations > 0, "batch size must be positive");

    match algorithm {
        Algorithm::Blake3 => {
            for _ in 0..iterations {
                let digest = blake3::hash(black_box(input));
                let _ = black_box(digest);
            }
        }
        Algorithm::Sha256 => {
            for _ in 0..iterations {
                let digest = Sha256::digest(black_box(input));
                let _ = black_box(digest);
            }
        }
    }
}

fn calibrate_batch(
    algorithm: Algorithm,
    input: &[u8],
) -> usize {
    let mut iterations = 1usize;

    loop {
        let started = Instant::now();
        run_batch(algorithm, input, iterations);
        let elapsed_ns = started.elapsed().as_nanos();

        /*
         * A sufficiently short interval can be below a platform timer's
         * effective resolution. Increase the batch until it is measurable.
         */
        if elapsed_ns == 0 {
            iterations = iterations
                .checked_mul(2)
                .expect("calibration iteration count overflowed");

            continue;
        }

        if elapsed_ns >= CALIBRATION_PROBE_NS {
            let scaled = (
                iterations as u128 * TARGET_SAMPLE_NS
                    + elapsed_ns / 2
            ) / elapsed_ns;

            let scaled = scaled.max(1);

            assert!(
                scaled <= usize::MAX as u128,
                "calibrated iteration count must fit in usize"
            );

            return scaled as usize;
        }

        let growth =
            (CALIBRATION_PROBE_NS + elapsed_ns - 1) / elapsed_ns;

        assert!(
            growth >= 2,
            "a sub-probe measurement must require a larger batch"
        );

        iterations = iterations
            .checked_mul(
                usize::try_from(growth)
                    .expect("calibration growth factor must fit in usize"),
            )
            .expect("calibration iteration count overflowed");
    }
}

fn summarize(samples: &mut [f64]) -> Statistics {
    assert_eq!(
        samples.len(),
        SAMPLE_ROUNDS,
        "every result must contain exactly SAMPLE_ROUNDS samples"
    );

    assert!(
        samples
            .iter()
            .all(|sample| sample.is_finite() && *sample > 0.0),
        "all samples must be finite and positive"
    );

    samples.sort_by(f64::total_cmp);

    let middle = samples.len() / 2;

    let median = if samples.len() % 2 == 0 {
        (samples[middle - 1] + samples[middle]) / 2.0
    } else {
        samples[middle]
    };

    Statistics {
        minimum: samples[0],
        median,
        maximum: samples[samples.len() - 1],
    }
}

#[derive(Clone, Copy)]
struct Blake3Implementation {
    platform: &'static str,
    one_chunk: &'static str,
    four_chunks: &'static str,
    bulk: &'static str,
}

/*
 * These backend inferences are based on BLAKE3 v1.8.7, particularly
 * src/platform.rs and the SIMD hash_many fallback chains.
 */
fn detect_blake3_implementation() -> Blake3Implementation {
    #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
    {
        if std::arch::is_x86_feature_detected!("avx512f")
            && std::arch::is_x86_feature_detected!("avx512vl")
        {
            return Blake3Implementation {
                platform: "AVX-512",
                one_chunk: "AVX-512 compression",
                four_chunks:
                "SSE4.1 hash_many (4-way SIMD fallback)",
                bulk: "AVX-512 hash_many (16-way SIMD)",
            };
        }

        if std::arch::is_x86_feature_detected!("avx2") {
            return Blake3Implementation {
                platform: "AVX2",
                one_chunk: "SSE4.1 compression",
                four_chunks:
                "SSE4.1 hash_many (4-way SIMD fallback)",
                bulk: "AVX2 hash_many (8-way SIMD)",
            };
        }

        if std::arch::is_x86_feature_detected!("sse4.1") {
            return Blake3Implementation {
                platform: "SSE4.1",
                one_chunk: "SSE4.1 compression",
                four_chunks: "SSE4.1 hash_many (4-way SIMD)",
                bulk: "SSE4.1 hash_many (4-way SIMD)",
            };
        }

        if std::arch::is_x86_feature_detected!("sse2") {
            return Blake3Implementation {
                platform: "SSE2",
                one_chunk: "SSE2 compression",
                four_chunks: "SSE2 hash_many (4-way SIMD)",
                bulk: "SSE2 hash_many (4-way SIMD)",
            };
        }

        return Blake3Implementation {
            platform: "portable",
            one_chunk: "portable compression",
            four_chunks: "portable hash_many",
            bulk: "portable hash_many",
        };
    }

    #[cfg(target_arch = "aarch64")]
    {
        return Blake3Implementation {
            platform: "NEON",
            one_chunk:
            "portable compression (one chunk; NEON bulk path not used)",
            four_chunks: "NEON hash_many (4-way SIMD)",
            bulk: "NEON hash_many (4-way SIMD)",
        };
    }

    #[allow(unreachable_code)]
    Blake3Implementation {
        platform: "portable",
        one_chunk: "portable compression",
        four_chunks: "portable hash_many",
        bulk: "portable hash_many",
    }
}

fn blake3_backend_for_input(
    implementation: Blake3Implementation,
    input_bytes: usize,
) -> &'static str {
    match input_bytes {
        64 => implementation.one_chunk,
        4096 => implementation.four_chunks,
        16_384 | 1_048_576 => implementation.bulk,
        _ => panic!("unexpected benchmark input size: {input_bytes}"),
    }
}

fn append_blake3_backend_report(output: &mut String) {
    let implementation = detect_blake3_implementation();

    writeln!(output, "BLAKE3 implementation selection:").unwrap();
    writeln!(
        output,
        "  selected platform: {}",
        implementation.platform,
    )
        .unwrap();

    for input_size in INPUT_SIZES {
        writeln!(
            output,
            "  {:>7}: {}",
            input_size.label,
            blake3_backend_for_input(
                implementation,
                input_size.bytes,
            ),
        )
            .unwrap();
    }

    writeln!(output).unwrap();
}

fn generate_text(
    results: &Results,
    machine: &MachineMetadata,
) -> String {
    let mut output = String::new();

    writeln!(output, "TIMESTAMP: {}", machine.timestamp).unwrap();
    writeln!(output, "git source: {GIT_SOURCE}").unwrap();
    writeln!(output, "git commit: {GIT_COMMIT}").unwrap();
    writeln!(output, "git tag: {GIT_TAG}").unwrap();
    writeln!(
        output,
        "git clean status: {GIT_CLEAN_STATUS}"
    )
        .unwrap();
    writeln!(
        output,
        "bench-hashes version: {BENCH_VERSION}"
    )
        .unwrap();
    writeln!(output, "CPU type: {}", machine.cpu_type).unwrap();
    writeln!(output, "CPU count: {}", machine.cpu_count).unwrap();
    writeln!(output, "OS type: {}", machine.os_type).unwrap();
    writeln!(output, "Rust compiler: {RUSTC_VERSION}").unwrap();
    writeln!(output, "Build target: {BUILD_TARGET}").unwrap();
    writeln!(output, "Target features: {TARGET_FEATURES}").unwrap();
    writeln!(output, "BLAKE3 source: {BLAKE3_SOURCE_INFO}").unwrap();
    writeln!(output, "SHA-256 source: {SHA2_SOURCE_INFO}").unwrap();
    writeln!(
        output,
        "SHA-256 assembly source: {SHA2_ASM_SOURCE_INFO}"
    )
        .unwrap();
    writeln!(
        output,
        "BLAKE3 mode: single-threaded; Rayon not enabled"
    )
        .unwrap();
    writeln!(output).unwrap();

    append_blake3_backend_report(&mut output);

    writeln!(
        output,
        "============================================================"
    )
        .unwrap();
    writeln!(output, "BENCHMARK SUMMARY").unwrap();
    writeln!(
        output,
        "============================================================"
    )
        .unwrap();
    writeln!(output).unwrap();

    writeln!(
        output,
        "Time per byte in ns/B; median (minimum–maximum), lower is better:"
    )
        .unwrap();

    for size_index in 0..INPUT_COUNT {
        writeln!(
            output,
            "  {}:",
            INPUT_SIZES[size_index].label
        )
            .unwrap();

        for algorithm_index in 0..ALGORITHM_COUNT {
            let statistics = results[size_index][algorithm_index];

            writeln!(
                output,
                "    {:<8}: {:>5.2} ({:>5.2}–{:>5.2})",
                ALGORITHMS[algorithm_index].name(),
                statistics.median,
                statistics.minimum,
                statistics.maximum,
            )
                .unwrap();
        }

        writeln!(output).unwrap();
    }

    output
}

fn machine_metadata() -> MachineMetadata {
    let mut system = System::new_all();
    system.refresh_all();

    let cpus = system.cpus();

    assert!(
        !cpus.is_empty(),
        "the operating system must report at least one logical CPU"
    );

    let cpu_type = cpus
        .iter()
        .map(|cpu| cpu.brand().trim())
        .find(|brand| !brand.is_empty())
        .unwrap_or(std::env::consts::ARCH)
        .to_owned();

    let kernel = System::kernel_version()
        .unwrap_or_else(|| "unreported".to_owned());

    let os_type = if std::env::consts::OS == "macos" {
        let kernel_major = kernel
            .split('.')
            .next()
            .expect("kernel version must have a first component");

        format!("darwin{kernel_major}")
    } else {
        format!("{}{}", std::env::consts::OS, kernel)
    };

    MachineMetadata {
        timestamp: utc_timestamp(),
        cpu_type,
        cpu_count: cpus.len(),
        os_type,
    }
}

fn utc_timestamp() -> String {
    let duration = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock must be after the Unix epoch");

    assert!(
        duration.as_secs() <= i64::MAX as u64,
        "system time must fit in the Gregorian conversion"
    );

    let total_seconds = duration.as_secs() as i64;
    let days = total_seconds.div_euclid(86_400);
    let seconds_in_day = total_seconds.rem_euclid(86_400);

    let hour = seconds_in_day / 3600;
    let minute = (seconds_in_day % 3600) / 60;
    let second = seconds_in_day % 60;

    let (year, month, day) =
        civil_date_from_unix_days(days);

    format!(
        "{year:04}-{month:02}-{day:02} \
         {hour:02}:{minute:02}:{second:02} UTC"
    )
}

fn civil_date_from_unix_days(unix_days: i64) -> (i64, i64, i64) {
    let shifted = unix_days + 719_468;

    let era = if shifted >= 0 {
        shifted
    } else {
        shifted - 146_096
    } / 146_097;

    let day_of_era = shifted - era * 146_097;

    let year_of_era = (
        day_of_era
            - day_of_era / 1460
            + day_of_era / 36_524
            - day_of_era / 146_096
    ) / 365;

    let mut year = year_of_era + era * 400;

    let day_of_year = day_of_era
        - (
            365 * year_of_era
                + year_of_era / 4
                - year_of_era / 100
        );

    let month_prime = (5 * day_of_year + 2) / 153;
    let day =
        day_of_year - (153 * month_prime + 2) / 5 + 1;

    let month =
        month_prime + if month_prime < 10 { 3 } else { -9 };

    if month <= 2 {
        year += 1;
    }

    (year, month, day)
}

fn x_fraction(bytes: usize) -> f64 {
    let smallest = (INPUT_SIZES[0].bytes as f64).log2();
    let largest =
        (INPUT_SIZES[INPUT_COUNT - 1].bytes as f64).log2();

    ((bytes as f64).log2() - smallest) / (largest - smallest)
}

fn gigabytes_per_second(ns_per_byte: f64) -> String {
    assert!(ns_per_byte > 0.0);

    let throughput = 1.0 / ns_per_byte;

    if throughput >= 10.0 {
        format!("{throughput:.0} GB/s")
    } else {
        format!("{throughput:.1} GB/s")
    }
}

/*
 * Ratio convention throughout: ALGORITHMS[0] median ÷ ALGORITHMS[1] median.
 * A ratio above 1.0 means ALGORITHMS[1] is faster.
 */
fn median_ratios(results: &Results) -> [f64; INPUT_COUNT] {
    std::array::from_fn(|size_index| {
        results[size_index][0].median / results[size_index][1].median
    })
}

fn generate_takeaway(results: &Results) -> String {
    let ratios = median_ratios(results);

    let lowest = ratios.iter().copied().fold(f64::INFINITY, f64::min);
    let highest = ratios.iter().copied().fold(0.0_f64, f64::max);

    if lowest > 1.0 {
        format!(
            "{} is faster at every tested size on this machine — {:.2}× to {:.2}× faster than {}",
            ALGORITHMS[1].name(),
            lowest,
            highest,
            ALGORITHMS[0].name(),
        )
    } else if highest < 1.0 {
        format!(
            "{} is faster at every tested size on this machine — {:.2}× to {:.2}× faster than {}",
            ALGORITHMS[0].name(),
            1.0 / highest,
            1.0 / lowest,
            ALGORITHMS[1].name(),
        )
    } else {
        let first_winner = if ratios[0] > 1.0 { 1 } else { 0 };
        let last_winner =
            if ratios[INPUT_COUNT - 1] > 1.0 { 1 } else { 0 };

        format!(
            "{} is faster at {}; {} is faster at {} — the crossover is the story",
            ALGORITHMS[first_winner].name(),
            INPUT_SIZES[0].label,
            ALGORITHMS[last_winner].name(),
            INPUT_SIZES[INPUT_COUNT - 1].label,
        )
    }
}

fn sanitize_alphanumeric(input: &str) -> String {
    let sanitized: String = input
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .collect();

    assert!(
        !sanitized.is_empty(),
        "sanitized identifier must not be empty; input was {input:?}"
    );

    sanitized
}

fn output_directory(machine: &MachineMetadata) -> std::path::PathBuf {
    let cpu = sanitize_alphanumeric(&machine.cpu_type);
    let os = sanitize_alphanumeric(&machine.os_type);

    std::path::PathBuf::from("benchmark-results")
        .join(format!("{cpu}.{os}"))
}

fn generate_svg(
    results: &Results,
    machine: &MachineMetadata,
) -> String {
    const WIDTH: f64 = 1200.0;
    const HEIGHT: f64 = 860.0;
    const PLOT_LEFT: f64 = 110.0;
    const PLOT_RIGHT: f64 = 1000.0;
    const PLOT_TOP: f64 = 135.0;
    const PLOT_BOTTOM: f64 = 455.0;
    const RATIO_TOP: f64 = 500.0;
    const RATIO_BOTTOM: f64 = 590.0;

    assert_eq!(
        ALGORITHM_COUNT, 2,
        "the ratio panel is defined for exactly two algorithms"
    );

    let observed_max = results
        .iter()
        .flatten()
        .map(|statistics| statistics.maximum)
        .fold(0.0_f64, f64::max);

    assert!(
        observed_max.is_finite() && observed_max > 0.0,
        "graph maximum must be finite and positive"
    );

    let observed_min = results
        .iter()
        .flatten()
        .map(|statistics| statistics.minimum)
        .fold(f64::INFINITY, f64::min);

    assert!(
        observed_min.is_finite() && observed_min > 0.0,
        "log axis requires positive measurements"
    );

    let axis_min = nice_log_bound_below(observed_min * 0.92);
    let axis_max = nice_log_bound_above(observed_max * 1.08);

    let log_min = axis_min.ln();
    let log_max = axis_max.ln();

    let map_y = |value: f64| {
        assert!(value > 0.0);

        PLOT_BOTTOM
            - (value.ln() - log_min) / (log_max - log_min)
            * (PLOT_BOTTOM - PLOT_TOP)
    };

    const X_INSET: f64 = 40.0;

    let x_positions: [f64; INPUT_COUNT] =
        std::array::from_fn(|index| {
            PLOT_LEFT
                + X_INSET
                + x_fraction(INPUT_SIZES[index].bytes)
                * (PLOT_RIGHT - PLOT_LEFT - 2.0 * X_INSET)
        });

    let implementation = detect_blake3_implementation();
    let takeaway = generate_takeaway(results);

    let mut svg = String::new();

    writeln!(svg, r##"<?xml version="1.0" encoding="UTF-8"?>"##)
        .unwrap();

    writeln!(
        svg,
        r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {WIDTH:.0} {HEIGHT:.0}" width="{WIDTH:.0}" height="{HEIGHT:.0}">"##
    )
        .unwrap();

    writeln!(
        svg,
        r##"  <rect width="{WIDTH:.0}" height="{HEIGHT:.0}" fill="#fdfdfc"/>"##
    )
        .unwrap();

    svg.push_str(
        r##"  <style>
    text { font-family: -apple-system, "Segoe UI", "Helvetica Neue", Arial, sans-serif; }
    .title { font-size: 22px; font-weight: 700; fill: #1a1a1a; }
    .takeaway { font-size: 14px; font-weight: 600; fill: #3a3a3a; }
    .method { font-size: 11px; fill: #8a8a8a; }
    .axis-title { font-size: 12px; fill: #666666; }
    .tick-label { font-size: 11px; fill: #777777; }
    .size-label { font-size: 13px; font-weight: 600; fill: #333333; }
    .size-sublabel { font-size: 10px; fill: #999999; }
    .value-label { font-size: 10px; font-weight: 700; }
    .series-name { font-size: 13px; font-weight: 700; }
    .series-detail { font-size: 10px; fill: #777777; }
    .annotation { font-size: 10px; font-style: italic; fill: #8a8a8a; }
    .prov-head { font-size: 10px; font-weight: 700; fill: #aaaaaa; letter-spacing: 0.1em; }
    .prov { font-size: 9px; fill: #9a9a9a; }
    .grid { stroke: #e8e8e6; stroke-width: 1; }
    .grid-x { stroke: #f0f0ee; stroke-width: 1; }
    .axis { stroke: #55555a; stroke-width: 1; }
    .divider { stroke: #e0e0de; stroke-width: 1; }
  </style>
"##,
    );

    writeln!(
        svg,
        r##"  <text x="{PLOT_LEFT:.0}" y="44" class="title">Cryptographic Hash Time per Byte</text>"##
    )
        .unwrap();

    writeln!(
        svg,
        r##"  <text x="{PLOT_LEFT:.0}" y="70" class="takeaway">{}</text>"##,
        xml_escape(&takeaway),
    )
        .unwrap();

    writeln!(
        svg,
        r##"  <text x="{PLOT_LEFT:.0}" y="90" class="method">Line and dot: median · shaded band: minimum–maximum across {SAMPLE_ROUNDS} interleaved samples · single-threaded · lower is better</text>"##
    )
        .unwrap();

    /* Horizontal grid and y-axis tick labels. */
    for value in log_ticks(axis_min, axis_max) {
        let y = map_y(value);
        writeln!(
            svg,
            r##"  <line x1="{PLOT_LEFT:.1}" y1="{y:.2}" x2="{PLOT_RIGHT:.1}" y2="{y:.2}" class="grid"/>"##
        )
            .unwrap();

        writeln!(
            svg,
            r##"  <text x="{:.1}" y="{:.2}" class="tick-label" text-anchor="end">{}</text>"##,
            PLOT_LEFT - 10.0,
            y + 3.5,
            format_tick(value),
        )
            .unwrap();
    }

    writeln!(
        svg,
        r##"  <text x="30" y="{:.1}" class="axis-title" text-anchor="middle" transform="rotate(-90 30 {:.1})">Nanoseconds per byte</text>"##,
        (PLOT_TOP + PLOT_BOTTOM) / 2.0,
        (PLOT_TOP + PLOT_BOTTOM) / 2.0,
    )
        .unwrap();

    /* Vertical guides and x-axis labels at each tested size. */
    for size_index in 0..INPUT_COUNT {
        let x = x_positions[size_index];

        writeln!(
            svg,
            r##"  <line x1="{x:.2}" y1="{PLOT_TOP:.1}" x2="{x:.2}" y2="{PLOT_BOTTOM:.1}" class="grid-x"/>"##
        )
            .unwrap();

        writeln!(
            svg,
            r##"  <text x="{x:.2}" y="{:.1}" class="size-label" text-anchor="middle">{}</text>"##,
            RATIO_BOTTOM + 24.0,
            xml_escape(INPUT_SIZES[size_index].label),
        )
            .unwrap();

        if !INPUT_SIZES[size_index].label.starts_with(
            &INPUT_SIZES[size_index].bytes.to_string(),
        ) {
            writeln!(
                svg,
                r##"  <text x="{x:.2}" y="{:.1}" class="size-sublabel" text-anchor="middle">{} bytes</text>"##,
                RATIO_BOTTOM + 38.0,
                INPUT_SIZES[size_index].bytes,
            )
                .unwrap();
        }
    }

    writeln!(
        svg,
        r##"  <text x="{:.1}" y="{:.1}" class="axis-title" text-anchor="middle">Input size (logarithmic spacing)</text>"##,
        (PLOT_LEFT + PLOT_RIGHT) / 2.0,
        RATIO_BOTTOM + 60.0,
    )
        .unwrap();

    writeln!(
        svg,
        r##"  <line x1="{PLOT_LEFT:.1}" y1="{PLOT_TOP:.1}" x2="{PLOT_LEFT:.1}" y2="{PLOT_BOTTOM:.1}" class="axis"/>"##
    )
        .unwrap();

    writeln!(
        svg,
        r##"  <line x1="{PLOT_LEFT:.1}" y1="{PLOT_BOTTOM:.1}" x2="{PLOT_RIGHT:.1}" y2="{PLOT_BOTTOM:.1}" class="axis"/>"##
    )
        .unwrap();

    /*
     * Min–max bands: forward along the maximum, back along the minimum.
     * Drawn first so lines and dots sit on top.
     */
    for algorithm_index in 0..ALGORITHM_COUNT {
        let algorithm = ALGORITHMS[algorithm_index];
        let mut band = String::new();

        for size_index in 0..INPUT_COUNT {
            let x = x_positions[size_index];
            let y = map_y(results[size_index][algorithm_index].maximum);

            if size_index == 0 {
                write!(band, "M {x:.2} {y:.2}").unwrap();
            } else {
                write!(band, " L {x:.2} {y:.2}").unwrap();
            }
        }

        for size_index in (0..INPUT_COUNT).rev() {
            let x = x_positions[size_index];
            let y = map_y(results[size_index][algorithm_index].minimum);

            write!(band, " L {x:.2} {y:.2}").unwrap();
        }

        band.push_str(" Z");

        writeln!(
            svg,
            r##"  <path d="{band}" fill="{}" fill-opacity="0.16" stroke="none"/>"##,
            algorithm.color(),
        )
            .unwrap();
    }

    /* Median lines. */
    for algorithm_index in 0..ALGORITHM_COUNT {
        let algorithm = ALGORITHMS[algorithm_index];
        let mut path = String::new();

        for size_index in 0..INPUT_COUNT {
            let x = x_positions[size_index];
            let y = map_y(results[size_index][algorithm_index].median);

            if size_index == 0 {
                write!(path, "M {x:.2} {y:.2}").unwrap();
            } else {
                write!(path, " L {x:.2} {y:.2}").unwrap();
            }
        }

        writeln!(
            svg,
            r##"  <path d="{path}" fill="none" stroke="{}" stroke-width="2.5" stroke-linejoin="round" stroke-linecap="round"/>"##,
            algorithm.color(),
        )
            .unwrap();
    }

    /* Median dots, hover tooltips, and value labels. */
    for algorithm_index in 0..ALGORITHM_COUNT {
        let algorithm = ALGORITHMS[algorithm_index];

        /*
         * Stagger labels vertically per algorithm so nearly-coincident
         * series (BLAKE3 and SHA-256 here) never collide.
         */
        let label_offset = match algorithm_index {
            1 => 20.0,
            _ => -12.0,
        };

        for size_index in 0..INPUT_COUNT {
            let x = x_positions[size_index];
            let statistics = results[size_index][algorithm_index];
            let median_y = map_y(statistics.median);

            writeln!(
                svg,
                r##"  <circle cx="{x:.2}" cy="{median_y:.2}" r="5" fill="{}" stroke="#fdfdfc" stroke-width="1.5">"##,
                algorithm.color(),
            )
                .unwrap();

            writeln!(
                svg,
                r##"    <title>{}, {}: median {} ns/B ({}); range {}–{} ns/B</title>"##,
                xml_escape(INPUT_SIZES[size_index].label),
                xml_escape(algorithm.name()),
                format_result_value(statistics.median),
                gigabytes_per_second(statistics.median),
                format_result_value(statistics.minimum),
                format_result_value(statistics.maximum),
            )
                .unwrap();

            svg.push_str("  </circle>\n");

            /*
             * Edge columns anchor inward so labels never spill into the
             * y-axis gutter or the right-edge series labels.
             */
            let (label_x, anchor) = if size_index == 0 {
                (x + 9.0, "start")
            } else if size_index == INPUT_COUNT - 1 {
                (x - 9.0, "end")
            } else {
                (x, "middle")
            };

            writeln!(
                svg,
                r##"  <text x="{label_x:.2}" y="{:.2}" class="value-label" fill="{}" text-anchor="{anchor}">{}</text>"##,
                median_y + label_offset,
                algorithm.color(),
                format_result_value(statistics.median),
            )
                .unwrap();
        }
    }

    /*
     * Direct series labels at the right edge, replacing the legend. Stack
     * them apart when medians nearly coincide.
     */
    let mut label_slots: Vec<(usize, f64)> = (0..ALGORITHM_COUNT)
        .map(|algorithm_index| {
            (
                algorithm_index,
                map_y(
                    results[INPUT_COUNT - 1][algorithm_index].median,
                ),
            )
        })
        .collect();

    label_slots.sort_by(|a, b| a.1.total_cmp(&b.1));

    for index in 1..label_slots.len() {
        let minimum_y = label_slots[index - 1].1 + 44.0;

        if label_slots[index].1 < minimum_y {
            label_slots[index].1 = minimum_y;
        }
    }

    for (algorithm_index, label_y) in label_slots {
        let algorithm = ALGORITHMS[algorithm_index];
        let statistics = results[INPUT_COUNT - 1][algorithm_index];
        let label_x = PLOT_RIGHT + 14.0;

        writeln!(
            svg,
            r##"  <text x="{label_x:.1}" y="{:.2}" class="series-name" fill="{}">{}</text>"##,
            label_y + 4.0,
            algorithm.color(),
            xml_escape(algorithm.name()),
        )
            .unwrap();

        writeln!(
            svg,
            r##"  <text x="{label_x:.1}" y="{:.2}" class="series-detail">{} ns/B · {} at 1 MiB</text>"##,
            label_y + 18.0,
            format_result_value(statistics.median),
            gigabytes_per_second(statistics.median),
        )
            .unwrap();
    }

    /*
     * Ratio panel: median of ALGORITHMS[0] divided by median of
     * ALGORITHMS[1] at each size, on a log scale so equal ratios are
     * equal distances. Above the dashed line, ALGORITHMS[1] is faster.
     */
    {
        let ratios = median_ratios(results);

        let ratio_low = ratios
            .iter()
            .copied()
            .fold(1.0_f64, f64::min)
            * 0.94;

        let ratio_high = ratios
            .iter()
            .copied()
            .fold(1.0_f64, f64::max)
            * 1.06;

        let ratio_log_low = ratio_low.ln();
        let ratio_log_high = ratio_high.ln();

        let map_ratio_y = |ratio: f64| {
            assert!(ratio > 0.0);

            RATIO_BOTTOM
                - (ratio.ln() - ratio_log_low)
                / (ratio_log_high - ratio_log_low)
                * (RATIO_BOTTOM - RATIO_TOP)
        };

        writeln!(
            svg,
            r##"  <text x="{PLOT_LEFT:.1}" y="{:.1}" class="axis-title">Speed ratio: {} time ÷ {} time — above the dashed line, {} is faster</text>"##,
            RATIO_TOP - 10.0,
            xml_escape(ALGORITHMS[0].name()),
            xml_escape(ALGORITHMS[1].name()),
            xml_escape(ALGORITHMS[1].name()),
        )
            .unwrap();

        /* Panel frame and per-size guides. */
        writeln!(
            svg,
            r##"  <line x1="{PLOT_LEFT:.1}" y1="{RATIO_TOP:.1}" x2="{PLOT_LEFT:.1}" y2="{RATIO_BOTTOM:.1}" class="axis"/>"##
        )
            .unwrap();

        writeln!(
            svg,
            r##"  <line x1="{PLOT_LEFT:.1}" y1="{RATIO_BOTTOM:.1}" x2="{PLOT_RIGHT:.1}" y2="{RATIO_BOTTOM:.1}" class="axis"/>"##
        )
            .unwrap();

        for x in x_positions {
            writeln!(
                svg,
                r##"  <line x1="{x:.2}" y1="{RATIO_TOP:.1}" x2="{x:.2}" y2="{RATIO_BOTTOM:.1}" class="grid-x"/>"##
            )
                .unwrap();
        }

        /* Equal-speed reference line. */
        let equal_y = map_ratio_y(1.0);

        writeln!(
            svg,
            r##"  <line x1="{PLOT_LEFT:.1}" y1="{equal_y:.2}" x2="{PLOT_RIGHT:.1}" y2="{equal_y:.2}" stroke="#aaaaaa" stroke-width="1" stroke-dasharray="5,4"/>"##
        )
            .unwrap();

        writeln!(
            svg,
            r##"  <text x="{:.1}" y="{:.2}" class="tick-label" text-anchor="end">1.0×</text>"##,
            PLOT_LEFT - 10.0,
            equal_y + 3.5,
        )
            .unwrap();

        /* Connect the ratios, then dot and label each one. */
        let mut path = String::new();

        for size_index in 0..INPUT_COUNT {
            let x = x_positions[size_index];
            let y = map_ratio_y(ratios[size_index]);

            if size_index == 0 {
                write!(path, "M {x:.2} {y:.2}").unwrap();
            } else {
                write!(path, " L {x:.2} {y:.2}").unwrap();
            }
        }

        writeln!(
            svg,
            r##"  <path d="{path}" fill="none" stroke="#888888" stroke-width="1.5" stroke-linejoin="round"/>"##
        )
            .unwrap();

        for size_index in 0..INPUT_COUNT {
            let x = x_positions[size_index];
            let ratio = ratios[size_index];
            let y = map_ratio_y(ratio);

            /* Color each dot by whichever algorithm wins at that size. */
            let winner = if ratio > 1.0 {
                ALGORITHMS[1]
            } else {
                ALGORITHMS[0]
            };

            writeln!(
                svg,
                r##"  <circle cx="{x:.2}" cy="{y:.2}" r="4.5" fill="{}" stroke="#fdfdfc" stroke-width="1.5"/>"##,
                winner.color(),
            )
                .unwrap();

            let (label_x, anchor) = if size_index == 0 {
                (x + 8.0, "start")
            } else if size_index == INPUT_COUNT - 1 {
                (x - 8.0, "end")
            } else {
                (x, "middle")
            };

            /*
             * Label below the dot unless that would crowd the dashed
             * equal-speed line; then label above.
             */
            let below = y + 18.0;

            let label_y = if (below - equal_y).abs() < 12.0 {
                equal_y + 14.0
            } else {
                below
            };

            writeln!(
                svg,
                r##"  <text x="{label_x:.2}" y="{label_y:.2}" class="value-label" fill="{}" text-anchor="{anchor}">{ratio:.2}×</text>"##,
                winner.color(),
            )
                .unwrap();
        }
    }

    /*
     * Annotate the BLAKE3 single-chunk elbow: the 64 B point uses a
     * different code path than the bulk sizes, and that is the whole story
     * of its shape.
     */
    {
        let blake3_index = 0;
        let x = x_positions[0];
        let y = map_y(results[0][blake3_index].median);

        let short_backend = implementation
            .one_chunk
            .split(" (")
            .next()
            .expect("backend description is not empty");

        writeln!(
            svg,
            r##"  <line x1="{:.2}" y1="{:.2}" x2="{:.2}" y2="{:.2}" stroke="#bbbbbb" stroke-width="1"/>"##,
            x + 8.0,
            y - 8.0,
            x + 42.0,
            y - 48.0,
        )
            .unwrap();

        writeln!(
            svg,
            r##"  <text x="{:.2}" y="{:.2}" class="annotation">BLAKE3: 64 B fits one chunk → {}</text>"##,
            x + 46.0,
            y - 52.0,
            xml_escape(short_backend),
        )
            .unwrap();

        writeln!(
            svg,
            r##"  <text x="{:.2}" y="{:.2}" class="annotation">(bulk sizes use {})</text>"##,
            x + 46.0,
            y - 40.0,
            xml_escape(
                implementation
                    .bulk
                    .split(" (")
                    .next()
                    .expect("backend description is not empty"),
            ),
        )
            .unwrap();
    }

    /* Machine-readable provenance, complete and untruncated. */
    writeln!(svg, "  <metadata>").unwrap();

    for (name, value) in [
        ("timestamp", machine.timestamp.as_str()),
        ("git source", GIT_SOURCE),
        ("git commit", GIT_COMMIT),
        ("git tag", GIT_TAG),
        ("git clean status", GIT_CLEAN_STATUS),
        ("bench-hashes version", BENCH_VERSION),
        ("CPU type", machine.cpu_type.as_str()),
        ("OS type", machine.os_type.as_str()),
        ("Rust compiler", RUSTC_VERSION),
        ("build target", BUILD_TARGET),
        ("target features", TARGET_FEATURES),
        ("BLAKE3 source", BLAKE3_SOURCE_INFO),
        ("SHA-256 source", SHA2_SOURCE_INFO),
        ("SHA-256 assembly source", SHA2_ASM_SOURCE_INFO),
    ] {
        writeln!(
            svg,
            "    {}: {}",
            xml_escape(name),
            xml_escape(value),
        )
            .unwrap();
    }

    writeln!(svg, "  </metadata>").unwrap();

    /* Human-readable provenance: left-aligned, compact, de-emphasized. */
    let provenance_top = RATIO_BOTTOM + 85.0;

    writeln!(
        svg,
        r##"  <line x1="{PLOT_LEFT:.1}" y1="{:.1}" x2="{:.1}" y2="{:.1}" class="divider"/>"##,
        provenance_top,
        WIDTH - PLOT_LEFT,
        provenance_top,
    )
        .unwrap();

    writeln!(
        svg,
        r##"  <text x="{PLOT_LEFT:.1}" y="{:.1}" class="prov-head">PROVENANCE</text>"##,
        provenance_top + 20.0,
    )
        .unwrap();

    let provenance_lines = [
        format!(
            "Run: {} · bench-hashes {BENCH_VERSION}",
            machine.timestamp,
        ),
        format!(
            "Machine: {} · {} logical CPUs · {}",
            machine.cpu_type, machine.cpu_count, machine.os_type,
        ),
        format!(
            "Toolchain: {RUSTC_VERSION} · {BUILD_TARGET}"
        ),
        format!(
            "Source: {GIT_SOURCE} @ {GIT_COMMIT}"
        ),
        format!(
            "Tag: {GIT_TAG} · Working tree: {GIT_CLEAN_STATUS}"
        ),
        format!(
            "Crates: {} · {} (+{})",
            package_name_and_version(BLAKE3_SOURCE_INFO),
            package_name_and_version(SHA2_SOURCE_INFO),
            package_name_and_version(SHA2_ASM_SOURCE_INFO),
        ),
        format!(
            "BLAKE3: single-threaded, Rayon not enabled · platform {} · full crate checksums embedded in this file's metadata element",
            implementation.platform,
        ),
    ];

    for (index, line) in provenance_lines.iter().enumerate() {
        writeln!(
            svg,
            r##"  <text x="{PLOT_LEFT:.1}" y="{:.1}" class="prov">{}</text>"##,
            provenance_top + 40.0 + index as f64 * 15.0,
            xml_escape(line),
        )
            .unwrap();
    }

    svg.push_str("</svg>\n");
    svg
}

fn package_name_and_version(source_info: &str) -> &str {
    source_info
        .split(';')
        .next()
        .expect("package source information must not be empty")
}

fn nice_log_bound_below(value: f64) -> f64 {
    assert!(value.is_finite() && value > 0.0);

    let magnitude = 10.0_f64.powf(value.log10().floor());
    let normalized = value / magnitude;

    let nice = if normalized >= 5.0 {
        5.0
    } else if normalized >= 2.0 {
        2.0
    } else {
        1.0
    };

    nice * magnitude
}

fn nice_log_bound_above(value: f64) -> f64 {
    assert!(value.is_finite() && value > 0.0);

    let magnitude = 10.0_f64.powf(value.log10().floor());
    let normalized = value / magnitude;

    let nice = if normalized <= 1.0 {
        1.0
    } else if normalized <= 2.0 {
        2.0
    } else if normalized <= 5.0 {
        5.0
    } else {
        10.0
    };

    nice * magnitude
}

fn log_ticks(axis_min: f64, axis_max: f64) -> Vec<f64> {
    let lowest_exponent = axis_min.log10().floor() as i32 - 1;
    let highest_exponent = axis_max.log10().ceil() as i32 + 1;

    let mut ticks = Vec::new();

    for exponent in lowest_exponent..=highest_exponent {
        for mantissa in [1.0, 1.5, 2.0, 3.0, 5.0, 7.0] {
            let value = mantissa * 10.0_f64.powi(exponent);

            /*
             * Tolerate one part in a million of floating-point error at
             * the axis bounds themselves.
             */
            if value >= axis_min * 0.999_999
                && value <= axis_max * 1.000_001
            {
                ticks.push(value);
            }
        }
    }

    assert!(
        ticks.len() >= 2,
        "a log axis must have at least two ticks"
    );

    ticks
}

fn format_tick(value: f64) -> String {
    assert!(
        value > 0.0,
        "log-axis ticks must be positive"
    );

    if value >= 10.0 {
        format!("{value:.0}")
    } else if value >= 1.0 {
        format!("{value:.1}")
    } else {
        format!("{value:.2}")
    }
}

fn format_result_value(value: f64) -> String {
    format!("{value:.2}")
}

fn xml_escape(input: &str) -> String {
    let mut escaped = String::with_capacity(input.len());

    for character in input.chars() {
        match character {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&apos;"),
            _ => escaped.push(character),
        }
    }

    escaped
}

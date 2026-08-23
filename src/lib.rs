use sha2::{Digest, Sha256};
use sha3::Sha3_256;
use std::fmt::Write as _;
use std::hint::black_box;

#[cfg(not(target_arch = "wasm32"))]
use std::fs;

#[cfg(not(target_arch = "wasm32"))]
use std::time::{Instant, SystemTime, UNIX_EPOCH};

#[cfg(target_arch = "wasm32")]
use web_time::Instant;

#[cfg(target_arch = "wasm32")]
use wasm_bindgen::prelude::*;

#[cfg(all(
    target_arch = "wasm32",
    not(target_feature = "simd128")
))]
compile_error!(
    "The browser build requires +simd128; use the supplied .cargo/config.toml"
);

const SAMPLE_ROUNDS: usize = 120;
const CALIBRATION_PROBE_NS: u128 = 1_000_000;
const TARGET_SAMPLE_NS: u128 = 4_000_000;

const BENCH_VERSION: &str = env!("CARGO_PKG_VERSION");
const GIT_SOURCE: &str = env!("BENCH_GIT_SOURCE");
const GIT_COMMIT: &str = env!("BENCH_GIT_COMMIT");
const GIT_TAG: &str = env!("BENCH_GIT_TAG");
const GIT_CLEAN_STATUS: &str =
    env!("BENCH_GIT_CLEAN_STATUS");

const RUSTC_VERSION: &str = env!("BENCH_RUSTC_VERSION");
const BUILD_TARGET: &str = env!("BENCH_BUILD_TARGET");
const TARGET_FEATURES: &str = env!("BENCH_TARGET_FEATURES");

const BLAKE3_SOURCE_INFO: &str =
    env!("BLAKE3_SOURCE_INFO");
const SHA2_SOURCE_INFO: &str =
    env!("SHA2_SOURCE_INFO");
const SHA2_ASM_SOURCE_INFO: &str =
    env!("SHA2_ASM_SOURCE_INFO");
const SHA3_SOURCE_INFO: &str =
    env!("SHA3_SOURCE_INFO");
const KECCAK_SOURCE_INFO: &str =
    env!("KECCAK_SOURCE_INFO");
const KECCAK_ASM_SOURCE_INFO: &str =
    env!("KECCAK_ASM_SOURCE_INFO");

const RUSTC_VERSION: &str = env!("BENCH_RUSTC_VERSION");
const BUILD_TARGET: &str = env!("BENCH_BUILD_TARGET");


const INPUT_SIZES: [InputSize; 4] = [
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

const ALGORITHMS: [Algorithm; 3] = [
    Algorithm::Blake3,
    Algorithm::Sha256,
    Algorithm::Sha3_256,
];

/*
 * All six permutations are used equally often because SAMPLE_ROUNDS is
 * divisible by six. Thus every algorithm appears equally often in every
 * position within an interleaved group.
 */
const ALGORITHM_ORDERS: [[usize; 3]; 6] = [
    [0, 1, 2],
    [0, 2, 1],
    [1, 0, 2],
    [1, 2, 0],
    [2, 0, 1],
    [2, 1, 0],
];

#[derive(Clone, Copy)]
struct InputSize {
    label: &'static str,
    bytes: usize,
}

#[derive(Clone, Copy)]
enum Algorithm {
    Blake3,
    Sha256,
    Sha3_256,
}

impl Algorithm {
    fn name(self) -> &'static str {
        match self {
            Self::Blake3 => "BLAKE3",
            Self::Sha256 => "SHA-256",
            Self::Sha3_256 => "SHA3-256",
        }
    }

    fn color(self) -> &'static str {
        match self {
            Self::Blake3 => "#3b82f6",
            Self::Sha256 => "#e07a45",
            Self::Sha3_256 => "#8b5cf6",
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

fn main() -> Result<(), Box<dyn Error>> {
    let timestamp = utc_timestamp();

    println!("TIMESTAMP: {timestamp}");
    println!("Rust compiler: {RUSTC_VERSION}");
    println!("Build target: {BUILD_TARGET}");
    println!("BLAKE3 source: {BLAKE3_SOURCE_INFO}");
    println!("SHA-256 API source: {SHA2_SOURCE_INFO}");
    println!("SHA-256 assembly source: {SHA2_ASM_SOURCE_INFO}");
    println!("SHA3-256 API source: {SHA3_SOURCE_INFO}");
    println!("Keccak source: {KECCAK_SOURCE_INFO}");
    println!("Keccak assembly source: {KECCAK_ASM_SOURCE_INFO}");
    println!("BLAKE3 mode: single-threaded; Rayon feature not enabled");
    println!();

    print_blake3_backend_report();

    let inputs: Vec<Vec<u8>> = INPUT_SIZES
        .iter()
        .map(|size| make_input(size.bytes))
        .collect();

    /*
     * Calibrate a separate batch count for each algorithm and input size.
     * This makes timed blocks approximately equal in duration, reducing the
     * extent to which a slow algorithm occupies a disproportionately long
     * uninterrupted interval.
     */
    let mut batch_iterations = [[1usize; 3]; 4];

    for size_index in 0..INPUT_SIZES.len() {
        for algorithm_index in 0..ALGORITHMS.len() {
            batch_iterations[size_index][algorithm_index] = calibrate_batch(
                ALGORITHMS[algorithm_index],
                &inputs[size_index],
            );
        }
    }

    /*
     * Warm up the relevant dispatch paths and code. This uses the same
     * balanced permutations as the measured portion but is not recorded.
     */
    for warmup_round in 0..ALGORITHM_ORDERS.len() {
        let order = ALGORITHM_ORDERS[warmup_round];

        for size_offset in 0..INPUT_SIZES.len() {
            let size_index =
                (size_offset + warmup_round) % INPUT_SIZES.len();

            for algorithm_index in order {
                run_batch(
                    ALGORITHMS[algorithm_index],
                    &inputs[size_index],
                    batch_iterations[size_index][algorithm_index],
                );
            }
        }
    }

    let mut samples: Vec<Vec<Vec<f64>>> = (0..INPUT_SIZES.len())
        .map(|_| {
            (0..ALGORITHMS.len())
                .map(|_| Vec::with_capacity(SAMPLE_ROUNDS))
                .collect()
        })
        .collect();

    /*
     * Measurement order:
     *
     * - Algorithm order cycles through all six permutations.
     * - Input-size order rotates every round.
     * - Every algorithm gets every position equally often.
     * - Every input size gets every size-order position equally often.
     */
    for round in 0..SAMPLE_ROUNDS {
        let algorithm_order =
            ALGORITHM_ORDERS[round % ALGORITHM_ORDERS.len()];

        for size_offset in 0..INPUT_SIZES.len() {
            let size_index = (size_offset + round) % INPUT_SIZES.len();
            let input = &inputs[size_index];

            for algorithm_index in algorithm_order {
                let iterations =
                    batch_iterations[size_index][algorithm_index];
                let algorithm = ALGORITHMS[algorithm_index];

                let started = Instant::now();
                run_batch(algorithm, input, iterations);
                let elapsed = started.elapsed();

                let total_bytes =
                    input.len() as f64 * iterations as f64;
                let nanoseconds_per_byte =
                    elapsed.as_secs_f64() * 1_000_000_000.0
                    / total_bytes;

                samples[size_index][algorithm_index]
                    .push(nanoseconds_per_byte);
            }
        }
    }

    let mut results = [
        [Statistics::ZERO; ALGORITHMS.len()];
        INPUT_SIZES.len()
    ];

    for size_index in 0..INPUT_SIZES.len() {
        for algorithm_index in 0..ALGORITHMS.len() {
            results[size_index][algorithm_index] =
                summarize(
                    &mut samples[size_index][algorithm_index],
                );
        }
    }

    println!("============================================================");
    println!("HASH BENCHMARK RESULTS");
    println!("Time per byte; lower is better");
    println!("============================================================");
    println!();

    for size_index in 0..INPUT_SIZES.len() {
        println!("Input size: {}", INPUT_SIZES[size_index].label);

        for algorithm_index in 0..ALGORITHMS.len() {
            let stats = results[size_index][algorithm_index];

            println!(
                "name: {:<8}, median ns/B: {:>5.2}, minimum ns/B: {:>5.2}, maximum ns/B: {:>5.2}",
                ALGORITHMS[algorithm_index].name(),
                stats.median,
                stats.minimum,
                stats.maximum,
            );
        }

        println!();
    }

    println!(
        "{:<10} {:<10} {:>18} {:>18} {:>18}",
        "input", "algorithm", "median ns/B", "minimum ns/B", "maximum ns/B"
    );

    for size_index in 0..INPUT_SIZES.len() {
        for algorithm_index in 0..ALGORITHMS.len() {
            let stats = results[size_index][algorithm_index];

            println!(
                "{:<10} {:<10} {:>18.6} {:>18.6} {:>18.6}",
                INPUT_SIZES[size_index].label,
                ALGORITHMS[algorithm_index].name(),
                stats.median,
                stats.minimum,
                stats.maximum,
            );
        }

        println!();
    }

    let backend = blake3_backend_description();
    let svg = generate_svg(&results, &timestamp, &backend);
    fs::write("bench-hashes.svg", svg)?;

    println!("Graph saved to: bench-hashes.svg");

    Ok(())
}

fn make_input(size: usize) -> Vec<u8> {
    let mut input = vec![0_u8; size];

    /*
     * Deterministic xorshift data avoids allocating or generating data inside
     * a timed interval. Cryptographic hash speed should not depend on the
     * values of these bytes.
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

fn run_batch(algorithm: Algorithm, input: &[u8], iterations: usize) {
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
        Algorithm::Sha3_256 => {
            for _ in 0..iterations {
                let digest = Sha3_256::digest(black_box(input));
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
         * Browser clocks can return zero for intervals below their timer
         * resolution. Increase the batch until it is measurable.
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

            assert!(
                scaled > 0 && scaled <= usize::MAX as u128,
                "calibrated iteration count must fit in usize"
            );

            return scaled as usize;
        }

        let factor =
            (CALIBRATION_PROBE_NS + elapsed_ns - 1)
            / elapsed_ns;

        assert!(
            factor >= 2,
            "sub-probe timing must require growth"
        );

        iterations = iterations
            .checked_mul(
                usize::try_from(factor)
                    .expect("calibration factor must fit usize"),
            )
            .expect("calibration iteration count overflowed");
    }
}

fn summarize(samples: &mut [f64]) -> Statistics {
    assert_eq!(
        samples.len(),
        SAMPLE_ROUNDS,
        "every benchmark must have exactly SAMPLE_ROUNDS samples"
    );

    assert!(
        samples
            .iter()
            .all(|sample| sample.is_finite() && *sample > 0.0),
        "all timing samples must be finite and positive"
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

struct MachineMetadata {
    timestamp: String,
    cpu_type: String,
    cpu_count: usize,
    os_type: String,
}

fn generate_text(
    results: &[[Statistics; 3]; 4],
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
    writeln!(
        output,
        "Target features: {TARGET_FEATURES}"
    )
        .unwrap();
    writeln!(output, "BLAKE3 source: {BLAKE3_SOURCE_INFO}").unwrap();
    writeln!(output, "SHA-256 source: {SHA2_SOURCE_INFO}").unwrap();
    writeln!(
        output,
        "SHA-256 assembly source: {SHA2_ASM_SOURCE_INFO}"
    )
        .unwrap();
    writeln!(output, "SHA3-256 source: {SHA3_SOURCE_INFO}").unwrap();
    writeln!(output, "Keccak source: {KECCAK_SOURCE_INFO}").unwrap();
    writeln!(
        output,
        "Keccak assembly source: {KECCAK_ASM_SOURCE_INFO}"
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

    for size_index in 0..INPUT_SIZES.len() {
        writeln!(
            output,
            "  {}:",
            INPUT_SIZES[size_index].label
        )
            .unwrap();

        for algorithm_index in 0..ALGORITHMS.len() {
            let stats = results[size_index][algorithm_index];

            writeln!(
                output,
                "    {:<8}: {:>5.2} ({:>5.2}–{:>5.2})",
                ALGORITHMS[algorithm_index].name(),
                stats.median,
                stats.minimum,
                stats.maximum,
            )
                .unwrap();
        }

        writeln!(output).unwrap();
    }

    output
}

#[derive(Clone, Copy)]
struct Blake3Implementation {
    platform: &'static str,
    single_input: &'static str,
    bulk: &'static str,
}

/*
 * These backend inferences mirror the behavior of BLAKE3 v1.8.7,
 * particularly src/platform.rs. They describe the implementation selected
 * for the performance-dominant path for each benchmark input size.
 */
fn detect_blake3_implementation() -> Blake3Implementation {
    #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
    {
        if std::arch::is_x86_feature_detected!("avx512f")
            && std::arch::is_x86_feature_detected!("avx512vl")
        {
            return Blake3Implementation {
                platform: "AVX-512",
                single_input: "AVX-512 compression",
                bulk: "AVX-512 hash_many (16-way SIMD)",
            };
        }

        if std::arch::is_x86_feature_detected!("avx2") {
            return Blake3Implementation {
                platform: "AVX2",
                single_input: "SSE4.1 compression",
                bulk: "AVX2 hash_many (8-way SIMD)",
            };
        }

        if std::arch::is_x86_feature_detected!("sse4.1") {
            return Blake3Implementation {
                platform: "SSE4.1",
                single_input: "SSE4.1 compression",
                bulk: "SSE4.1 hash_many (4-way SIMD)",
            };
        }

        if std::arch::is_x86_feature_detected!("sse2") {
            return Blake3Implementation {
                platform: "SSE2",
                single_input: "SSE2 compression",
                bulk: "SSE2 hash_many (4-way SIMD)",
            };
        }

        return Blake3Implementation {
            platform: "portable",
            single_input: "portable compression",
            bulk: "portable hash_many",
        };
    }

    #[cfg(target_arch = "aarch64")]
    {
        return Blake3Implementation {
            platform: "NEON",
            single_input: "portable compression",
            bulk: "NEON hash_many (4-way SIMD)",
        };
    }

    #[cfg(target_arch = "arm")]
    {
        if cfg!(target_feature = "neon") {
            return Blake3Implementation {
                platform: "NEON",
                single_input: "portable compression",
                bulk: "NEON hash_many (4-way SIMD)",
            };
        }

        return Blake3Implementation {
            platform: "portable ARM",
            single_input: "portable compression",
            bulk: "portable hash_many",
        };
    }

    #[cfg(target_arch = "wasm32")]
    {
        return Blake3Implementation {
            platform: "WebAssembly SIMD128",
            single_input: "WebAssembly SIMD128 compression",
            bulk: "WebAssembly SIMD128 hash_many",
        };
    }

    #[allow(unreachable_code)]
    Blake3Implementation {
        platform: "portable",
        single_input: "portable compression",
        bulk: "portable hash_many",
    }
}

fn blake3_backend_for_input(
    implementation: Blake3Implementation,
    input_bytes: usize,
) -> &'static str {
    if input_bytes <= 1024 {
        implementation.single_input
    } else {
        implementation.bulk
    }
}

fn append_blake3_backend_report(output: &mut String) {
    let implementation = detect_blake3_implementation();

    writeln!(
        output,
        "BLAKE3 implementation selection:"
    )
        .unwrap();
    writeln!(
        output,
        "  selected platform: {}",
        implementation.platform
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

fn print_blake3_backend_report() {
    let implementation = detect_blake3_implementation();

    println!("BLAKE3 implementation selection:");
    println!("  selected platform: {}", implementation.platform);

    for input_size in INPUT_SIZES {
        println!(
            "  {:>7}: {}",
            input_size.label,
            blake3_backend_for_input(implementation, input_size.bytes),
        );
    }

    println!();
}

#[cfg(not(target_arch = "wasm32"))]
fn utc_timestamp() -> String {
    let duration = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("the system clock must be after the Unix epoch");

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

/*
 * Gregorian calendar conversion based on the standard civil-from-days
 * arithmetic. Unix day zero is 1970-01-01.
 */
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
        - (365 * year_of_era
           + year_of_era / 4
           - year_of_era / 100);

    let month_prime = (5 * day_of_year + 2) / 153;
    let day =
        day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = month_prime
        + if month_prime < 10 { 3 } else { -9 };

    if month <= 2 {
        year += 1;
    }

    (year, month, day)
}

fn generate_svg(
    results: &[Vec<Statistics>],
    timestamp: &str,
    backend: &str,
) -> String {
    const WIDTH: f64 = 1200.0;
    const HEIGHT: f64 = 750.0;
    const PLOT_LEFT: f64 = 105.0;
    const PLOT_RIGHT: f64 = 1125.0;
    const PLOT_TOP: f64 = 105.0;
    const PLOT_BOTTOM: f64 = 535.0;
    const TICK_COUNT: usize = 4;

    let mut observed_max = 0.0_f64;

    for size_results in results {
        for stats in size_results {
            observed_max = observed_max.max(stats.maximum);
        }
    }

    let axis_max = nice_linear_ceiling(observed_max * 1.08);

    let map_y = |value: f64| {
        PLOT_BOTTOM
            - value / axis_max * (PLOT_BOTTOM - PLOT_TOP)
    };

    let plot_width = PLOT_RIGHT - PLOT_LEFT;
    let x_positions: Vec<f64> = (0..INPUT_SIZES.len())
        .map(|index| {
            PLOT_LEFT
                + plot_width * index as f64
                / (INPUT_SIZES.len() - 1) as f64
        })
        .collect();

    let mut svg = String::new();

    writeln!(
        svg,
        r#"<?xml version="1.0" encoding="UTF-8"?>"#
    )
        .unwrap();

    writeln!(
        svg,
        r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {WIDTH:.0} {HEIGHT:.0}" width="{WIDTH:.0}" height="{HEIGHT:.0}">"#
    )
        .unwrap();

    writeln!(
        svg,
        r#"  <rect width="{WIDTH:.0}" height="{HEIGHT:.0}" fill="white"/>"#
    )
        .unwrap();

    svg.push_str(
        r#"  <style>
    .title { font-family: sans-serif; font-size: 20px; font-weight: bold; fill: #2d2d2d; }
    .subtitle { font-family: sans-serif; font-size: 12px; fill: #666666; }
    .axis-label { font-family: sans-serif; font-size: 12px; fill: #555555; }
    .tick-label { font-family: sans-serif; font-size: 10px; fill: #555555; }
    .size-label { font-family: sans-serif; font-size: 12px; font-weight: bold; fill: #333333; }
    .legend-label { font-family: sans-serif; font-size: 11px; fill: #333333; }
    .value-label { font-family: sans-serif; font-size: 9px; font-weight: bold; }
    .grid { stroke: #dddddd; stroke-width: 1; }
    .vertical-grid { stroke: #eeeeee; stroke-width: 1; }
    .axis { stroke: #444444; stroke-width: 1; }
    .metadata { font-family: sans-serif; font-size: 8px; font-style: italic; fill: #999999; }
  </style>
"#,
    );

    writeln!(
        svg,
        r#"  <text x="600" y="30" class="title" text-anchor="middle">Cryptographic Hash Time per Byte</text>"#
    )
        .unwrap();

    writeln!(
        svg,
        r#"  <text x="600" y="50" class="subtitle" text-anchor="middle">Median, minimum, and maximum connected across input sizes · lower is better · linear scale</text>"#
    )
        .unwrap();

    writeln!(
        svg,
        r#"  <text x="25" y="320" class="axis-label" text-anchor="middle" transform="rotate(-90 25 320)">Nanoseconds per byte</text>"#
    )
        .unwrap();

    for tick_index in 0..=TICK_COUNT {
        let value =
            axis_max * tick_index as f64 / TICK_COUNT as f64;
        let y = map_y(value);

        writeln!(
            svg,
            r#"  <line x1="{PLOT_LEFT:.1}" y1="{y:.2}" x2="{PLOT_RIGHT:.1}" y2="{y:.2}" class="grid"/>"#
        )
            .unwrap();

        writeln!(
            svg,
            r#"  <text x="94" y="{:.2}" class="tick-label" text-anchor="end">{}</text>"#,
            y + 3.5,
            format_linear_tick(value),
        )
            .unwrap();
    }

    for (size_index, &x) in x_positions.iter().enumerate() {
        writeln!(
            svg,
            r#"  <line x1="{x:.2}" y1="{PLOT_TOP:.1}" x2="{x:.2}" y2="{PLOT_BOTTOM:.1}" class="vertical-grid"/>"#
        )
            .unwrap();

        writeln!(
            svg,
            r#"  <text x="{x:.2}" y="558" class="size-label" text-anchor="middle">{}</text>"#,
            xml_escape(INPUT_SIZES[size_index].label),
        )
            .unwrap();
    }

    writeln!(
        svg,
        r#"  <line x1="{PLOT_LEFT:.1}" y1="{PLOT_TOP:.1}" x2="{PLOT_LEFT:.1}" y2="{PLOT_BOTTOM:.1}" class="axis"/>"#
    )
        .unwrap();

    writeln!(
        svg,
        r#"  <line x1="{PLOT_LEFT:.1}" y1="{PLOT_BOTTOM:.1}" x2="{PLOT_RIGHT:.1}" y2="{PLOT_BOTTOM:.1}" class="axis"/>"#
    )
        .unwrap();

    for algorithm_index in 0..ALGORITHMS.len() {
        let algorithm = ALGORITHMS[algorithm_index];

        for (selector, dash, opacity) in [
            (
                |stats: Statistics| stats.minimum,
                "6,4",
                0.45,
            ),
            (
                |stats: Statistics| stats.maximum,
                "2,4",
                0.45,
            ),
        ] {
            let mut path = String::new();

            for (size_index, &x) in
                x_positions.iter().enumerate()
            {
                let y = map_y(selector(
                    results[size_index][algorithm_index],
                ));

                if size_index == 0 {
                    write!(path, "M {x:.2} {y:.2}").unwrap();
                } else {
                    write!(path, " L {x:.2} {y:.2}").unwrap();
                }
            }

            writeln!(
                svg,
                r#"  <path d="{path}" fill="none" stroke="{}" stroke-width="1.5" stroke-dasharray="{dash}" stroke-linejoin="round" opacity="{opacity:.2}"/>"#,
                algorithm.color(),
            )
                .unwrap();
        }
    }
    /*
     * Draw each median line first, so the range markers and dots appear on
     * top of the line.
     */
    for algorithm_index in 0..ALGORITHMS.len() {
        let algorithm = ALGORITHMS[algorithm_index];
        let mut path = String::new();

        for (size_index, &x) in x_positions.iter().enumerate() {
            let y =
                map_y(results[size_index][algorithm_index].median);

            if size_index == 0 {
                write!(path, "M {x:.2} {y:.2}").unwrap();
            } else {
                write!(path, " L {x:.2} {y:.2}").unwrap();
            }
        }

        writeln!(
            svg,
            r#"  <path d="{path}" fill="none" stroke="{}" stroke-width="3" stroke-linejoin="round" stroke-linecap="round" opacity="0.85"/>"#,
            algorithm.color(),
        )
            .unwrap();
    }

    for algorithm_index in 0..ALGORITHMS.len() {
        let algorithm = ALGORITHMS[algorithm_index];

        for (size_index, &x) in x_positions.iter().enumerate() {
            let stats = results[size_index][algorithm_index];

            let minimum_y = map_y(stats.minimum);
            let median_y = map_y(stats.median);
            let maximum_y = map_y(stats.maximum);

            /*
             * All algorithms intentionally use the same x coordinate for an
             * input size. Their measured times place them vertically.
             */
            writeln!(
                svg,
                r#"  <line x1="{x:.2}" y1="{maximum_y:.2}" x2="{x:.2}" y2="{minimum_y:.2}" stroke="{}" stroke-width="4" stroke-linecap="round" opacity="0.30"/>"#,
                algorithm.color(),
            )
                .unwrap();

            writeln!(
                svg,
                r#"  <line x1="{:.2}" y1="{maximum_y:.2}" x2="{:.2}" y2="{maximum_y:.2}" stroke="{}" stroke-width="2"/>"#,
                x - 7.0,
                x + 7.0,
                algorithm.color(),
            )
                .unwrap();

            writeln!(
                svg,
                r#"  <line x1="{:.2}" y1="{minimum_y:.2}" x2="{:.2}" y2="{minimum_y:.2}" stroke="{}" stroke-width="2"/>"#,
                x - 7.0,
                x + 7.0,
                algorithm.color(),
            )
                .unwrap();

            writeln!(
                svg,
                r#"  <circle cx="{x:.2}" cy="{median_y:.2}" r="6" fill="{}" stroke="white" stroke-width="1.5">"#,
                algorithm.color(),
            )
                .unwrap();

            writeln!(
                svg,
                r#"    <title>{}, {}: median {} ns/B; minimum {} ns/B; maximum {} ns/B</title>"#,
                xml_escape(INPUT_SIZES[size_index].label),
                xml_escape(algorithm.name()),
                format_result_value(stats.median),
                format_result_value(stats.minimum),
                format_result_value(stats.maximum),
            )
                .unwrap();

            svg.push_str("  </circle>\n");

            writeln!(
                svg,
                r#"  <text x="{:.2}" y="{:.2}" class="value-label" fill="{}">{}</text>"#,
                x + 9.0,
                median_y - 8.0,
                algorithm.color(),
                format_result_value(stats.median),
            )
                .unwrap();
        }
    }

    let legend_start_x = 390.0;

    for (index, algorithm) in ALGORITHMS.iter().enumerate() {
        let x = legend_start_x + index as f64 * 165.0;

        writeln!(
            svg,
            r#"  <line x1="{x:.1}" y1="76" x2="{:.1}" y2="76" stroke="{}" stroke-width="3"/>"#,
            x + 26.0,
            algorithm.color(),
        )
            .unwrap();

        writeln!(
            svg,
            r#"  <circle cx="{:.1}" cy="76" r="5" fill="{}" stroke="white" stroke-width="1"/>"#,
            x + 13.0,
            algorithm.color(),
        )
            .unwrap();

        writeln!(
            svg,
            r#"  <text x="{:.1}" y="80" class="legend-label">{}</text>"#,
            x + 34.0,
            xml_escape(algorithm.name()),
        )
            .unwrap();
    }

    let escaped_timestamp = xml_escape(timestamp);
    let escaped_backend = xml_escape(backend);
    let escaped_blake3 = xml_escape(BLAKE3_SOURCE_INFO);
    let escaped_sha2 = xml_escape(SHA2_SOURCE_INFO);
    let escaped_sha2_asm = xml_escape(SHA2_ASM_SOURCE_INFO);
    let escaped_sha3 = xml_escape(SHA3_SOURCE_INFO);
    let escaped_keccak = xml_escape(KECCAK_SOURCE_INFO);
    let escaped_keccak_asm = xml_escape(KECCAK_ASM_SOURCE_INFO);
    let escaped_rustc = xml_escape(RUSTC_VERSION);
    let escaped_target = xml_escape(BUILD_TARGET);

    writeln!(
        svg,
        r#"  <text x="600" y="606" class="metadata" text-anchor="middle">Timestamp: {escaped_timestamp} · BLAKE3 bulk backend: {escaped_backend}</text>"#
    )
        .unwrap();

    writeln!(
        svg,
        r#"  <text x="600" y="624" class="metadata" text-anchor="middle">BLAKE3: {escaped_blake3}</text>"#
    )
        .unwrap();

    writeln!(
        svg,
        r#"  <text x="600" y="642" class="metadata" text-anchor="middle">SHA-256: {escaped_sha2}</text>"#
    )
        .unwrap();

    writeln!(
        svg,
        r#"  <text x="600" y="660" class="metadata" text-anchor="middle">SHA-256 assembly: {escaped_sha2_asm}</text>"#
    )
        .unwrap();

    writeln!(
        svg,
        r#"  <text x="600" y="678" class="metadata" text-anchor="middle">SHA3-256: {escaped_sha3} · {escaped_keccak}</text>"#
    )
        .unwrap();

    writeln!(
        svg,
        r#"  <text x="600" y="696" class="metadata" text-anchor="middle">Keccak assembly: {escaped_keccak_asm}</text>"#
    )
        .unwrap();

    writeln!(
        svg,
        r#"  <text x="600" y="714" class="metadata" text-anchor="middle">{escaped_rustc} · target {escaped_target}</text>"#
    )
        .unwrap();

    svg.push_str("</svg>\n");
    svg
}

fn nice_linear_ceiling(value: f64) -> f64 {
    assert!(
        value.is_finite() && value > 0.0,
        "linear-axis maximum must be finite and positive"
    );

    let magnitude =
        10.0_f64.powf(value.log10().floor());

    let normalized = value / magnitude;

    let nice_normalized = if normalized <= 1.0 {
        1.0
    } else if normalized <= 2.0 {
        2.0
    } else if normalized <= 4.0 {
        4.0
    } else if normalized <= 5.0 {
        5.0
    } else {
        10.0
    };

    nice_normalized * magnitude
}

fn format_linear_tick(value: f64) -> String {
    if value == 0.0 {
        "0".to_owned()
    } else if value >= 10.0 {
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

struct Artifacts {
    text: String,
    svg: String,
}

fn run_benchmark(machine: MachineMetadata) -> Artifacts {
    /*
     * Put the existing input creation, calibration, warmup, measurement,
     * and corrected results summarization here.
     */

    let results = measure_all();

    let text = generate_text(&results, &machine);
    let svg = generate_svg(&results, &machine);

    Artifacts { text, svg }
}

fn measure_all() -> [[Statistics; 3]; 4] {
    let inputs: Vec<Vec<u8>> = INPUT_SIZES
        .iter()
        .map(|size| make_input(size.bytes))
        .collect();

    let mut batch_iterations =
        [[1usize; ALGORITHMS.len()]; INPUT_SIZES.len()];

    for size_index in 0..INPUT_SIZES.len() {
        for algorithm_index in 0..ALGORITHMS.len() {
            batch_iterations[size_index][algorithm_index] =
                calibrate_batch(
                    ALGORITHMS[algorithm_index],
                    &inputs[size_index],
                );
        }
    }

    for warmup_round in 0..ALGORITHM_ORDERS.len() {
        let order = ALGORITHM_ORDERS[warmup_round];

        for size_offset in 0..INPUT_SIZES.len() {
            let size_index =
                (size_offset + warmup_round)
                % INPUT_SIZES.len();

            for algorithm_index in order {
                run_batch(
                    ALGORITHMS[algorithm_index],
                    &inputs[size_index],
                    batch_iterations[size_index]
                        [algorithm_index],
                );
            }
        }
    }

    let mut samples: Vec<Vec<Vec<f64>>> =
        (0..INPUT_SIZES.len())
        .map(|_| {
            (0..ALGORITHMS.len())
                .map(|_| {
                    Vec::with_capacity(SAMPLE_ROUNDS)
                })
                .collect()
        })
        .collect();

    for round in 0..SAMPLE_ROUNDS {
        let algorithm_order =
            ALGORITHM_ORDERS[round % ALGORITHM_ORDERS.len()];

        for size_offset in 0..INPUT_SIZES.len() {
            let size_index =
                (size_offset + round) % INPUT_SIZES.len();

            let input = &inputs[size_index];

            for algorithm_index in algorithm_order {
                let iterations =
                    batch_iterations[size_index]
                    [algorithm_index];

                let started = Instant::now();

                run_batch(
                    ALGORITHMS[algorithm_index],
                    input,
                    iterations,
                );

                let elapsed = started.elapsed();
                let total_bytes =
                    input.len() as f64 * iterations as f64;

                let nanoseconds_per_byte =
                    elapsed.as_secs_f64()
                    * 1_000_000_000.0
                    / total_bytes;

                samples[size_index][algorithm_index]
                    .push(nanoseconds_per_byte);
            }
        }
    }

    let mut results = [
        [Statistics::ZERO; ALGORITHMS.len()];
        INPUT_SIZES.len()
    ];

    for size_index in 0..INPUT_SIZES.len() {
        for algorithm_index in 0..ALGORITHMS.len() {
            results[size_index][algorithm_index] =
                summarize(
                    &mut samples[size_index]
                        [algorithm_index],
                );
        }
    }

    results
}

#[cfg(not(target_arch = "wasm32"))]
fn native_machine_metadata() -> MachineMetadata {
    use sysinfo::System;

    let mut system = System::new_all();
    system.refresh_all();

    let cpus = system.cpus();

    assert!(
        !cpus.is_empty(),
        "the operating system must report at least one CPU"
    );

    let cpu_type = cpus[0].brand().trim().to_owned();

    assert!(
        !cpu_type.is_empty(),
        "the operating system must report a CPU brand"
    );

    let kernel = System::kernel_version()
        .expect("the operating system must report a kernel version");

    let kernel_major = kernel
        .split('.')
        .next()
        .expect("kernel version must have a major component");

    assert!(
        kernel_major.chars().all(|character| {
            character.is_ascii_digit()
        }),
        "kernel major version must be numeric"
    );

    let os_name = match std::env::consts::OS {
        "macos" => "darwin",
        other => other,
    };

    MachineMetadata {
        timestamp: utc_timestamp(),
        cpu_type,
        cpu_count: cpus.len(),
        os_type: format!("{os_name}{kernel_major}"),
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub fn run_native() {
    let artifacts = run_benchmark(native_machine_metadata());

    print!("{}", artifacts.text);

    fs::write("bench-hashes.svg", artifacts.svg)
        .expect("bench-hashes.svg must be writable");

    println!("Graph saved to: bench-hashes.svg");
}

#[cfg(target_arch = "wasm32")]
fn browser_machine_metadata() -> MachineMetadata {
    let window = web_sys::window()
        .expect("the Wasm module must run in a browser Window");

    let navigator = window.navigator();

    let platform = navigator
        .platform()
        .expect("the browser must expose navigator.platform");

    let user_agent = navigator
        .user_agent()
        .expect("the browser must expose navigator.userAgent");

    let cpu_count = navigator.hardware_concurrency() as usize;

    assert!(
        cpu_count > 0,
        "the browser must report positive hardwareConcurrency"
    );

    let iso: String =
        js_sys::Date::new_0().to_iso_string().into();

    assert!(
        iso.len() >= 20 && iso.ends_with('Z'),
        "Date.toISOString() must return an ISO UTC timestamp"
    );

    MachineMetadata {
        timestamp: format!(
            "{} UTC",
            iso[..19].replace('T', " ")
        ),
        cpu_type: format!(
            "not exposed by browser; platform {platform}"
        ),
        cpu_count,
        os_type: user_agent,
    }
}

#[cfg(target_arch = "wasm32")]
#[wasm_bindgen(start)]
pub fn run_browser() {
    let artifacts =
        run_benchmark(browser_machine_metadata());

    let window = web_sys::window()
        .expect("the Wasm module must run in a browser Window");

    let document = window
        .document()
        .expect("the browser Window must have a Document");

    let body = document
        .body()
        .expect("the document must have a body");

    body.set_inner_html("");

    let container = document
        .create_element("main")
        .expect("main element creation must succeed");

    container
        .set_attribute(
            "style",
            concat!(
                "display:grid;",
                "grid-template-columns:",
                "repeat(auto-fit,minmax(460px,1fr));",
                "gap:24px;",
                "align-items:start;",
                "padding:20px;",
                "font-family:system-ui,sans-serif;",
            ),
        )
        .expect("main style assignment must succeed");

    let text = document
        .create_element("pre")
        .expect("pre element creation must succeed");

    text.set_text_content(Some(&artifacts.text));

    text.set_attribute(
        "style",
        concat!(
            "margin:0;",
            "padding:16px;",
            "overflow:auto;",
            "background:#f7f7f8;",
            "border:1px solid #ddd;",
            "border-radius:8px;",
            "font-size:12px;",
            "line-height:1.4;",
        ),
    )
        .expect("pre style assignment must succeed");

    let graph = document
        .create_element("section")
        .expect("section element creation must succeed");

    graph.set_inner_html(&artifacts.svg);

    graph
        .set_attribute(
            "style",
            concat!(
                "min-width:0;",
                "overflow:auto;",
                "border:1px solid #ddd;",
                "border-radius:8px;",
                "background:white;",
            ),
        )
        .expect("graph style assignment must succeed");

    container
        .append_child(&text)
        .expect("text insertion must succeed");

    container
        .append_child(&graph)
        .expect("graph insertion must succeed");

    body.append_child(&container)
        .expect("benchmark insertion must succeed");
}

use sha2::{Digest, Sha256};
use sha3::Sha3_256;
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
const ALGORITHM_COUNT: usize = 3;

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
const SHA3_SOURCE_INFO: &str = env!("SHA3_SOURCE_INFO");
const KECCAK_SOURCE_INFO: &str = env!("KECCAK_SOURCE_INFO");
const KECCAK_ASM_SOURCE_INFO: &str = env!("KECCAK_ASM_SOURCE_INFO");

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
    Algorithm::Sha3_256,
];

/*
 * SAMPLE_ROUNDS is divisible by six. Therefore every permutation appears
 * equally often and every algorithm occupies every ordering position equally
 * often.
 */
const ALGORITHM_ORDERS: [[usize; ALGORITHM_COUNT]; 6] = [
    [0, 1, 2],
    [0, 2, 1],
    [1, 0, 2],
    [1, 2, 0],
    [2, 0, 1],
    [2, 1, 0],
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

struct MachineMetadata {
    timestamp: String,
    cpu_type: String,
    cpu_count: usize,
    os_type: String,
}

#[derive(Clone, Copy)]
enum StatisticSeries {
    Minimum,
    Median,
    Maximum,
}

impl StatisticSeries {
    fn value(self, statistics: Statistics) -> f64 {
        match self {
            Self::Minimum => statistics.minimum,
            Self::Median => statistics.median,
            Self::Maximum => statistics.maximum,
        }
    }
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

    fs::write("bench-hashes.svg", svg)
        .expect("bench-hashes.svg must be writable");

    println!("Graph saved to: bench-hashes.svg");
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

fn generate_svg(
    results: &Results,
    machine: &MachineMetadata,
) -> String {
    const WIDTH: f64 = 1200.0;
    const HEIGHT: f64 = 790.0;
    const PLOT_LEFT: f64 = 105.0;
    const PLOT_RIGHT: f64 = 1125.0;
    const PLOT_TOP: f64 = 105.0;
    const PLOT_BOTTOM: f64 = 515.0;
    const TICK_COUNT: usize = 6;

    let observed_max = results
        .iter()
        .flatten()
        .map(|statistics| statistics.maximum)
        .fold(0.0_f64, f64::max);

    assert!(
        observed_max.is_finite() && observed_max > 0.0,
        "graph maximum must be finite and positive"
    );

    /*
     * Choose five intervals sufficient to contain the data, then reserve one
     * additional interval above it for labels and visual headroom.
     */
    let tick_step = nice_tick_step(
        observed_max / (TICK_COUNT - 1) as f64,
    );

    let axis_max = tick_step * TICK_COUNT as f64;

    let map_y = |value: f64| {
        PLOT_BOTTOM
            - value / axis_max * (PLOT_BOTTOM - PLOT_TOP)
    };

    let plot_width = PLOT_RIGHT - PLOT_LEFT;

    let x_positions: [f64; INPUT_COUNT] =
        std::array::from_fn(|index| {
            PLOT_LEFT
                + plot_width * index as f64
                / (INPUT_COUNT - 1) as f64
        });

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
        r#"  <text x="600" y="50" class="subtitle" text-anchor="middle">Solid: median · long dash: minimum · short dash: maximum · lower is better · linear scale</text>"#
    )
        .unwrap();

    writeln!(
        svg,
        r#"  <text x="25" y="310" class="axis-label" text-anchor="middle" transform="rotate(-90 25 310)">Nanoseconds per byte</text>"#
    )
        .unwrap();

    for tick_index in 0..=TICK_COUNT {
        let value = tick_step * tick_index as f64;

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

    for size_index in 0..INPUT_COUNT {
        let x = x_positions[size_index];

        writeln!(
            svg,
            r#"  <line x1="{x:.2}" y1="{PLOT_TOP:.1}" x2="{x:.2}" y2="{PLOT_BOTTOM:.1}" class="vertical-grid"/>"#
        )
            .unwrap();

        writeln!(
            svg,
            r#"  <text x="{x:.2}" y="540" class="size-label" text-anchor="middle">{}</text>"#,
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

    /*
     * Draw the connected minimum, maximum, and median series. All algorithms
     * use the same x coordinate for a given input size.
     */
    for algorithm_index in 0..ALGORITHM_COUNT {
        let algorithm = ALGORITHMS[algorithm_index];

        for (series, width, dash, opacity) in [
            (
                StatisticSeries::Minimum,
                1.5,
                "7,5",
                0.50,
            ),
            (
                StatisticSeries::Maximum,
                1.5,
                "2,4",
                0.50,
            ),
            (
                StatisticSeries::Median,
                3.0,
                "",
                0.88,
            ),
        ] {
            let mut path = String::new();

            for size_index in 0..INPUT_COUNT {
                let x = x_positions[size_index];

                let value =
                    series.value(results[size_index][algorithm_index]);

                let y = map_y(value);

                if size_index == 0 {
                    write!(path, "M {x:.2} {y:.2}").unwrap();
                } else {
                    write!(path, " L {x:.2} {y:.2}").unwrap();
                }
            }

            let dash_attribute = if dash.is_empty() {
                String::new()
            } else {
                format!(r#" stroke-dasharray="{dash}""#)
            };

            writeln!(
                svg,
                r#"  <path d="{path}" fill="none" stroke="{}" stroke-width="{width:.1}"{dash_attribute} stroke-linejoin="round" stroke-linecap="round" opacity="{opacity:.2}"/>"#,
                algorithm.color(),
            )
                .unwrap();
        }
    }

    /*
     * Draw ranges and median dots over the connecting lines.
     */
    for algorithm_index in 0..ALGORITHM_COUNT {
        let algorithm = ALGORITHMS[algorithm_index];

        for size_index in 0..INPUT_COUNT {
            let x = x_positions[size_index];
            let statistics = results[size_index][algorithm_index];

            let minimum_y = map_y(statistics.minimum);
            let median_y = map_y(statistics.median);
            let maximum_y = map_y(statistics.maximum);

            writeln!(
                svg,
                r#"  <line x1="{x:.2}" y1="{maximum_y:.2}" x2="{x:.2}" y2="{minimum_y:.2}" stroke="{}" stroke-width="4" stroke-linecap="round" opacity="0.25"/>"#,
                algorithm.color(),
            )
                .unwrap();

            for y in [minimum_y, maximum_y] {
                writeln!(
                    svg,
                    r#"  <line x1="{:.2}" y1="{y:.2}" x2="{:.2}" y2="{y:.2}" stroke="{}" stroke-width="2"/>"#,
                    x - 7.0,
                    x + 7.0,
                    algorithm.color(),
                )
                    .unwrap();
            }

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
                format_result_value(statistics.median),
                format_result_value(statistics.minimum),
                format_result_value(statistics.maximum),
            )
                .unwrap();

            svg.push_str("  </circle>\n");

            let label_x = match algorithm_index {
                0 => x - 10.0,
                1 => x + 10.0,
                2 => x + 10.0,
                _ => unreachable!(),
            };

            let anchor = if algorithm_index == 0 {
                "end"
            } else {
                "start"
            };

            writeln!(
                svg,
                r#"  <text x="{label_x:.2}" y="{:.2}" class="value-label" fill="{}" text-anchor="{anchor}">{}</text>"#,
                median_y - 8.0,
                algorithm.color(),
                format_result_value(statistics.median),
            )
                .unwrap();
        }
    }

    let legend_start_x = 365.0;

    for (index, algorithm) in ALGORITHMS.iter().enumerate() {
        let x = legend_start_x + index as f64 * 180.0;

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

    let implementation = detect_blake3_implementation();

    let metadata = [
        format!(
            "Timestamp: {} · BLAKE3 1 MiB backend: {}",
            machine.timestamp,
            blake3_backend_for_input(
                implementation,
                1024 * 1024,
            ),
        ),
        format!("Git source: {GIT_SOURCE}"),
        format!("Git commit: {GIT_COMMIT} · Tag: {GIT_TAG}"),
        format!(
            "Git clean status: {GIT_CLEAN_STATUS} · bench-hashes: {BENCH_VERSION}"
        ),
        format!(
            "CPU: {} · CPU count: {} · OS: {}",
            machine.cpu_type,
            machine.cpu_count,
            machine.os_type,
        ),
        format!(
            "Rust: {RUSTC_VERSION} · Target: {BUILD_TARGET}"
        ),
        format!(
            "Sources: {} · {} · {}",
            package_name_and_version(BLAKE3_SOURCE_INFO),
            package_name_and_version(SHA2_SOURCE_INFO),
            package_name_and_version(SHA3_SOURCE_INFO),
        ),
        format!(
            "Acceleration sources: {} · {} · {}",
            package_name_and_version(SHA2_ASM_SOURCE_INFO),
            package_name_and_version(KECCAK_SOURCE_INFO),
            package_name_and_version(KECCAK_ASM_SOURCE_INFO),
        ),
    ];

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
        ("SHA3-256 source", SHA3_SOURCE_INFO),
        ("Keccak source", KECCAK_SOURCE_INFO),
        ("Keccak assembly source", KECCAK_ASM_SOURCE_INFO),
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

    for (index, line) in metadata.iter().enumerate() {
        let y = 575.0 + index as f64 * 16.0;

        writeln!(
            svg,
            r#"  <text x="600" y="{y:.1}" class="metadata" text-anchor="middle">{}</text>"#,
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

fn nice_tick_step(minimum_step: f64) -> f64 {
    assert!(
        minimum_step.is_finite() && minimum_step > 0.0,
        "tick step must be finite and positive"
    );

    let magnitude =
        10.0_f64.powf(minimum_step.log10().floor());

    let normalized = minimum_step / magnitude;

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

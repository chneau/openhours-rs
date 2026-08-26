# openhours-rs

A high-performance, zero-allocation Rust parser and interval-math evaluator for OpenStreetMap [`opening_hours`](https://wiki.openstreetmap.org/wiki/Key:opening_hours) specifications.

[![Crates.io](https://img.shields.io/badge/crates.io-v1.0.0-orange?logo=rust)](https://crates.io/crates/openhours)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](https://opensource.org/licenses/MIT)

---

## ⚡ Features & Performance

- **$O(1)$ Hardware-Accelerated Bitmask**: Evaluates point-in-time checks (`is_open`) in **< 1 nanosecond** (>1 Billion ops/sec) via LLVM bit-parallel instructions.
- **Zero-Allocation Interval Math**: High-speed interval calculations with zero heap allocations on query paths (`is_open`, `get_time_to_open`, `get_time_to_open_for_duration`, `when`, `next_dur`, `next_date`).
- **Concurrent Lock-Free Interning**: Automatic thread-safe caching and deduplication of parsed instances (`DashMap`).
- **Overnight Shifts**: Full support for shifts spanning midnight (e.g. `Mo 22:00-04:00`, `Su 22:00-04:00`).
- **Overrides & Exclusions**: Handles `off` / `closed` rules overriding previous rules (e.g. `Mo-Su 00:00-24:00; Tu 12:00-13:00 off`).
- **Duration Availability**: Find wait times for continuous tasks of duration $D$ (`get_time_to_open_for_duration` / `when`).
- **Serde JSON Support**: Native `Serialize` and `Deserialize` implementations for seamless JSON serialization.

---

## 🚀 Quick Start

### Installation

#### 1. Via `Cargo.toml`

```toml
[dependencies]
openhours = { git = "https://github.com/chneau/openhours-rs" }
chrono = "0.4"
```

Or pinned to a release tag:

```toml
[dependencies]
openhours = { git = "https://github.com/chneau/openhours-rs", tag = "v1.0.0" }
chrono = "0.4"
```

#### 2. Via Cargo CLI

```bash
cargo add --git https://github.com/chneau/openhours-rs
cargo add chrono
```

---

### Usage Example

```rust
use chrono::{Duration, TimeZone, Utc};
use openhours::OpenHours;

fn main() {
    // 1. Parse an OSM opening_hours string
    let oh = OpenHours::parse("Mo-Fr 08:00-12:00, 13:00-17:00; Sa 08:00-12:00");

    let monday_10am = Utc.with_ymd_and_hms(2026, 5, 18, 10, 0, 0).unwrap();

    // 2. Fast point-in-time check (<1 ns/op)
    let is_open = oh.is_open(&monday_10am); // true

    // 3. Current shift end
    let shift_end = oh.get_current_shift_end(&monday_10am); // Some(2026-05-18 12:00:00 UTC)

    // 4. Time to next open
    let tuesday_lunch = Utc.with_ymd_and_hms(2026, 5, 19, 12, 30, 0).unwrap();
    let time_to_open = oh.get_time_to_open(&tuesday_lunch); // Some(30 minutes)

    // 5. Find when a 3-hour job can be serviced
    let wait_for_3h = oh.get_time_to_open_for_duration(&tuesday_lunch, Duration::hours(3));
    let when_can_start = oh.when(&tuesday_lunch, Duration::hours(3)); // Some(2026-05-19 13:00:00 UTC)

    // 6. Next state transitions
    let (is_open_now, remaining_duration) = oh.next_dur(&monday_10am);
    let (_, next_transition_date) = oh.next_date(&monday_10am); // 2026-05-18 12:00:00 UTC
}
```

---

## 📊 Benchmark Suite (Rust on AMD Ryzen 9)

| # | Workload | Calls | Latency / Op | Throughput |
| :--- | :--- | :--- | :--- | :--- |
| **1** | **`is_open` (Rolling timeline)** | 100,000 | **18.0 ns** | 55,000,000 ops/sec |
| **2** | **`is_open` (Pure call)** | 1,000,000 | **< 0.5 ns** | >1,000,000,000 ops/sec |
| **3** | **`get_time_to_open`** | 10,000 | **59.0 ns** | 17,000,000 ops/sec |
| **4** | **`get_time_to_open_for_duration` 4h** | 10,000 | **3.3 µs** | 300,000 ops/sec |
| **5** | **`when` 4h** | 10,000 | **3.0 µs** | 330,000 ops/sec |
| **6** | **`next_dur`** | 10,000 | **55.0 ns** | 18,000,000 ops/sec |
| **7** | **`next_date`** | 10,000 | **57.0 ns** | 17,500,000 ops/sec |
| **8** | **`parse` (Cached)** | 1,000 | **32.0 ns** | 31,000,000 ops/sec |
| **9** | **`JSON Deserialize`** | 1,000 | **126 ns** | 8,000,000 ops/sec |
| **10** | **Stress Test (5,000 unique objects)** | 5,000 | **0.3 µs/obj** | 3,300,000 objs/sec |

---

## 🛠️ Development & Testing

```bash
# Run unit tests
cargo test

# Run benchmark suite in release mode
cargo run --release --example benchmark

# Run release build
cargo build --release
```

---

## 📄 License

MIT License. Copyright (c) 2026 chneau.

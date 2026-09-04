use chrono::{Duration, TimeZone, Utc};
use openhours::OpenHours;
use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

struct CountingAllocator;

static ALLOCATED: AtomicUsize = AtomicUsize::new(0);

unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCATED.fetch_add(layout.size(), Ordering::Relaxed);
        System.alloc(layout)
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        System.dealloc(ptr, layout)
    }
}

#[global_allocator]
static A: CountingAllocator = CountingAllocator;

fn reset_alloc() -> usize {
    ALLOCATED.swap(0, Ordering::SeqCst)
}

fn get_alloc() -> usize {
    ALLOCATED.load(Ordering::SeqCst)
}

fn main() {
    println!("========================================================");
    println!("Running OpenHours Benchmarks (Rust Standard Suite)");
    println!("========================================================");

    let complex_expr = "Mo-Fr 08:00-12:00, 13:00-17:00; Sa 08:00-12:00";
    let oh = OpenHours::parse(complex_expr);
    let start = Utc.with_ymd_and_hms(2026, 5, 18, 0, 0, 0).unwrap();
    let fixed_time = Utc.with_ymd_and_hms(2026, 5, 18, 10, 0, 0).unwrap();
    let iterations = 10_000;
    let four_hours = Duration::hours(4);
    // Multiply iteration counts by this factor so benchmarks run longer and
    // yield more stable per-op timings across all workloads.
    let bench_scale = 10;

    // Warm-up
    let json_str = format!("\"{}\"", complex_expr);
    for i in 0..10_000 {
        oh.is_open(&(start + Duration::minutes(i % 168)));
        oh.get_time_to_open(&(start + Duration::hours(i % 168)));
        OpenHours::parse(complex_expr);
        let _: OpenHours = serde_json::from_str(&json_str).unwrap();
    }

    // 1. IsOpen (100k rolling calls)
    reset_alloc();
    let t0 = Instant::now();
    for i in 0..(iterations * 10 * bench_scale) {
        std::hint::black_box(oh.is_open(std::hint::black_box(&(start + Duration::minutes(i)))));
    }
    let d1 = t0.elapsed();
    let alloc1 = get_alloc() as f64 / (iterations * 10 * bench_scale) as f64;
    let d1_us = d1.as_secs_f64() * 1_000_000.0 / (iterations * 10 * bench_scale) as f64;
    println!(
        "1. IsOpen (100k rolling calls):            {:4} ms ({:.5} us/op, {:.1} B/op)",
        d1.as_millis(),
        d1_us,
        alloc1
    );

    // 2. IsOpen (1M pure calls)
    reset_alloc();
    let t0 = Instant::now();
    for _ in 0..(1_000_000 * bench_scale) {
        std::hint::black_box(oh.is_open(std::hint::black_box(&fixed_time)));
    }
    let d2 = t0.elapsed();
    let alloc2 = get_alloc() as f64 / (1_000_000 * bench_scale) as f64;
    let d2_us = d2.as_secs_f64() * 1_000_000.0 / (1_000_000 * bench_scale) as f64;
    println!(
        "2. IsOpen (1M pure calls):                 {:4} ms ({:.5} us/op, {:.1} B/op)",
        d2.as_millis(),
        d2_us,
        alloc2
    );

    // 3. GetTimeToOpen (10k calls)
    reset_alloc();
    let t0 = Instant::now();
    for i in 0..(iterations * bench_scale) {
        std::hint::black_box(oh.get_time_to_open(std::hint::black_box(&(start + Duration::hours(i % 168)))));
    }
    let d3 = t0.elapsed();
    let alloc3 = get_alloc() as f64 / (iterations * bench_scale) as f64;
    let d3_us = d3.as_secs_f64() * 1_000_000.0 / (iterations * bench_scale) as f64;
    println!(
        "3. GetTimeToOpen (10k calls):              {:4} ms ({:.5} us/op, {:.1} B/op)",
        d3.as_millis(),
        d3_us,
        alloc3
    );

    // 4. GetTimeToOpenForDuration 4h (10k calls)
    reset_alloc();
    let t0 = Instant::now();
    for i in 0..(iterations * bench_scale) {
        std::hint::black_box(oh.get_time_to_open_for_duration(std::hint::black_box(&(start + Duration::hours(i % 168))), std::hint::black_box(four_hours)));
    }
    let d4 = t0.elapsed();
    let alloc4 = get_alloc() as f64 / (iterations * bench_scale) as f64;
    let d4_us = d4.as_secs_f64() * 1_000_000.0 / (iterations * bench_scale) as f64;
    println!(
        "4. GetTimeToOpenForDuration 4h (10k calls):{:4} ms ({:.5} us/op, {:.1} B/op)",
        d4.as_millis(),
        d4_us,
        alloc4
    );

    // 5. When 4h (10k calls)
    reset_alloc();
    let t0 = Instant::now();
    for i in 0..(iterations * bench_scale) {
        std::hint::black_box(oh.when(std::hint::black_box(&(start + Duration::hours(i % 168))), std::hint::black_box(four_hours)));
    }
    let d5 = t0.elapsed();
    let alloc5 = get_alloc() as f64 / (iterations * bench_scale) as f64;
    let d5_us = d5.as_secs_f64() * 1_000_000.0 / (iterations * bench_scale) as f64;
    println!(
        "5. When 4h (10k calls):                    {:4} ms ({:.5} us/op, {:.1} B/op)",
        d5.as_millis(),
        d5_us,
        alloc5
    );

    // 6. NextDur (10k calls)
    reset_alloc();
    let t0 = Instant::now();
    for i in 0..(iterations * bench_scale) {
        std::hint::black_box(oh.next_dur(std::hint::black_box(&(start + Duration::hours(i % 168)))));
    }
    let d6 = t0.elapsed();
    let alloc6 = get_alloc() as f64 / (iterations * bench_scale) as f64;
    let d6_us = d6.as_secs_f64() * 1_000_000.0 / (iterations * bench_scale) as f64;
    println!(
        "6. NextDur (10k calls):                    {:4} ms ({:.5} us/op, {:.1} B/op)",
        d6.as_millis(),
        d6_us,
        alloc6
    );

    // 7. NextDate (10k calls)
    reset_alloc();
    let t0 = Instant::now();
    for i in 0..(iterations * bench_scale) {
        std::hint::black_box(oh.next_date(std::hint::black_box(&(start + Duration::hours(i % 168)))));
    }
    let d7 = t0.elapsed();
    let alloc7 = get_alloc() as f64 / (iterations * bench_scale) as f64;
    let d7_us = d7.as_secs_f64() * 1_000_000.0 / (iterations * bench_scale) as f64;
    println!(
        "7. NextDate (10k calls):                   {:4} ms ({:.5} us/op, {:.1} B/op)",
        d7.as_millis(),
        d7_us,
        alloc7
    );

    // 8. Parse Cached (1k calls)
    reset_alloc();
    let t0 = Instant::now();
    for _ in 0..(1_000 * bench_scale) {
        std::hint::black_box(OpenHours::parse(std::hint::black_box(complex_expr)));
    }
    let d8 = t0.elapsed();
    let alloc8 = get_alloc() as f64 / (1_000 * bench_scale) as f64;
    let d8_us = d8.as_secs_f64() * 1_000_000.0 / (1_000 * bench_scale) as f64;
    println!(
        "8. Parse Cached (1k calls):                {:4} ms ({:.5} us/op, {:.1} B/op)",
        d8.as_millis(),
        d8_us,
        alloc8
    );

    // 9. JSON Deserialize (1k calls)
    reset_alloc();
    let t0 = Instant::now();
    for _ in 0..(1_000 * bench_scale) {
        let obj: OpenHours = serde_json::from_str(std::hint::black_box(&json_str)).unwrap();
        std::hint::black_box(obj);
    }
    let d9 = t0.elapsed();
    let alloc9 = get_alloc() as f64 / (1_000 * bench_scale) as f64;
    let d9_us = d9.as_secs_f64() * 1_000_000.0 / (1_000 * bench_scale) as f64;
    println!(
        "9. JSON Deserialize (1k calls):            {:4} ms ({:.5} us/op, {:.1} B/op)",
        d9.as_millis(),
        d9_us,
        alloc9
    );

    // 10. Stress Test (5,000 unique objects)
    reset_alloc();
    let t0 = Instant::now();
    let stress_count: usize = 5_000 * bench_scale as usize;
    let mut locations = Vec::with_capacity(stress_count);
    for i in 0..stress_count {
        let h_start = 8 + (i % 60) / 60;
        let m_start = i % 60;
        let h_end = 17 + (i % 60) / 60;
        let m_end = i % 60;
        let expr = format!("Mo-Fr {:02}:{:02}-{:02}:{:02}", h_start, m_start, h_end, m_end);
        locations.push(OpenHours::parse(&expr));
    }
    let d10 = t0.elapsed();
    let alloc10 = get_alloc() as f64 / stress_count as f64;
    let d10_ms_obj = d10.as_secs_f64() * 1_000.0 / stress_count as f64;
    println!(
        "10. Stress Test (5,000 unique objects):    {:4} ms ({:.4} ms/obj, {:.1} B/obj)",
        d10.as_millis(),
        d10_ms_obj,
        alloc10
    );
    println!("========================================================");
}

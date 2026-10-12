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

const TARGET_MS: f64 = 500.0;

fn reset_alloc() -> usize {
    ALLOCATED.swap(0, Ordering::SeqCst)
}

fn get_alloc() -> usize {
    ALLOCATED.load(Ordering::SeqCst)
}

/// Runs op(0..base) repeatedly until a batch runs for at least TARGET_MS, then
/// keeps taking batches until the measured region reaches TARGET_MS. Returns
/// (measured ms, bytes allocated in the pass, total op count).
fn measure<F: FnMut(usize)>(base: usize, mut op: F) -> (f64, usize, u64) {
    let mut reps: u64 = 1;
    loop {
        let t0 = Instant::now();
        for _ in 0..reps {
            for i in 0..base {
                op(i);
            }
        }
        let el = t0.elapsed().as_secs_f64() * 1000.0;
        if el >= TARGET_MS || reps > (1u64 << 34) / base as u64 {
            break;
        }
        let mut next = (reps as f64 * TARGET_MS / el) as u64 + 1;
        if next <= reps {
            next = reps + 1;
        }
        reps = next;
    }

    reset_alloc();
    let t0 = Instant::now();
    let mut total: u64 = 0;
    loop {
        for _ in 0..reps {
            for i in 0..base {
                op(i);
            }
        }
        total += reps * base as u64;
        if t0.elapsed().as_secs_f64() * 1000.0 >= TARGET_MS {
            break;
        }
    }
    let ms = t0.elapsed().as_secs_f64() * 1000.0;
    (ms, get_alloc(), total)
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
    // Base workload sizes; each step is repeated until it runs for TARGET_MS.
    let bench_scale = 10;

    // Warm-up
    let json_str = format!("\"{}\"", complex_expr);
    for i in 0..10_000 {
        oh.is_open(&(start + Duration::minutes(i % 168)));
        oh.get_time_to_open(&(start + Duration::hours(i % 168)));
        OpenHours::parse(complex_expr);
        let _: OpenHours = serde_json::from_str(&json_str).unwrap();
    }

    // Precalculate timelines up-front matching Go and C# so that chrono's DateTime addition
    // is not measured inside the algorithm benchmark loops.
    let rolling_count = iterations * 10 * bench_scale;
    let mut rolling_timeline = Vec::with_capacity(rolling_count);
    for i in 0..rolling_count {
        rolling_timeline.push(start + Duration::minutes(i as i64));
    }

    let mut hour_timeline = Vec::with_capacity(168);
    for i in 0..168 {
        hour_timeline.push(start + Duration::hours(i as i64));
    }

    // 1. IsOpen (100k rolling calls)
    let (ms1, a1, n1) = measure(rolling_count, |i| {
        std::hint::black_box(oh.is_open(std::hint::black_box(&rolling_timeline[i])));
    });
    println!(
        "1. IsOpen (100k rolling calls):            {:4} ms ({:.5} us/op, {:.1} B/op)",
        ms1.round() as u64,
        ms1 * 1000.0 / n1 as f64,
        a1 as f64 / n1 as f64
    );

    // 2. IsOpen (1M pure calls)
    let (ms2, a2, n2) = measure(1_000_000 * bench_scale, |_| {
        std::hint::black_box(oh.is_open(std::hint::black_box(&fixed_time)));
    });
    println!(
        "2. IsOpen (1M pure calls):                 {:4} ms ({:.5} us/op, {:.1} B/op)",
        ms2.round() as u64,
        ms2 * 1000.0 / n2 as f64,
        a2 as f64 / n2 as f64
    );

    // 3. GetTimeToOpen (10k calls)
    let (ms3, a3, n3) = measure(iterations * bench_scale, |i| {
        std::hint::black_box(oh.get_time_to_open(std::hint::black_box(&hour_timeline[i % 168])));
    });
    println!(
        "3. GetTimeToOpen (10k calls):              {:4} ms ({:.5} us/op, {:.1} B/op)",
        ms3.round() as u64,
        ms3 * 1000.0 / n3 as f64,
        a3 as f64 / n3 as f64
    );

    // 4. GetTimeToOpenForDuration 4h (10k calls)
    let (ms4, a4, n4) = measure(iterations * bench_scale, |i| {
        std::hint::black_box(oh.get_time_to_open_for_duration(
            std::hint::black_box(&hour_timeline[i % 168]),
            std::hint::black_box(four_hours),
        ));
    });
    println!(
        "4. GetTimeToOpenForDuration 4h (10k calls):{:4} ms ({:.5} us/op, {:.1} B/op)",
        ms4.round() as u64,
        ms4 * 1000.0 / n4 as f64,
        a4 as f64 / n4 as f64
    );

    // 5. When 4h (10k calls)
    let (ms5, a5, n5) = measure(iterations * bench_scale, |i| {
        std::hint::black_box(oh.when(
            std::hint::black_box(&hour_timeline[i % 168]),
            std::hint::black_box(four_hours),
        ));
    });
    println!(
        "5. When 4h (10k calls):                    {:4} ms ({:.5} us/op, {:.1} B/op)",
        ms5.round() as u64,
        ms5 * 1000.0 / n5 as f64,
        a5 as f64 / n5 as f64
    );

    // 6. NextDur (10k calls)
    let (ms6, a6, n6) = measure(iterations * bench_scale, |i| {
        std::hint::black_box(oh.next_dur(std::hint::black_box(&hour_timeline[i % 168])));
    });
    println!(
        "6. NextDur (10k calls):                    {:4} ms ({:.5} us/op, {:.1} B/op)",
        ms6.round() as u64,
        ms6 * 1000.0 / n6 as f64,
        a6 as f64 / n6 as f64
    );

    // 7. NextDate (10k calls)
    let (ms7, a7, n7) = measure(iterations * bench_scale, |i| {
        std::hint::black_box(oh.next_date(std::hint::black_box(&hour_timeline[i % 168])));
    });
    println!(
        "7. NextDate (10k calls):                   {:4} ms ({:.5} us/op, {:.1} B/op)",
        ms7.round() as u64,
        ms7 * 1000.0 / n7 as f64,
        a7 as f64 / n7 as f64
    );

    // 8. Parse Cached (1k calls)
    let (ms8, a8, n8) = measure(1_000 * bench_scale, |_| {
        std::hint::black_box(OpenHours::parse(std::hint::black_box(complex_expr)));
    });
    println!(
        "8. Parse Cached (1k calls):                {:4} ms ({:.5} us/op, {:.1} B/op)",
        ms8.round() as u64,
        ms8 * 1000.0 / n8 as f64,
        a8 as f64 / n8 as f64
    );

    // 9. JSON Deserialize (1k calls)
    let (ms9, a9, n9) = measure(1_000 * bench_scale, |_| {
        let obj: OpenHours = serde_json::from_str(std::hint::black_box(&json_str)).unwrap();
        std::hint::black_box(obj);
    });
    println!(
        "9. JSON Deserialize (1k calls):            {:4} ms ({:.5} us/op, {:.1} B/op)",
        ms9.round() as u64,
        ms9 * 1000.0 / n9 as f64,
        a9 as f64 / n9 as f64
    );

    // 10. Stress Test (5,000 unique objects)
    let (ms10, a10, n10) = measure(5_000 * bench_scale, |i| {
        let h_start = 8 + (i % 60) / 60;
        let m_start = i % 60;
        let h_end = 17 + (i % 60) / 60;
        let m_end = i % 60;
        let expr = format!("Mo-Fr {:02}:{:02}-{:02}:{:02}", h_start, m_start, h_end, m_end);
        std::hint::black_box(OpenHours::parse(std::hint::black_box(&expr)));
    });
    println!(
        "10. Stress Test (5,000 unique objects):    {:4} ms ({:.4} ms/obj, {:.1} B/obj)",
        ms10.round() as u64,
        ms10 / n10 as f64,
        a10 as f64 / n10 as f64
    );
    println!("========================================================");
}

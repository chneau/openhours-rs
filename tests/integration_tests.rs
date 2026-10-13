use chrono::{DateTime, Duration, TimeZone, Utc};
use openhours::{OpenHours, TimeWindow, MINUTES_PER_WEEK};
use std::thread;

fn monday_midnight() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 5, 18, 0, 0, 0).unwrap()
}

#[test]
fn test_is_open_table_driven() {
    let base = monday_midnight();
    let cases = vec![
        // Basic daily range
        ("Mo-Fr 08:00-17:00", base + Duration::hours(10), true),
        ("Mo-Fr 08:00-17:00", base + Duration::hours(7) + Duration::minutes(59), false),
        ("Mo-Fr 08:00-17:00", base + Duration::hours(17), false),
        ("Mo-Fr 08:00-17:00", base + Duration::days(5) + Duration::hours(10), false), // Saturday

        // Multiple time intervals
        ("Mo-Fr 08:00-12:00, 13:00-17:00", base + Duration::hours(12) + Duration::minutes(30), false),
        ("Mo-Fr 08:00-12:00, 13:00-17:00", base + Duration::hours(14), true),

        // Multiple rules
        ("Mo-Fr 08:00-17:00; Sa 08:00-12:00", base + Duration::days(5) + Duration::hours(10), true),
        ("Mo-Fr 08:00-17:00; Sa 08:00-12:00", base + Duration::days(5) + Duration::hours(14), false),

        // Off modifier
        ("Mo-Su 00:00-24:00; Tu 12:00-13:00 off", base + Duration::days(1) + Duration::hours(12) + Duration::minutes(30), false),
        ("Mo-Su 00:00-24:00; Tu 12:00-13:00 off", base + Duration::days(1) + Duration::hours(14), true),
        ("Mo-Su 00:00-24:00; Tu 12:00-13:00 closed", base + Duration::days(1) + Duration::hours(12) + Duration::minutes(30), false),

        // 24/7
        ("24/7", base + Duration::days(6) + Duration::hours(23) + Duration::minutes(59), true),

        // Day range wrap-around
        ("Sa-Su 08:00-12:00", base + Duration::days(6) + Duration::hours(10), true),
        ("Sa-Su 08:00-12:00", base + Duration::hours(10), false),

        // Open ended
        ("Mo 10:00+", base + Duration::hours(22), true),
        ("Mo 10:00+", base + Duration::hours(9), false),

        // No days (every day)
        ("10:00-12:00", base + Duration::hours(11), true),
        ("10:00-12:00", base + Duration::days(5) + Duration::hours(11), true),

        // 00:00-24:00 (every day 24h)
        ("00:00-24:00", base, true),
        ("00:00-24:00", base + Duration::hours(12), true),

        // Day only without time range defaults to 24h
        ("Mo", base, true),
        ("Mo", base + Duration::hours(12), true),
        ("Mo", base + Duration::days(1) + Duration::hours(10), false),
        ("Mo-Fr", base + Duration::hours(10), true),
        ("Mo-Fr", base + Duration::days(5) + Duration::hours(10), false),

        // Mid-week overnight shift
        ("Mo 22:00-04:00", base + Duration::hours(21) + Duration::minutes(59), false),
        ("Mo 22:00-04:00", base + Duration::hours(22), true),
        ("Mo 22:00-04:00", base + Duration::hours(23) + Duration::minutes(30), true),
        ("Mo 22:00-04:00", base + Duration::days(1) + Duration::minutes(30), true),
        ("Mo 22:00-04:00", base + Duration::days(1) + Duration::hours(3) + Duration::minutes(59), true),
        ("Mo 22:00-04:00", base + Duration::days(1) + Duration::hours(4), false),

        // Week wrap-around overnight shift (Sunday 22:00 to Monday 04:00)
        ("Su 22:00-04:00", base + Duration::days(6) + Duration::hours(21) + Duration::minutes(59), false),
        ("Su 22:00-04:00", base + Duration::days(6) + Duration::hours(22), true),
        ("Su 22:00-04:00", base + Duration::minutes(1), true),
        ("Su 22:00-04:00", base + Duration::hours(3) + Duration::minutes(59), true),
        ("Su 22:00-04:00", base + Duration::hours(4), false),

        // Invalid inputs
        ("invalid", base + Duration::hours(10), false),
        ("Mo invalid", base + Duration::hours(10), false),
        ("Mo 25:00-26:00", base + Duration::hours(10), false),
        ("Xx 08:00-17:00", base + Duration::hours(10), false),
        ("", base + Duration::hours(10), false),
        ("   ", base + Duration::hours(10), false),
    ];

    for (expr, dt, expected) in cases {
        let oh = OpenHours::parse(expr);
        assert_eq!(
            oh.is_open(&dt),
            expected,
            "Expression '{}' at {:?} failed",
            expr,
            dt
        );
        assert_eq!(
            oh.match_time(&dt),
            expected,
            "Match time '{}' at {:?} failed",
            expr,
            dt
        );
    }
}

#[test]
fn test_get_current_shift_end_table_driven() {
    let base = monday_midnight();
    let cases = vec![
        (
            "Mo-Fr 08:00-17:00",
            base + Duration::hours(10),
            Some(base + Duration::hours(17)),
        ),
        (
            "Mo-Fr 08:00-17:00",
            base + Duration::hours(17) + Duration::minutes(30),
            None,
        ),
        (
            "Mo 22:00-04:00",
            base + Duration::hours(23),
            Some(base + Duration::days(1) + Duration::hours(4)),
        ),
        (
            "Mo 22:00-04:00",
            base + Duration::days(1) + Duration::hours(2),
            Some(base + Duration::days(1) + Duration::hours(4)),
        ),
        (
            "Su 22:00-04:00",
            base + Duration::days(6) + Duration::hours(23),
            Some(base + Duration::days(7) + Duration::hours(4)),
        ),
        (
            "Su 22:00-04:00",
            base + Duration::hours(2),
            Some(base + Duration::hours(4)),
        ),
    ];

    for (expr, dt, expected) in cases {
        let oh = OpenHours::parse(expr);
        assert_eq!(
            oh.get_current_shift_end(&dt),
            expected,
            "get_current_shift_end on '{}' at {:?}",
            expr,
            dt
        );
    }
}

#[test]
fn test_get_time_to_open_table_driven() {
    let base = monday_midnight();
    let cases = vec![
        (
            "Mo-Fr 08:00-17:00",
            base + Duration::hours(6),
            Some(Duration::hours(2)),
        ),
        (
            "Mo-Fr 08:00-17:00",
            base + Duration::hours(10),
            Some(Duration::zero()),
        ),
        (
            "Mo-Fr 08:00-12:00, 13:00-17:00",
            base + Duration::hours(12) + Duration::minutes(30),
            Some(Duration::minutes(30)),
        ),
        (
            "Mo 22:00-04:00",
            base + Duration::hours(20),
            Some(Duration::hours(2)),
        ),
        (
            "Su 22:00-04:00",
            base + Duration::days(6) + Duration::hours(20),
            Some(Duration::hours(2)),
        ),
        ("24/7", base + Duration::hours(10), Some(Duration::zero())),
        ("", base + Duration::hours(10), None),
    ];

    for (expr, dt, expected) in cases {
        let oh = OpenHours::parse(expr);
        assert_eq!(
            oh.get_time_to_open(&dt),
            expected,
            "get_time_to_open on '{}' at {:?}",
            expr,
            dt
        );
    }
}

#[test]
fn test_get_time_to_open_for_duration_and_when() {
    let base = monday_midnight();
    let oh = OpenHours::parse("Mo-Fr 08:00-12:00, 13:00-17:00; Sa 08:00-12:00");

    // Need 3 hours starting at Monday 11:00 (11-12 is only 1h, next slot is 13:00-17:00 which has 4h)
    // Wait time: 2 hours (11:00 -> 13:00)
    let req = Duration::hours(3);
    let wait = oh.get_time_to_open_for_duration(&(base + Duration::hours(11)), req);
    assert_eq!(wait, Some(Duration::hours(2)));

    let when = oh.when(&(base + Duration::hours(11)), req);
    assert_eq!(when, Some(base + Duration::hours(13)));

    // Requesting duration longer than week returns None
    assert_eq!(
        oh.get_time_to_open_for_duration(&base, Duration::days(8)),
        None
    );
}

#[test]
fn test_next_dur_and_next_date_table_driven() {
    let base = monday_midnight();
    let oh = OpenHours::parse("Mo 08:00-18:00");

    // Inside open shift
    let (is_open, dur) = oh.next_dur(&(base + Duration::hours(10)));
    assert!(is_open);
    assert_eq!(dur, Duration::hours(8));

    let (is_open_date, date) = oh.next_date(&(base + Duration::hours(10)));
    assert!(is_open_date);
    assert_eq!(date, base + Duration::hours(18));

    // Outside shift (closed)
    let (is_open2, dur2) = oh.next_dur(&(base + Duration::hours(6)));
    assert!(!is_open2);
    assert_eq!(dur2, Duration::hours(2));

    let (is_open_date2, date2) = oh.next_date(&(base + Duration::hours(6)));
    assert!(!is_open_date2);
    assert_eq!(date2, base + Duration::hours(8));
}

#[test]
fn test_windows_disjoint_integrity() {
    let oh = OpenHours::parse("Mo 08:00-12:00, 13:00-17:00; Tu 08:00-12:00");
    let wins = oh.windows();
    assert_eq!(wins.len(), 3);
    assert_eq!(wins[0].start, 8 * 60);
    assert_eq!(wins[0].end, 12 * 60);
    assert_eq!(wins[1].start, 13 * 60);
    assert_eq!(wins[1].end, 17 * 60);
    assert_eq!(wins[2].start, 1440 + 8 * 60);
    assert_eq!(wins[2].end, 1440 + 12 * 60);
}

#[test]
fn test_concurrent_multithreaded_evaluations() {
    let expr = "Mo-Fr 08:00-12:00, 13:00-17:00; Sa 08:00-12:00";
    let base = monday_midnight();

    let mut handles = Vec::new();
    for thread_idx in 0..8 {
        let handle = thread::spawn(move || {
            let oh = OpenHours::parse(expr);
            for i in 0..10_000 {
                let dt = base + Duration::minutes((thread_idx * 1000 + i) % 10080);
                let _ = oh.is_open(&dt);
                let _ = oh.get_time_to_open(&dt);
            }
        });
        handles.push(handle);
    }

    for h in handles {
        h.join().unwrap();
    }
}

#[test]
fn test_sub_minute_precision() {
    let base = monday_midnight();
    let oh = OpenHours::parse("Mo 08:00-17:00");

    // 30 seconds until open.
    let near_open = base + Duration::hours(7) + Duration::minutes(59) + Duration::seconds(30);
    let expected = Duration::seconds(30);
    let wait = oh.get_time_to_open(&near_open).unwrap();
    assert_eq!(wait, expected);

    // 30 seconds until close while open.
    let near_close = base + Duration::hours(16) + Duration::minutes(59) + Duration::seconds(30);
    let (is_open, dur) = oh.next_dur(&near_close);
    assert!(is_open);
    assert_eq!(dur, Duration::seconds(30));

    // next_date truncates back exactly to 17:00.
    let (is_open_next, next_date) = oh.next_date(&near_close);
    assert!(is_open_next);
    assert_eq!(next_date, base + Duration::hours(17));
}

#[test]
fn test_advanced_day_syntax() {
    let base = monday_midnight();

    // Comma day list.
    let list = OpenHours::parse("Mo, Tu, We 08:00-12:00");
    assert!(list.is_open(&(base + Duration::hours(10))));
    assert!(list.is_open(&(base + Duration::hours(24 + 10))));
    assert!(list.is_open(&(base + Duration::hours(48 + 10))));
    assert!(!list.is_open(&(base + Duration::hours(72 + 10)))); // Thursday

    // Full-name aliases.
    let full = OpenHours::parse("Monday-Friday 08:00-17:00");
    assert!(full.is_open(&(base + Duration::hours(10))));
    assert!(!full.is_open(&(base + Duration::days(5) + Duration::hours(10))));

    // Combined range + list.
    let combined = OpenHours::parse("Mo-We, Fr 08:00-17:00");
    assert!(combined.is_open(&(base + Duration::days(2) + Duration::hours(10)))); // Wed
    assert!(!combined.is_open(&(base + Duration::days(3) + Duration::hours(10)))); // Thu
    assert!(combined.is_open(&(base + Duration::days(4) + Duration::hours(10)))); // Fri

    // 00:00-00:00 all-day on the selected day.
    let midnight = OpenHours::parse("Mo 00:00-00:00");
    assert!(midnight.is_open(&(base + Duration::hours(15))));
    assert!(!midnight.is_open(&(base + Duration::days(1) + Duration::hours(15))));

    // Day-only defaults to 24h on those days.
    let mo = OpenHours::parse("Mo");
    assert!(mo.is_open(&base));
    assert!(mo.is_open(&(base + Duration::hours(23) + Duration::minutes(59))));
    assert!(!mo.is_open(&(base + Duration::hours(24))));
}

// ---------------------------------------------------------------------------
// Differential test: the lazy per-minute `succ`/`widx` tables must reproduce,
// bit for bit, the lower-bound window search they replaced. The reference
// functions below are a faithful copy of the pre-optimization algorithm,
// derived only from `windows()`.
// ---------------------------------------------------------------------------

fn ref_first(windows: &[TimeWindow], t: usize) -> usize {
    let mut i = 0;
    while i < windows.len() && (windows[i].end as usize) <= t {
        i += 1;
    }
    i
}

/// Copy of `OpenHours::duration_minus_sub`.
fn ref_dur(diff_min: usize, sub_seconds: i64, sub_nanos: i64) -> Duration {
    let secs = diff_min as i64 * 60 - sub_seconds;
    if sub_nanos == 0 {
        Duration::seconds(secs)
    } else {
        Duration::new(secs - 1, (1_000_000_000 - sub_nanos) as u32).unwrap()
    }
}

fn ref_time_to_open(oh: &OpenHours, t: usize, ss: i64, sn: i64) -> Option<Duration> {
    let w = oh.windows();
    if w.is_empty() {
        return None;
    }
    if oh.is_always_open() {
        return Some(Duration::zero());
    }
    let n = w.len();
    let idx = ref_first(w, t);
    let diff = if idx < n {
        if (w[idx].start as usize) <= t {
            return Some(Duration::zero());
        }
        (w[idx].start as usize) - t
    } else {
        (MINUTES_PER_WEEK - t) + (w[0].start as usize)
    };
    Some(ref_dur(diff, ss, sn))
}

fn ref_next_dur(oh: &OpenHours, t: usize, ss: i64, sn: i64) -> (bool, Duration) {
    let w = oh.windows();
    if w.is_empty() {
        return (false, Duration::zero());
    }
    if oh.is_always_open() {
        return (true, Duration::days(365));
    }
    let n = w.len();
    let idx = ref_first(w, t);
    if idx < n {
        if (w[idx].start as usize) <= t {
            let mut diff = (w[idx].end as usize) - t;
            if idx == n - 1 && w[idx].end as usize == MINUTES_PER_WEEK && w[0].start == 0 {
                diff = (MINUTES_PER_WEEK - t) + (w[0].end as usize);
            }
            return (true, ref_dur(diff, ss, sn));
        }
        return (false, ref_dur((w[idx].start as usize) - t, ss, sn));
    }
    (false, ref_dur((MINUTES_PER_WEEK - t) + (w[0].start as usize), ss, sn))
}

fn ref_current_shift_end(oh: &OpenHours, t: usize, ss: i64, sn: i64) -> Option<Duration> {
    let w = oh.windows();
    if w.is_empty() {
        return None;
    }
    if oh.is_always_open() {
        return Some(Duration::weeks(52));
    }
    let n = w.len();
    let idx = ref_first(w, t);
    if idx >= n || (w[idx].start as usize) > t {
        return None;
    }
    let mut diff = (w[idx].end as usize) - t;
    if idx == n - 1 && w[idx].end as usize == MINUTES_PER_WEEK && w[0].start == 0 {
        diff = (MINUTES_PER_WEEK - t) + (w[0].end as usize);
    }
    Some(ref_dur(diff, ss, sn))
}

fn ref_time_to_open_for_duration(
    oh: &OpenHours,
    t: usize,
    ss: i64,
    sn: i64,
    required: Duration,
) -> Option<Duration> {
    let w = oh.windows();
    if w.is_empty() {
        return None;
    }
    let req_nanos = required.num_nanoseconds().unwrap_or(0);
    if req_nanos <= 0 {
        return Some(Duration::zero());
    }
    let req_minutes = ((required.num_seconds() + 59) / 60) as usize;
    if req_minutes > MINUTES_PER_WEEK {
        return None;
    }
    if oh.is_always_open() {
        return Some(Duration::zero());
    }
    let sub_dur_nanos = ss * 1_000_000_000 + sn;
    let n = w.len();
    let start_idx = ref_first(w, t);
    let last_ends = w[n - 1].end as usize == MINUTES_PER_WEEK;
    let first_zero = w[0].start == 0;
    for i in start_idx..n {
        let effective_end = if i == n - 1 && last_ends && first_zero {
            MINUTES_PER_WEEK + (w[0].end as usize)
        } else {
            w[i].end as usize
        };
        if t >= (w[i].start as usize) {
            let rem_nanos = ((effective_end - t) as i64) * 60_000_000_000 - sub_dur_nanos;
            if rem_nanos >= req_nanos {
                return Some(Duration::zero());
            }
        } else if effective_end - (w[i].start as usize) >= req_minutes {
            return Some(ref_dur((w[i].start as usize) - t, ss, sn));
        }
    }
    for i in 0..n {
        let effective_end = if i == n - 1 && last_ends && first_zero {
            MINUTES_PER_WEEK + (w[0].end as usize)
        } else {
            w[i].end as usize
        };
        if effective_end - (w[i].start as usize) >= req_minutes {
            return Some(ref_dur((MINUTES_PER_WEEK - t) + (w[i].start as usize), ss, sn));
        }
    }
    None
}

#[test]
fn test_per_minute_tables_match_reference_search() {
    let base = monday_midnight();
    let expressions = [
        "Mo-Fr 08:00-12:00, 13:00-17:00; Sa 08:00-12:00",
        "Mo 08:00-18:00",
        "Mo 22:00-04:00",
        "Su 22:00-04:00",
        "Mo-Su 00:00-24:00; Tu 12:00-13:00 off",
        "Mo-Su 08:00-16:00",
        "Sa-Su 10:00-02:00",
        "Mo-Tu,Th,Fr 06:00-22:00",
        "24/7",
    ];
    // (sub_seconds, sub_nanos) query instants within each minute.
    let subs: [(i64, i64); 4] = [(0, 0), (30, 0), (0, 500_000_000), (59, 999_999_999)];
    let reqs = [
        Duration::nanoseconds(1),
        Duration::minutes(90),
        Duration::hours(4),
        Duration::weeks(1),
    ];

    for expr in expressions {
        let oh = OpenHours::parse(expr);
        for minute in 0..MINUTES_PER_WEEK {
            for (ss, sn) in subs {
                let dt = base
                    + Duration::minutes(minute as i64)
                    + Duration::seconds(ss)
                    + Duration::nanoseconds(sn);

                assert_eq!(
                    oh.get_time_to_open(&dt),
                    ref_time_to_open(&oh, minute, ss, sn),
                    "get_time_to_open {expr} m={minute} ss={ss} sn={sn}"
                );
                assert_eq!(
                    oh.next_dur(&dt),
                    ref_next_dur(&oh, minute, ss, sn),
                    "next_dur {expr} m={minute} ss={ss} sn={sn}"
                );
                let ref_shift = ref_current_shift_end(&oh, minute, ss, sn);
                let got_shift = oh.get_current_shift_end(&dt).map(|d| d - dt.clone());
                assert_eq!(got_shift, ref_shift, "shift_end {expr} m={minute} ss={ss} sn={sn}");

                // next_date / when built from the reference duration.
                let rnd = ref_next_dur(&oh, minute, ss, sn);
                let expected_date = if rnd.1.is_zero() {
                    dt.clone()
                } else {
                    dt.clone() + rnd.1
                };
                assert_eq!(
                    oh.next_date(&dt),
                    (rnd.0, expected_date),
                    "next_date {expr} m={minute} ss={ss} sn={sn}"
                );

                for req in reqs {
                    let r = ref_time_to_open_for_duration(&oh, minute, ss, sn, req);
                    assert_eq!(
                        oh.get_time_to_open_for_duration(&dt, req),
                        r,
                        "dur {expr} m={minute} ss={ss} sn={sn} req={req:?}"
                    );
                    let expected_when = r.map(|w| if w.is_zero() { dt.clone() } else { dt.clone() + w });
                    assert_eq!(
                        oh.when(&dt, req),
                        expected_when,
                        "when {expr} m={minute} ss={ss} sn={sn} req={req:?}"
                    );
                }
            }
        }
    }
}

use chrono::{DateTime, Duration, TimeZone, Utc};
use openhours::OpenHours;
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

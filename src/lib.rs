use chrono::{Datelike, DateTime, Duration, NaiveDateTime, Offset, TimeZone, Timelike, Weekday};
use parking_lot::RwLock;
use rustc_hash::FxHashMap;
use serde::de::{self, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::cell::RefCell;
use std::fmt;
use std::str::FromStr;
use std::sync::{Arc, LazyLock};

pub const MINUTES_PER_WEEK: usize = 7 * 24 * 60; // 10,080
pub const BITMASK_WORDS: usize = (MINUTES_PER_WEEK + 63) / 64; // 158

static INTERN_POOL: LazyLock<RwLock<FxHashMap<String, Arc<OpenHours>>>> =
    LazyLock::new(|| RwLock::new(FxHashMap::default()));

static ALWAYS_OPEN: LazyLock<Arc<OpenHours>> = LazyLock::new(|| {
    let mut bm = [!0u64; BITMASK_WORDS];
    let rem = MINUTES_PER_WEEK % 64;
    if rem != 0 {
        bm[BITMASK_WORDS - 1] = (1u64 << rem) - 1;
    }
    Arc::new(OpenHours {
        raw: "24/7".to_string(),
        windows: vec![TimeWindow {
            start: 0,
            end: MINUTES_PER_WEEK as u16,
        }],
        bitmask: bm,
    })
});

static EMPTY: LazyLock<Arc<OpenHours>> = LazyLock::new(|| {
    Arc::new(OpenHours {
        raw: String::new(),
        windows: Vec::new(),
        bitmask: [0u64; BITMASK_WORDS],
    })
});

thread_local! {
    static LAST_HASH: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
    static LAST_VAL: RefCell<Option<Arc<OpenHours>>> = const { RefCell::new(None) };
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct TimeWindow {
    pub start: u16,
    pub end: u16,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[repr(align(64))]
pub struct OpenHours {
    raw: String,
    windows: Vec<TimeWindow>,
    bitmask: [u64; BITMASK_WORDS],
}

impl OpenHours {
    /// Parses an OSM opening_hours expression with global lock-free caching.
    #[inline(always)]
    pub fn parse(expression: &str) -> Arc<Self> {
        let trimmed = expression.trim();
        if trimmed.is_empty() {
            return Arc::clone(&EMPTY);
        }
        if trimmed.len() == 4 && trimmed.eq_ignore_ascii_case("24/7") {
            return Arc::clone(&ALWAYS_OPEN);
        }

        // Fast pointer check matching Go's L1 unsafe.StringData check
        let ptr_hit = LAST_VAL.with(|v| {
            if let Some(cached) = v.borrow().as_ref() {
                if cached.raw.as_ptr() == trimmed.as_ptr() && cached.raw.len() == trimmed.len() {
                    return Some(Arc::clone(cached));
                }
            }
            None
        });
        if let Some(cached) = ptr_hit {
            return cached;
        }

        let hash = std::hash::BuildHasher::hash_one(&rustc_hash::FxBuildHasher, trimmed);
        let hit = LAST_HASH.with(|h| {
            if h.get() == hash && hash != 0 {
                LAST_VAL.with(|v| {
                    if let Some(cached) = v.borrow().as_ref() {
                        if cached.raw == trimmed {
                            return Some(Arc::clone(cached));
                        }
                    }
                    None
                })
            } else {
                None
            }
        });
        if let Some(cached) = hit {
            return cached;
        }

        // 2. Global intern pool lookup
        {
            let pool = INTERN_POOL.read();
            if let Some(cached) = pool.get(trimmed) {
                let arc = Arc::clone(cached);
                LAST_HASH.with(|h| h.set(hash));
                LAST_VAL.with(|v| *v.borrow_mut() = Some(Arc::clone(&arc)));
                return arc;
            }
        }

        // 3. Parse and insert
        let parsed = Arc::new(Self::parse_uncached(trimmed));
        let mut pool = INTERN_POOL.write();
        pool.insert(trimmed.to_string(), Arc::clone(&parsed));
        LAST_HASH.with(|h| h.set(hash));
        LAST_VAL.with(|v| *v.borrow_mut() = Some(Arc::clone(&parsed)));
        parsed
    }

    /// Returns the raw OSM expression.
    #[inline(always)]
    pub fn raw(&self) -> &str {
        &self.raw
    }

    /// Returns the slice of baked disjoint time windows.
    #[inline(always)]
    pub fn windows(&self) -> &[TimeWindow] {
        &self.windows
    }

    /// Returns true if the schedule is 24/7.
    #[inline(always)]
    pub fn is_always_open(&self) -> bool {
        self.windows.len() == 1
            && self.windows[0].start == 0
            && self.windows[0].end == MINUTES_PER_WEEK as u16
    }

    /// Returns true if the schedule is completely closed / empty.
    #[inline(always)]
    pub fn is_empty(&self) -> bool {
        self.windows.is_empty()
    }

    /// Ultra-fast $O(1)$ scalar hardware bit testing using 64-bit integer timestamp math.
    #[inline(always)]
    pub fn is_open<Tz: TimeZone>(&self, dt: &DateTime<Tz>) -> bool {
        if self.windows.is_empty() {
            return false;
        }
        if self.is_always_open() {
            return true;
        }

        let local_secs = (dt.timestamp() + dt.offset().fix().local_minus_utc() as i64) as u64;
        let week_min = ((local_secs / 60 + 4320) % (MINUTES_PER_WEEK as u64)) as usize;
        let word = week_min >> 6;
        let mask = 1u64 << (week_min & 63);
        unsafe { (*self.bitmask.get_unchecked(word) & mask) != 0 }
    }

    /// Point-in-time check for NaiveDateTime.
    #[inline(always)]
    pub fn is_open_naive(&self, dt: &NaiveDateTime) -> bool {
        if self.windows.is_empty() {
            return false;
        }
        if self.is_always_open() {
            return true;
        }

        let week_min = Self::get_week_minute_naive(dt.weekday(), dt.hour(), dt.minute());
        let word = week_min >> 6;
        let mask = 1u64 << (week_min & 63);
        (self.bitmask[word] & mask) != 0
    }

    /// Alias for is_open.
    #[inline(always)]
    pub fn match_time<Tz: TimeZone>(&self, dt: &DateTime<Tz>) -> bool {
        self.is_open(dt)
    }

    /// Returns the duration until the next opening window via $O(\log N)$ window binary search.
    #[inline(always)]
    pub fn get_time_to_open<Tz: TimeZone>(&self, from: &DateTime<Tz>) -> Option<Duration> {
        if self.windows.is_empty() {
            return None;
        }
        if self.is_always_open() {
            return Some(Duration::zero());
        }

        let (t, sub_seconds, sub_nanos) = Self::get_week_minute_and_sub(from);
        let idx = self.find_first_window_starting_at_or_after(t);

        if idx < self.windows.len() {
            let w = unsafe { self.windows.get_unchecked(idx) };
            if (w.start as usize) <= t {
                return Some(Duration::zero());
            }
            let diff_min = (w.start as usize) - t;
            let total_nanos = (diff_min as i64 * 60 - sub_seconds) * 1_000_000_000 - sub_nanos;
            Some(Duration::nanoseconds(total_nanos))
        } else {
            let diff_min = (MINUTES_PER_WEEK - t) + (unsafe { self.windows.get_unchecked(0).start } as usize);
            let total_nanos = (diff_min as i64 * 60 - sub_seconds) * 1_000_000_000 - sub_nanos;
            Some(Duration::nanoseconds(total_nanos))
        }
    }

    /// Returns the duration until an opening window with at least `required` continuous duration is available.
    #[inline(always)]
    pub fn get_time_to_open_for_duration<Tz: TimeZone>(
        &self,
        from: &DateTime<Tz>,
        required: Duration,
    ) -> Option<Duration> {
        if self.windows.is_empty() {
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
        if self.is_always_open() {
            return Some(Duration::zero());
        }

        let (t, sub_seconds, sub_nanos) = Self::get_week_minute_and_sub(from);
        let sub_dur_nanos = sub_seconds * 1_000_000_000 + sub_nanos;
        let start_idx = self.find_first_window_starting_at_or_after(t);

        let n = self.windows.len();
        let last_ends_at_week_end = self.windows[n - 1].end == MINUTES_PER_WEEK as u16;
        let first_starts_at_zero = self.windows[0].start == 0;

        for i in start_idx..n {
            let w = unsafe { self.windows.get_unchecked(i) };
            let effective_end = if i == n - 1 && last_ends_at_week_end && first_starts_at_zero {
                MINUTES_PER_WEEK + (unsafe { self.windows.get_unchecked(0).end } as usize)
            } else {
                w.end as usize
            };

            if t >= (w.start as usize) {
                let rem_nanos = ((effective_end - t) as i64) * 60_000_000_000 - sub_dur_nanos;
                if rem_nanos >= req_nanos {
                    return Some(Duration::zero());
                }
            } else {
                if effective_end - (w.start as usize) >= req_minutes {
                    let diff_min = (w.start as usize) - t;
                    let total_nanos = (diff_min as i64 * 60 - sub_seconds) * 1_000_000_000 - sub_nanos;
                    return Some(Duration::nanoseconds(total_nanos));
                }
            }
        }

        for i in 0..n {
            let w = unsafe { self.windows.get_unchecked(i) };
            let effective_end = if i == n - 1 && last_ends_at_week_end && first_starts_at_zero {
                MINUTES_PER_WEEK + (unsafe { self.windows.get_unchecked(0).end } as usize)
            } else {
                w.end as usize
            };

            if effective_end - (w.start as usize) >= req_minutes {
                let diff_min = (MINUTES_PER_WEEK - t) + (w.start as usize);
                let total_nanos = (diff_min as i64 * 60 - sub_seconds) * 1_000_000_000 - sub_nanos;
                return Some(Duration::nanoseconds(total_nanos));
            }
        }

        None
    }

    /// Returns the exact timestamp when a job of duration `duration` can start.
    #[inline(always)]
    pub fn when<Tz: TimeZone>(&self, from: &DateTime<Tz>, duration: Duration) -> Option<DateTime<Tz>> {
        let wait = self.get_time_to_open_for_duration(from, duration)?;
        if wait.is_zero() {
            Some(from.clone())
        } else {
            from.clone().checked_add_signed(wait)
        }
    }

    /// Returns the end timestamp of the current open shift.
    #[inline(always)]
    pub fn get_current_shift_end<Tz: TimeZone>(&self, dt: &DateTime<Tz>) -> Option<DateTime<Tz>> {
        if self.windows.is_empty() {
            return None;
        }
        if self.is_always_open() {
            return Some(dt.clone() + Duration::weeks(52));
        }

        let (t, sub_seconds, sub_nanos) = Self::get_week_minute_and_sub(dt);
        let idx = self.find_first_window_starting_at_or_after(t);
        if idx >= self.windows.len() || (self.windows[idx].start as usize) > t {
            return None;
        }

        let w = unsafe { self.windows.get_unchecked(idx) };
        let mut diff_min = (w.end as usize) - t;
        if idx == self.windows.len() - 1
            && w.end == MINUTES_PER_WEEK as u16
            && unsafe { self.windows.get_unchecked(0).start } == 0
        {
            diff_min = (MINUTES_PER_WEEK - t) + (unsafe { self.windows.get_unchecked(0).end } as usize);
        }

        let total_nanos = (diff_min as i64 * 60 - sub_seconds) * 1_000_000_000 - sub_nanos;
        dt.clone().checked_add_signed(Duration::nanoseconds(total_nanos))
    }

    /// Returns (is_currently_open, duration_until_next_state_change).
    #[inline(always)]
    pub fn next_dur<Tz: TimeZone>(&self, dt: &DateTime<Tz>) -> (bool, Duration) {
        if self.windows.is_empty() {
            return (false, Duration::zero());
        }
        if self.is_always_open() {
            return (true, Duration::days(365));
        }

        let (t, sub_seconds, sub_nanos) = Self::get_week_minute_and_sub(dt);
        let idx = self.find_first_window_starting_at_or_after(t);

        if idx < self.windows.len() {
            let w = unsafe { self.windows.get_unchecked(idx) };
            if (w.start as usize) <= t {
                // Currently open
                let mut diff_min = (w.end as usize) - t;
                if idx == self.windows.len() - 1
                    && w.end == MINUTES_PER_WEEK as u16
                    && unsafe { self.windows.get_unchecked(0).start } == 0
                {
                    diff_min = (MINUTES_PER_WEEK - t) + (unsafe { self.windows.get_unchecked(0).end } as usize);
                }
                let total_nanos = (diff_min as i64 * 60 - sub_seconds) * 1_000_000_000 - sub_nanos;
                return (true, Duration::nanoseconds(total_nanos));
            }
            // Currently closed, opens at w.start
            let diff_min = (w.start as usize) - t;
            let total_nanos = (diff_min as i64 * 60 - sub_seconds) * 1_000_000_000 - sub_nanos;
            return (false, Duration::nanoseconds(total_nanos));
        }

        // Currently closed, opens at windows[0].start next week
        let diff_min = (MINUTES_PER_WEEK - t) + (unsafe { self.windows.get_unchecked(0).start } as usize);
        let total_nanos = (diff_min as i64 * 60 - sub_seconds) * 1_000_000_000 - sub_nanos;
        (false, Duration::nanoseconds(total_nanos))
    }

    /// Returns (is_currently_open, next_transition_timestamp).
    #[inline(always)]
    pub fn next_date<Tz: TimeZone>(&self, dt: &DateTime<Tz>) -> (bool, DateTime<Tz>) {
        if self.windows.is_empty() {
            return (false, dt.clone());
        }
        if self.is_always_open() {
            return (true, dt.clone() + Duration::days(365));
        }

        let (t, sub_seconds, sub_nanos) = Self::get_week_minute_and_sub(dt);
        let idx = self.find_first_window_starting_at_or_after(t);

        let n = self.windows.len();
        if idx < n {
            let w = unsafe { self.windows.get_unchecked(idx) };
            if (w.start as usize) <= t {
                // Currently open
                let mut diff_min = (w.end as usize) - t;
                if idx == n - 1
                    && w.end == MINUTES_PER_WEEK as u16
                    && unsafe { self.windows.get_unchecked(0).start } == 0
                {
                    diff_min = (MINUTES_PER_WEEK - t) + (unsafe { self.windows.get_unchecked(0).end } as usize);
                }
                let total_nanos = (diff_min as i64 * 60 - sub_seconds) * 1_000_000_000 - sub_nanos;
                let next_dt = dt
                    .clone()
                    .checked_add_signed(Duration::nanoseconds(total_nanos))
                    .unwrap_or_else(|| dt.clone());
                return (true, next_dt);
            }
            // Currently closed, opens at w.start
            let diff_min = (w.start as usize) - t;
            let total_nanos = (diff_min as i64 * 60 - sub_seconds) * 1_000_000_000 - sub_nanos;
            let next_dt = dt
                .clone()
                .checked_add_signed(Duration::nanoseconds(total_nanos))
                .unwrap_or_else(|| dt.clone());
            return (false, next_dt);
        }

        // Currently closed, opens at windows[0].start next week
        let diff_min = (MINUTES_PER_WEEK - t) + (unsafe { self.windows.get_unchecked(0).start } as usize);
        let total_nanos = (diff_min as i64 * 60 - sub_seconds) * 1_000_000_000 - sub_nanos;
        let next_dt = dt
            .clone()
            .checked_add_signed(Duration::nanoseconds(total_nanos))
            .unwrap_or_else(|| dt.clone());
        (false, next_dt)
    }

    #[inline(always)]
    fn get_week_minute_and_sub<Tz: TimeZone>(dt: &DateTime<Tz>) -> (usize, i64, i64) {
        let raw_local = dt.timestamp() + dt.offset().fix().local_minus_utc() as i64;
        let local_secs = raw_local as u64;
        let mins = local_secs / 60 + 4320;
        let week_min = (mins % (MINUTES_PER_WEEK as u64)) as usize;
        let sub_seconds = (local_secs % 60) as i64;
        let sub_nanos = dt.timestamp_subsec_nanos() as i64;
        (week_min, sub_seconds, sub_nanos)
    }

    #[inline(always)]
    pub fn is_open_utc(&self, dt: &DateTime<chrono::Utc>) -> bool {
        if self.windows.is_empty() {
            return false;
        }
        if self.is_always_open() {
            return true;
        }

        let local_secs = dt.timestamp() as u64;
        let week_min = ((local_secs / 60 + 4320) % (MINUTES_PER_WEEK as u64)) as usize;
        let word = week_min >> 6;
        let mask = 1u64 << (week_min & 63);
        unsafe { (*self.bitmask.get_unchecked(word) & mask) != 0 }
    }

    #[inline(always)]
    fn get_week_minute_naive(weekday: Weekday, hour: u32, minute: u32) -> usize {
        let day_idx = weekday.num_days_from_monday() as usize; // Monday = 0 .. Sunday = 6
        day_idx * 1440 + (hour as usize) * 60 + (minute as usize)
    }

    #[inline(always)]
    fn find_first_window_starting_at_or_after(&self, t: usize) -> usize {
        let windows = self.windows.as_slice();
        let n = windows.len();
        match n {
            0 => 0,
            1 => {
                if (unsafe { windows.get_unchecked(0).end } as usize) > t {
                    0
                } else {
                    1
                }
            }
            2 => {
                if (unsafe { windows.get_unchecked(0).end } as usize) > t {
                    0
                } else if (unsafe { windows.get_unchecked(1).end } as usize) > t {
                    1
                } else {
                    2
                }
            }
            3 => {
                if (unsafe { windows.get_unchecked(0).end } as usize) > t {
                    0
                } else if (unsafe { windows.get_unchecked(1).end } as usize) > t {
                    1
                } else if (unsafe { windows.get_unchecked(2).end } as usize) > t {
                    2
                } else {
                    3
                }
            }
            4 => {
                if (unsafe { windows.get_unchecked(0).end } as usize) > t {
                    0
                } else if (unsafe { windows.get_unchecked(1).end } as usize) > t {
                    1
                } else if (unsafe { windows.get_unchecked(2).end } as usize) > t {
                    2
                } else if (unsafe { windows.get_unchecked(3).end } as usize) > t {
                    3
                } else {
                    4
                }
            }
            _ => {
                let mut low = 0;
                let mut high = n - 1;
                let mut result = n;
                while low <= high {
                    let mid = (low + high) >> 1;
                    let w = unsafe { windows.get_unchecked(mid) };
                    if (w.end as usize) > t {
                        result = mid;
                        if mid == 0 {
                            break;
                        }
                        high = mid - 1;
                    } else {
                        low = mid + 1;
                    }
                }
                result
            }
        }
    }

    fn parse_uncached(expression: &str) -> Self {
        let mut minutes = [false; MINUTES_PER_WEEK];
        let bytes = expression.as_bytes();
        let mut rule_start = 0;

        while rule_start < bytes.len() {
            let mut rule_end = rule_start;
            while rule_end < bytes.len() && bytes[rule_end] != b';' {
                rule_end += 1;
            }
            let rule_bytes = &bytes[rule_start..rule_end];
            rule_start = rule_end + 1;

            let trimmed = Self::trim_ascii_slice(rule_bytes);
            if trimmed.is_empty() {
                continue;
            }

            let is_off = Self::contains_ignore_case(trimmed, b"off")
                || Self::contains_ignore_case(trimmed, b"closed");

            let mut days = [0usize; 7];
            let mut num_days = 0;
            let mut intervals = [(0usize, 0usize); 16];
            let mut num_intervals = 0;

            if !Self::parse_rule_tokens_fast(
                trimmed,
                &mut days,
                &mut num_days,
                &mut intervals,
                &mut num_intervals,
            ) {
                continue;
            }

            if num_days == 0 && num_intervals == 0 {
                continue;
            }
            if num_days == 0 {
                for d in 0..7 {
                    days[d] = d;
                }
                num_days = 7;
            }
            if num_intervals == 0 {
                intervals[0] = (0, 1440);
                num_intervals = 1;
            }

            for &day in &days[..num_days] {
                for &(start, end) in &intervals[..num_intervals] {
                    let start_min = day * 1440 + start;
                    if end > 1440 {
                        let actual_end = day * 1440 + end;
                        for m in start_min..actual_end {
                            minutes[m % MINUTES_PER_WEEK] = !is_off;
                        }
                    } else if start > end {
                        let actual_end = (day + 1) * 1440 + end;
                        for m in start_min..actual_end {
                            minutes[m % MINUTES_PER_WEEK] = !is_off;
                        }
                    } else {
                        let end_min = day * 1440 + end;
                        for m in start_min..end_min {
                            minutes[m % MINUTES_PER_WEEK] = !is_off;
                        }
                    }
                }
            }
        }

        // Bake into disjoint windows and bitmask
        let mut bm = [0u64; BITMASK_WORDS];
        let mut win_list = Vec::with_capacity(16);
        let mut in_window_start: Option<usize> = None;

        for (i, &open) in minutes.iter().enumerate() {
            if open {
                bm[i >> 6] |= 1u64 << (i & 63);
                if in_window_start.is_none() {
                    in_window_start = Some(i);
                }
            } else if let Some(start) = in_window_start {
                win_list.push(TimeWindow {
                    start: start as u16,
                    end: i as u16,
                });
                in_window_start = None;
            }
        }
        if let Some(start) = in_window_start {
            win_list.push(TimeWindow {
                start: start as u16,
                end: MINUTES_PER_WEEK as u16,
            });
        }
        win_list.shrink_to_fit();

        OpenHours {
            raw: expression.to_string(),
            windows: win_list,
            bitmask: bm,
        }
    }

    #[inline(always)]
    fn trim_ascii_slice(bytes: &[u8]) -> &[u8] {
        let mut start = 0;
        while start < bytes.len() && bytes[start].is_ascii_whitespace() {
            start += 1;
        }
        let mut end = bytes.len();
        while end > start && bytes[end - 1].is_ascii_whitespace() {
            end -= 1;
        }
        &bytes[start..end]
    }

    #[inline(always)]
    fn contains_ignore_case(haystack: &[u8], needle: &[u8]) -> bool {
        if needle.is_empty() {
            return true;
        }
        if haystack.len() < needle.len() {
            return false;
        }
        haystack.windows(needle.len()).any(|w| {
            w.iter()
                .zip(needle.iter())
                .all(|(a, b)| a.to_ascii_lowercase() == b.to_ascii_lowercase())
        })
    }

    fn parse_rule_tokens_fast(
        rule: &[u8],
        days: &mut [usize; 7],
        num_days: &mut usize,
        intervals: &mut [(usize, usize); 16],
        num_intervals: &mut usize,
    ) -> bool {
        let mut i = 0;
        while i < rule.len() {
            while i < rule.len() && rule[i].is_ascii_whitespace() {
                i += 1;
            }
            if i >= rule.len() {
                break;
            }
            let start = i;
            while i < rule.len() && !rule[i].is_ascii_whitespace() {
                i += 1;
            }
            let token = &rule[start..i];
            if token.is_empty() {
                continue;
            }

            if token.eq_ignore_ascii_case(b"off") || token.eq_ignore_ascii_case(b"closed") {
                continue;
            }

            let first_char = token[0];
            if first_char.is_ascii_digit() || first_char == b'+' {
                if !Self::parse_time_intervals_fast(token, intervals, num_intervals) {
                    return false;
                }
            } else if first_char.is_ascii_alphabetic() {
                if !Self::parse_days_fast(token, days, num_days) {
                    return false;
                }
            } else {
                return false;
            }
        }
        true
    }

    fn parse_days_fast(token: &[u8], days: &mut [usize; 7], num_days: &mut usize) -> bool {
        for part in token.split(|&b| b == b',') {
            let p = Self::trim_ascii_slice(part);
            if p.is_empty() {
                continue;
            }

            if let Some(dash_idx) = p.iter().position(|&b| b == b'-') {
                let d1_str = &p[..dash_idx];
                let d2_str = &p[dash_idx + 1..];
                if let (Some(d1), Some(mut d2)) =
                    (Self::parse_day_fast(d1_str), Self::parse_day_fast(d2_str))
                {
                    if d2 < d1 {
                        d2 += 7;
                    }
                    for d in d1..=d2 {
                        let actual = d % 7;
                        if !days[..*num_days].contains(&actual) && *num_days < 7 {
                            days[*num_days] = actual;
                            *num_days += 1;
                        }
                    }
                } else {
                    return false;
                }
            } else if let Some(d) = Self::parse_day_fast(p) {
                if !days[..*num_days].contains(&d) && *num_days < 7 {
                    days[*num_days] = d;
                    *num_days += 1;
                }
            } else {
                return false;
            }
        }
        true
    }

    #[inline(always)]
    fn parse_day_fast(day: &[u8]) -> Option<usize> {
        let trimmed = Self::trim_ascii_slice(day);
        if trimmed.len() < 2 {
            return None;
        }
        let b0 = trimmed[0].to_ascii_lowercase();
        let b1 = trimmed[1].to_ascii_lowercase();
        match (b0, b1) {
            (b'm', b'o') => Some(0),
            (b't', b'u') => Some(1),
            (b'w', b'e') => Some(2),
            (b't', b'h') => Some(3),
            (b'f', b'r') => Some(4),
            (b's', b'a') => Some(5),
            (b's', b'u') => Some(6),
            _ => None,
        }
    }

    fn parse_time_intervals_fast(
        token: &[u8],
        intervals: &mut [(usize, usize); 16],
        num_intervals: &mut usize,
    ) -> bool {
        for part in token.split(|&b| b == b',') {
            let p = Self::trim_ascii_slice(part);
            if p.is_empty() {
                continue;
            }

            if p.ends_with(b"+") {
                if let Some(start) = Self::parse_minute_of_day_fast(&p[..p.len() - 1]) {
                    if *num_intervals < 16 {
                        intervals[*num_intervals] = (start, 1440);
                        *num_intervals += 1;
                    }
                } else {
                    return false;
                }
            } else if let Some(dash_idx) = p.iter().position(|&b| b == b'-') {
                let start_str = &p[..dash_idx];
                let end_str = &p[dash_idx + 1..];
                if let (Some(start), Some(mut end)) = (
                    Self::parse_minute_of_day_fast(start_str),
                    Self::parse_minute_of_day_fast(end_str),
                ) {
                    if end == 0 || end < start {
                        end += 1440;
                    }
                    if *num_intervals < 16 {
                        intervals[*num_intervals] = (start, end);
                        *num_intervals += 1;
                    }
                } else {
                    return false;
                }
            } else {
                return false;
            }
        }
        true
    }

    #[inline(always)]
    fn parse_minute_of_day_fast(time: &[u8]) -> Option<usize> {
        let trimmed = Self::trim_ascii_slice(time);
        let colon_idx = trimmed.iter().position(|&b| b == b':')?;
        let h = Self::parse_u32_fast(&trimmed[..colon_idx])? as usize;
        let m = Self::parse_u32_fast(&trimmed[colon_idx + 1..])? as usize;
        if h <= 24 && m < 60 {
            Some(h * 60 + m)
        } else {
            None
        }
    }

    #[inline(always)]
    fn parse_u32_fast(bytes: &[u8]) -> Option<u32> {
        let trimmed = Self::trim_ascii_slice(bytes);
        if trimmed.is_empty() {
            return None;
        }
        let mut val = 0u32;
        for &b in trimmed {
            if !b.is_ascii_digit() {
                return None;
            }
            val = val * 10 + (b - b'0') as u32;
        }
        Some(val)
    }
}

impl FromStr for OpenHours {
    type Err = std::convert::Infallible;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(Self::parse_uncached(s))
    }
}

impl fmt::Display for OpenHours {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.raw)
    }
}

impl Serialize for OpenHours {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.raw)
    }
}

impl<'de> Deserialize<'de> for OpenHours {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct OpenHoursVisitor;

        impl<'de> Visitor<'de> for OpenHoursVisitor {
            type Value = OpenHours;

            fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
                formatter.write_str("a valid opening_hours string")
            }

            fn visit_str<E>(self, value: &str) -> Result<OpenHours, E>
            where
                E: de::Error,
            {
                let arc = OpenHours::parse(value);
                Ok((*arc).clone())
            }
        }

        deserializer.deserialize_str(OpenHoursVisitor)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};

    fn monday_midnight() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 5, 18, 0, 0, 0).unwrap()
    }

    #[test]
    fn test_always_open() {
        let oh = OpenHours::parse("24/7");
        let start = monday_midnight();
        assert!(oh.is_always_open());
        assert!(oh.is_open(&start));
        assert_eq!(oh.get_time_to_open(&start), Some(Duration::zero()));
    }

    #[test]
    fn test_empty_schedule() {
        let oh = OpenHours::parse("");
        let start = monday_midnight();
        assert!(oh.is_empty());
        assert!(!oh.is_open(&start));
        assert_eq!(oh.get_time_to_open(&start), None);
        assert_eq!(oh.get_current_shift_end(&start), None);
    }

    #[test]
    fn test_simple_shift() {
        let oh = OpenHours::parse("Mo 08:00-18:00");
        let start = monday_midnight();
        assert!(!oh.is_open(&(start + Duration::hours(7))));
        assert!(oh.is_open(&(start + Duration::hours(8))));
        assert!(oh.is_open(&(start + Duration::hours(12))));
        assert!(oh.is_open(&(start + Duration::hours(17) + Duration::minutes(59))));
        assert!(!oh.is_open(&(start + Duration::hours(18))));
    }

    #[test]
    fn test_multi_day_and_split_shifts() {
        let oh = OpenHours::parse("Mo-Fr 08:00-12:00, 13:00-17:00; Sa 08:00-12:00");
        let start = monday_midnight();

        assert!(oh.is_open(&(start + Duration::hours(10))));
        assert!(!oh.is_open(&(start + Duration::hours(12) + Duration::minutes(30))));
        assert!(oh.is_open(&(start + Duration::hours(14))));
        assert!(oh.is_open(&(start + Duration::days(5) + Duration::hours(10))));
        assert!(!oh.is_open(&(start + Duration::days(5) + Duration::hours(14))));
        assert!(!oh.is_open(&(start + Duration::days(6) + Duration::hours(10))));
    }

    #[test]
    fn test_overnight_shifts() {
        let oh = OpenHours::parse("Mo 22:00-04:00");
        let start = monday_midnight();
        assert!(oh.is_open(&(start + Duration::hours(23))));
        assert!(oh.is_open(&(start + Duration::days(1) + Duration::hours(2))));
        assert!(!oh.is_open(&(start + Duration::days(1) + Duration::hours(5))));

        let sunday_oh = OpenHours::parse("Su 22:00-04:00");
        assert!(oh.is_open(&(start + Duration::hours(23))));
        assert!(sunday_oh.is_open(&(start + Duration::days(6) + Duration::hours(23))));
        assert!(sunday_oh.is_open(&(start + Duration::hours(2))));
        assert!(!sunday_oh.is_open(&(start + Duration::hours(5))));
    }

    #[test]
    fn test_off_exclusion() {
        let oh = OpenHours::parse("Mo-Su 00:00-24:00; Tu 12:00-13:00 off");
        let start = monday_midnight();
        assert!(oh.is_open(&(start + Duration::days(1) + Duration::hours(11))));
        assert!(!oh.is_open(&(start + Duration::days(1) + Duration::hours(12) + Duration::minutes(30))));
        assert!(oh.is_open(&(start + Duration::days(1) + Duration::hours(14))));
    }

    #[test]
    fn test_current_shift_end() {
        let oh = OpenHours::parse("Mo-Fr 08:00-12:00, 13:00-17:00");
        let start = monday_midnight();
        assert_eq!(
            oh.get_current_shift_end(&(start + Duration::hours(10))),
            Some(start + Duration::hours(12))
        );
        assert_eq!(
            oh.get_current_shift_end(&(start + Duration::hours(12) + Duration::minutes(30))),
            None
        );
    }

    #[test]
    fn test_get_time_to_open() {
        let oh = OpenHours::parse("Mo-Fr 08:00-12:00, 13:00-17:00");
        let start = monday_midnight();
        assert_eq!(
            oh.get_time_to_open(&(start + Duration::hours(6))),
            Some(Duration::hours(2))
        );
        assert_eq!(
            oh.get_time_to_open(&(start + Duration::hours(12) + Duration::minutes(30))),
            Some(Duration::minutes(30))
        );
        assert_eq!(
            oh.get_time_to_open(&(start + Duration::hours(10))),
            Some(Duration::zero())
        );
    }

    #[test]
    fn test_get_time_to_open_for_duration() {
        let oh = OpenHours::parse("Mo 08:00-10:00, 11:00-17:00");
        let start = monday_midnight();
        let wait = oh.get_time_to_open_for_duration(&(start + Duration::hours(9)), Duration::hours(4));
        assert_eq!(wait, Some(Duration::hours(2)));

        let when = oh.when(&(start + Duration::hours(9)), Duration::hours(4));
        assert_eq!(when, Some(start + Duration::hours(11)));
    }

    #[test]
    fn test_next_dur_and_next_date() {
        let oh = OpenHours::parse("Mo 08:00-18:00");
        let start = monday_midnight();
        let (is_open, dur) = oh.next_dur(&(start + Duration::hours(10)));
        assert!(is_open);
        assert_eq!(dur, Duration::hours(8));

        let (is_open_next, next_date) = oh.next_date(&(start + Duration::hours(10)));
        assert!(is_open_next);
        assert_eq!(next_date, start + Duration::hours(18));
    }

    #[test]
    fn test_serde_json() {
        let oh = OpenHours::parse("Mo-Fr 08:00-17:00");
        let json = serde_json::to_string(&*oh).unwrap();
        assert_eq!(json, "\"Mo-Fr 08:00-17:00\"");

        let deserialized: OpenHours = serde_json::from_str(&json).unwrap();
        assert_eq!(oh.raw, deserialized.raw);
    }
}

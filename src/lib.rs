use chrono::{Datelike, DateTime, Duration, NaiveDateTime, Offset, TimeZone, Timelike, Weekday};
use parking_lot::RwLock;
use rustc_hash::FxHashMap;
use serde::de::{self, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::cell::RefCell;
use std::fmt;
use std::str::FromStr;
use std::sync::{Arc, LazyLock, OnceLock};

pub const MINUTES_PER_WEEK: usize = 7 * 24 * 60; // 10,080
pub const BITMASK_WORDS: usize = (MINUTES_PER_WEEK + 63) / 64; // 158

/// High bit of a packed `succ` entry: set when the week-minute is open.
const OPEN_FLAG: u16 = 0x8000;
/// Low 15 bits of a packed `succ` entry: minutes to the next state change.
/// The largest possible value is `(MINUTES_PER_WEEK - 1) + (MINUTES_PER_WEEK - 1) < 2^15`.
const CHANGE_MASK: u16 = 0x7fff;

static INTERN_POOL: LazyLock<RwLock<FxHashMap<String, Arc<OpenHours>>>> =
    LazyLock::new(|| RwLock::new(FxHashMap::default()));

static ALWAYS_OPEN: LazyLock<Arc<OpenHours>> = LazyLock::new(|| {
    let mut bm = [!0u64; BITMASK_WORDS];
    let rem = MINUTES_PER_WEEK % 64;
    if rem != 0 {
        bm[BITMASK_WORDS - 1] = (1u64 << rem) - 1;
    }
    Arc::new(OpenHours {
        inner: Arc::new(OpenHoursInner {
            raw: Arc::from("24/7"),
            windows: Arc::from(vec![TimeWindow {
                start: 0,
                end: MINUTES_PER_WEEK as u16,
            }]),
            bitmask: bm,
            succ: OnceLock::new(),
        }),
    })
});

static EMPTY: LazyLock<Arc<OpenHours>> = LazyLock::new(|| {
    Arc::new(OpenHours {
        inner: Arc::new(OpenHoursInner {
            raw: Arc::from(""),
            windows: Arc::from(Vec::<TimeWindow>::new()),
            bitmask: [0u64; BITMASK_WORDS],
            succ: OnceLock::new(),
        }),
    })
});

thread_local! {
    /// Most recent intern-pool result for the calling thread. The cached `Arc`'s own
    /// `raw` allocation is both the value and the identity key, so the hit path needs
    /// no content hash — a length check plus a slice comparison fully guards it.
    static LAST: RefCell<Option<Arc<OpenHours>>> = const { RefCell::new(None) };
    /// Content-keyed memo of the last JSON string decoded on this thread. It is keyed by
    /// content (never pointer identity), so a reused buffer with different bytes misses.
    static LAST_JSON: RefCell<Option<(Box<str>, Arc<OpenHoursInner>)>> = const { RefCell::new(None) };
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct TimeWindow {
    pub start: u16,
    pub end: u16,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[repr(align(64))]
#[doc(hidden)]
pub struct OpenHoursInner {
    raw: Arc<str>,
    windows: Arc<[TimeWindow]>,
    bitmask: [u64; BITMASK_WORDS],
    /// Lazily built per-minute successor table. `succ[m]` packs the answer for the
    /// week-minute `m`: bit 15 is the open flag, bits 0..14 hold the number of minutes
    /// until the next state change (window start when closed, window end / week-wrap
    /// when open). It is a pure function of `windows`, so it is bit-identical to the
    /// former lower-bound search, and being lazy means parse-only workloads never build it.
    succ: OnceLock<Box<[u16]>>,
}

impl std::hash::Hash for OpenHoursInner {
    #[inline]
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        // The lazy tables are derived from `windows`, so they are excluded to keep
        // hashing deterministic regardless of whether a query has run.
        self.raw.hash(state);
        self.windows.hash(state);
        self.bitmask.hash(state);
    }
}

/// An interned OSM `opening_hours` schedule.
///
/// The entire payload lives behind a single `Arc`, so cloning an `OpenHours` (as the
/// `Deserialize` impl does) is one refcount bump. Field access transparently dereferences
/// into [`OpenHoursInner`], so the hot `bitmask` lookup stays a single indirection.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct OpenHours {
    inner: Arc<OpenHoursInner>,
}

impl std::ops::Deref for OpenHours {
    type Target = OpenHoursInner;

    #[inline(always)]
    fn deref(&self) -> &OpenHoursInner {
        &self.inner
    }
}

impl OpenHours {
    /// Parses an OSM opening_hours expression with global lock-free caching.
    #[inline(always)]
    pub fn parse(expression: &str) -> Arc<Self> {
        let trimmed = Self::trim_ascii_str(expression);
        if trimmed.is_empty() {
            return Arc::clone(&EMPTY);
        }
        if trimmed.len() == 4 && trimmed.eq_ignore_ascii_case("24/7") {
            return Arc::clone(&ALWAYS_OPEN);
        }

        // 1. Single thread-local cache: the cached `Arc`'s own `raw` slice is the key, so
        //    the hit path is a length check plus one comparison — no rehashing required.
        let hit = LAST.with(|c| {
            let c = c.borrow();
            if let Some(cached) = c.as_ref() {
                if cached.raw.len() == trimmed.len() && &*cached.raw == trimmed {
                    return Some(Arc::clone(cached));
                }
            }
            None
        });
        if let Some(cached) = hit {
            return cached;
        }

        // 2. Global intern pool lookup
        {
            let pool = INTERN_POOL.read();
            if let Some(cached) = pool.get(trimmed) {
                let arc = Arc::clone(cached);
                Self::remember(&arc);
                return arc;
            }
        }

        // 3. Parse and insert
        let parsed = Arc::new(Self::parse_uncached(trimmed));
        let mut pool = INTERN_POOL.write();
        pool.insert(trimmed.to_string(), Arc::clone(&parsed));
        Self::remember(&parsed);
        parsed
    }

    /// Records `arc` as the calling thread's most recent intern-pool result.
    #[inline]
    fn remember(arc: &Arc<Self>) {
        LAST.with(|c| {
            *c.borrow_mut() = Some(Arc::clone(arc));
        });
    }

    /// Like [`parse`](Self::parse), but returns an owned [`OpenHours`] that shares the
    /// interned payload through a *single* inner-`Arc` clone. On a cache hit this avoids
    /// the extra `Arc` bump that `(*parse(expr)).clone()` would perform, which is exactly
    /// the `Deserialize` fast path.
    #[inline]
    pub fn parse_owned(expression: &str) -> Self {
        let trimmed = Self::trim_ascii_str(expression);
        if trimmed.is_empty() {
            return OpenHours {
                inner: Arc::clone(&EMPTY.inner),
            };
        }
        if trimmed.len() == 4 && trimmed.eq_ignore_ascii_case("24/7") {
            return OpenHours {
                inner: Arc::clone(&ALWAYS_OPEN.inner),
            };
        }

        let hit = LAST.with(|c| {
            let c = c.borrow();
            if let Some(cached) = c.as_ref() {
                if cached.raw.len() == trimmed.len() && &*cached.raw == trimmed {
                    return Some(Arc::clone(&cached.inner));
                }
            }
            None
        });
        if let Some(inner) = hit {
            return OpenHours { inner };
        }

        {
            let pool = INTERN_POOL.read();
            if let Some(cached) = pool.get(trimmed) {
                let inner = Arc::clone(&cached.inner);
                Self::remember(cached);
                return OpenHours { inner };
            }
        }

        let parsed = Arc::new(Self::parse_uncached(trimmed));
        let inner = Arc::clone(&parsed.inner);
        let mut pool = INTERN_POOL.write();
        pool.insert(trimmed.to_string(), Arc::clone(&parsed));
        drop(pool);
        Self::remember(&parsed);
        OpenHours { inner }
    }

    /// Decodes a JSON string payload into an owned [`OpenHours`], consulting a
    /// thread-local content memo first. The memo is content-keyed, so it is sound for
    /// reused buffers; on a hit it returns with a single inner-`Arc` clone.
    #[inline]
    fn from_json_payload(value: &str) -> Self {
        if let Some(inner) = LAST_JSON.with(|c| {
            let c = c.borrow();
            match c.as_ref() {
                Some((s, inner)) if &**s == value => Some(Arc::clone(inner)),
                _ => None,
            }
        }) {
            return OpenHours { inner };
        }
        let oh = Self::parse_owned(value);
        LAST_JSON.with(|c| {
            *c.borrow_mut() = Some((Box::from(value), Arc::clone(&oh.inner)));
        });
        oh
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
    ///
    /// The baked bitmask is the single source of truth for open/closed state: it is
    /// all-zero for an empty schedule and covers every minute of the week for `24/7`,
    /// so no separate empty / always-open branch is required.
    #[inline(always)]
    pub fn is_open<Tz: TimeZone>(&self, dt: &DateTime<Tz>) -> bool {
        let local_secs = (dt.timestamp() + dt.offset().fix().local_minus_utc() as i64) as u64;
        let week_min = ((local_secs / 60 + 4320) % (MINUTES_PER_WEEK as u64)) as usize;
        let word = week_min >> 6;
        let mask = 1u64 << (week_min & 63);
        unsafe { (*self.bitmask.get_unchecked(word) & mask) != 0 }
    }

    /// Point-in-time check for NaiveDateTime.
    #[inline(always)]
    pub fn is_open_naive(&self, dt: &NaiveDateTime) -> bool {
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

    /// Returns the duration until the next opening window (one lazy table load).
    #[inline(always)]
    pub fn get_time_to_open<Tz: TimeZone>(&self, from: &DateTime<Tz>) -> Option<Duration> {
        if self.windows.is_empty() {
            return None;
        }
        if self.is_always_open() {
            return Some(Duration::zero());
        }

        let (t, sub_seconds, sub_nanos) = Self::get_week_minute_and_sub(from);
        let e = unsafe { *self.succ_table().get_unchecked(t) };
        if e & OPEN_FLAG != 0 {
            return Some(Duration::zero());
        }
        Some(Self::duration_minus_sub((e & CHANGE_MASK) as usize, sub_seconds, sub_nanos))
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

        // Successor-table fast path: for a minute `t`, `succ[t]` already holds the
        // open flag and the distance to the next state change. This resolves the two
        // common cases without the lower-bound search: (a) already open with enough
        // time left in the current window, and (b) closed with the very next window
        // long enough — the next window's length is `succ` at that window's start.
        let e = unsafe { *self.succ_table().get_unchecked(t) };
        let dist = (e & CHANGE_MASK) as usize;
        if e & OPEN_FLAG != 0 {
            if dist as i64 * 60_000_000_000 - sub_dur_nanos >= req_nanos {
                return Some(Duration::zero());
            }
        } else {
            let start_m = (t + dist) % MINUTES_PER_WEEK;
            let next_len =
                (unsafe { *self.succ_table().get_unchecked(start_m) } & CHANGE_MASK) as usize;
            if next_len >= req_minutes {
                return Some(Self::duration_minus_sub(dist, sub_seconds, sub_nanos));
            }
        }

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
                    return Some(Self::duration_minus_sub(diff_min, sub_seconds, sub_nanos));
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
                return Some(Self::duration_minus_sub(diff_min, sub_seconds, sub_nanos));
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
        let e = unsafe { *self.succ_table().get_unchecked(t) };
        if e & OPEN_FLAG == 0 {
            return None;
        }
        dt.clone().checked_add_signed(Self::duration_minus_sub(
            (e & CHANGE_MASK) as usize,
            sub_seconds,
            sub_nanos,
        ))
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
        let e = unsafe { *self.succ_table().get_unchecked(t) };
        let open = e & OPEN_FLAG != 0;
        (open, Self::duration_minus_sub((e & CHANGE_MASK) as usize, sub_seconds, sub_nanos))
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
        let e = unsafe { *self.succ_table().get_unchecked(t) };
        let open = e & OPEN_FLAG != 0;
        let next_dt = dt
            .clone()
            .checked_add_signed(Self::duration_minus_sub((e & CHANGE_MASK) as usize, sub_seconds, sub_nanos))
            .unwrap_or_else(|| dt.clone());
        (open, next_dt)
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

    /// Lazily builds and returns the packed per-minute successor table.
    ///
    /// `succ[m] = (open << 15) | minutes_to_next_state_change`, for `m in [0, 10080)`.
    /// This is a pure function of `windows` and reproduces exactly what the former
    /// lower-bound search plus the per-window arithmetic returned, including the
    /// week-boundary wrap for a window that ends at the week end while `windows[0]`
    /// starts at 0. Built on first query, so parse-only workloads never pay for it.
    #[inline]
    fn succ_table(&self) -> &[u16] {
        self.succ.get_or_init(|| {
            let week = MINUTES_PER_WEEK;
            let windows: &[TimeWindow] = &self.windows;
            let n = windows.len();
            if n == 0 {
                return vec![0u16; week].into_boxed_slice();
            }
            let last_ends_at_week_end = windows[n - 1].end as usize == week;
            let first_starts_at_zero = windows[0].start == 0;
            let mut tbl = vec![0u16; week];
            let mut idx = 0usize;
            for (m, slot) in tbl.iter_mut().enumerate() {
                while idx < n && windows[idx].end as usize <= m {
                    idx += 1;
                }
                if idx < n && windows[idx].start as usize <= m {
                    // Open minute: distance to the window end (week-wrapped for the last one).
                    let mut diff = windows[idx].end as usize - m;
                    if idx == n - 1 && last_ends_at_week_end && first_starts_at_zero {
                        diff = (week - m) + windows[0].end as usize;
                    }
                    *slot = OPEN_FLAG | diff as u16;
                } else {
                    // Closed minute: distance to the next window start (week-wrapped).
                    let diff = if idx < n {
                        windows[idx].start as usize - m
                    } else {
                        (week - m) + windows[0].start as usize
                    };
                    *slot = diff as u16;
                }
            }
            tbl.into_boxed_slice()
        })
        .as_ref()
    }

    /// Returns the index of the first window with `end > t` (or `windows.len()`), via an
    /// unrolled compare chain for small window sets and a branch-light binary search
    /// otherwise.
    #[inline(always)]
    fn find_first_window_starting_at_or_after(&self, t: usize) -> usize {
        let windows: &[TimeWindow] = &self.windows;
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
                let mut high = n;
                while low < high {
                    let mid = (low + high) >> 1;
                    if (unsafe { windows.get_unchecked(mid).end } as usize) > t {
                        high = mid;
                    } else {
                        low = mid + 1;
                    }
                }
                low
            }
        }
    }

    fn parse_uncached(expression: &str) -> Self {
        // Accumulate open minutes directly into the week bitmask (158 words) instead of a
        // one-byte-per-minute scratch array, so the per-parse cost scales with the number
        // of rules rather than with MINUTES_PER_WEEK.
        let mut bm = [0u64; BITMASK_WORDS];
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
                    let (lo, hi) = if end > 1440 {
                        (start_min, day * 1440 + end)
                    } else if start > end {
                        (start_min, (day + 1) * 1440 + end)
                    } else {
                        (start_min, day * 1440 + end)
                    };
                    Self::apply_minute_range(&mut bm, lo, hi, !is_off);
                }
            }
        }

        // Bake disjoint windows by scanning the bitmask one word at a time. Only words that
        // straddle a window boundary are walked bit-by-bit; all-zero/all-one words are O(1).
        let mut win_list: Vec<TimeWindow> = Vec::with_capacity(16);
        let mut run_start = usize::MAX;
        for (wi, &w) in bm.iter().enumerate() {
            let base = wi << 6;
            if w == 0 {
                if run_start != usize::MAX {
                    win_list.push(TimeWindow {
                        start: run_start as u16,
                        end: base as u16,
                    });
                    run_start = usize::MAX;
                }
            } else if w == !0u64 {
                if run_start == usize::MAX {
                    run_start = base;
                }
            } else {
                let mut bit = 0usize;
                while bit < 64 {
                    if (w >> bit) & 1 != 0 {
                        if run_start == usize::MAX {
                            run_start = base + bit;
                        }
                    } else if run_start != usize::MAX {
                        win_list.push(TimeWindow {
                            start: run_start as u16,
                            end: (base + bit) as u16,
                        });
                        run_start = usize::MAX;
                    }
                    bit += 1;
                }
            }
        }
        if run_start != usize::MAX {
            win_list.push(TimeWindow {
                start: run_start as u16,
                end: MINUTES_PER_WEEK as u16,
            });
        }
        win_list.shrink_to_fit();
        OpenHours {
            inner: Arc::new(OpenHoursInner {
                raw: Arc::from(expression),
                windows: Arc::from(win_list),
                bitmask: bm,
                succ: OnceLock::new(),
            }),
        }
    }

    /// Applies `value` to minute range `[lo, hi)`, wrapping around the week boundary.
    #[inline(always)]
    fn apply_minute_range(bm: &mut [u64; BITMASK_WORDS], lo: usize, hi: usize, value: bool) {
        if lo >= hi {
            return;
        }
        let n = hi - lo; // always < MINUTES_PER_WEEK by construction
        let lo_wrapped = lo % MINUTES_PER_WEEK;
        if lo_wrapped + n <= MINUTES_PER_WEEK {
            Self::apply_bit_range(bm, lo_wrapped, lo_wrapped + n, value);
        } else {
            Self::apply_bit_range(bm, lo_wrapped, MINUTES_PER_WEEK, value);
            Self::apply_bit_range(bm, 0, lo_wrapped + n - MINUTES_PER_WEEK, value);
        }
    }

    /// Sets or clears every bit in `[lo, hi)`; `hi <= MINUTES_PER_WEEK`.
    #[inline]
    fn apply_bit_range(bm: &mut [u64; BITMASK_WORDS], lo: usize, hi: usize, value: bool) {
        if lo >= hi {
            return;
        }
        let first_word = lo >> 6;
        let last_word = (hi - 1) >> 6;
        if first_word == last_word {
            let mask = Self::bit_mask(lo & 63, hi - lo);
            if value {
                bm[first_word] |= mask;
            } else {
                bm[first_word] &= !mask;
            }
            return;
        }
        let head_mask = !0u64 << (lo & 63);
        if value {
            bm[first_word] |= head_mask;
        } else {
            bm[first_word] &= !head_mask;
        }
        for w in &mut bm[first_word + 1..last_word] {
            *w = if value { !0u64 } else { 0 };
        }
        let tail_mask = Self::bit_mask(0, ((hi - 1) & 63) + 1);
        if value {
            bm[last_word] |= tail_mask;
        } else {
            bm[last_word] &= !tail_mask;
        }
    }

    /// `len` consecutive bits starting at `offset` (offset + len <= 64).
    #[inline(always)]
    fn bit_mask(offset: usize, len: usize) -> u64 {
        if len == 64 {
            !0u64
        } else {
            ((1u64 << len) - 1) << offset
        }
    }

    /// Builds the `Duration` for `diff_min` whole minutes minus the sub-minute remainder
    /// `sub_seconds`/`sub_nanos` of the query instant.
    ///
    /// Equivalent to `Duration::nanoseconds((diff_min * 60 - sub_seconds) * 1e9 - sub_nanos)`
    /// for `diff_min >= 1` and `0 <= sub_seconds < 60`, `0 <= sub_nanos < 1e9`, but the seconds
    /// and nanoseconds are produced directly so no 64-bit division by `1e9` is needed.
    #[inline(always)]
    fn duration_minus_sub(diff_min: usize, sub_seconds: i64, sub_nanos: i64) -> Duration {
        let secs = diff_min as i64 * 60 - sub_seconds;
        if sub_nanos == 0 {
            Duration::seconds(secs)
        } else {
            Duration::new(secs - 1, (1_000_000_000 - sub_nanos) as u32).unwrap()
        }
    }

    /// ASCII-only trim for the parse hot path: `str::trim` uses Unicode
    /// whitespace tables and char-boundary handling, but OSM expressions are
    /// ASCII. Falls back to `str::trim` for any non-ASCII edge byte.
    #[inline(always)]
    fn trim_ascii_str(s: &str) -> &str {
        let b = s.as_bytes();
        let lo = b.first().copied().unwrap_or(0);
        let hi = b.last().copied().unwrap_or(0);
        if lo >= 0x80 || hi >= 0x80 {
            return s.trim();
        }
        s.trim_ascii()
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
                Ok(OpenHours::from_json_payload(value))
            }

            fn visit_borrowed_str<E>(self, value: &'de str) -> Result<OpenHours, E>
            where
                E: de::Error,
            {
                // serde_json hands a borrowed `&'de str` for an unescaped JSON string;
                // decode it through the same content-memoed single-clone path.
                Ok(OpenHours::from_json_payload(value))
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

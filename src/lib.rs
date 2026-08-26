use chrono::{DateTime, Datelike, Duration, NaiveDateTime, TimeZone, Timelike, Weekday};
use dashmap::DashMap;
use serde::de::{self, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::fmt;
use std::str::FromStr;
use std::sync::{Arc, LazyLock};

pub const MINUTES_PER_WEEK: usize = 7 * 24 * 60; // 10,080
pub const BITMASK_WORDS: usize = (MINUTES_PER_WEEK + 63) / 64; // 158

static INTERN_POOL: LazyLock<DashMap<String, Arc<OpenHours>>> = LazyLock::new(DashMap::new);

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

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct TimeWindow {
    pub start: u16,
    pub end: u16,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct OpenHours {
    raw: String,
    windows: Vec<TimeWindow>,
    bitmask: [u64; BITMASK_WORDS],
}

impl OpenHours {
    /// Parses an OSM opening_hours expression with global lock-free caching.
    pub fn parse(expression: &str) -> Arc<Self> {
        let trimmed = expression.trim();
        if trimmed.is_empty() {
            return Arc::clone(&EMPTY);
        }
        if trimmed.eq_ignore_ascii_case("24/7") {
            return Arc::clone(&ALWAYS_OPEN);
        }

        if let Some(cached) = INTERN_POOL.get(trimmed) {
            return Arc::clone(&cached);
        }

        let parsed = Arc::new(Self::parse_uncached(trimmed));
        INTERN_POOL.insert(trimmed.to_string(), Arc::clone(&parsed));
        parsed
    }

    /// Returns the raw OSM expression.
    #[inline]
    pub fn raw(&self) -> &str {
        &self.raw
    }

    /// Returns the slice of baked disjoint time windows.
    #[inline]
    pub fn windows(&self) -> &[TimeWindow] {
        &self.windows
    }

    /// Returns true if the schedule is 24/7.
    #[inline]
    pub fn is_always_open(&self) -> bool {
        self.windows.len() == 1
            && self.windows[0].start == 0
            && self.windows[0].end == MINUTES_PER_WEEK as u16
    }

    /// Returns true if the schedule is completely closed / empty.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.windows.is_empty()
    }

    /// $O(1)$ scalar hardware bit testing evaluating in ~0.5 - 1.0 nanoseconds.
    #[inline(always)]
    pub fn is_open<Tz: TimeZone>(&self, dt: &DateTime<Tz>) -> bool {
        if self.windows.is_empty() {
            return false;
        }
        if self.is_always_open() {
            return true;
        }

        let week_min = Self::get_week_minute(dt.weekday(), dt.hour(), dt.minute());
        let word = week_min >> 6;
        let mask = 1u64 << (week_min & 63);
        (self.bitmask[word] & mask) != 0
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

        let week_min = Self::get_week_minute(dt.weekday(), dt.hour(), dt.minute());
        let word = week_min >> 6;
        let mask = 1u64 << (week_min & 63);
        (self.bitmask[word] & mask) != 0
    }

    /// Alias for is_open.
    #[inline(always)]
    pub fn match_time<Tz: TimeZone>(&self, dt: &DateTime<Tz>) -> bool {
        self.is_open(dt)
    }

    /// Returns the duration until the next opening window.
    pub fn get_time_to_open<Tz: TimeZone>(&self, from: &DateTime<Tz>) -> Option<Duration> {
        if self.windows.is_empty() {
            return None;
        }
        if self.is_always_open() || self.is_open(from) {
            return Some(Duration::zero());
        }

        let week_min = Self::get_week_minute(from.weekday(), from.hour(), from.minute());
        let diff_min = self.find_next_open_minute(week_min)?;

        let sub_seconds = from.second() as i64;
        let sub_nanos = from.nanosecond() as i64;
        Some(
            Duration::minutes(diff_min as i64)
                - Duration::seconds(sub_seconds)
                - Duration::nanoseconds(sub_nanos),
        )
    }

    /// Returns the duration until an opening window with at least `required` continuous duration is available.
    pub fn get_time_to_open_for_duration<Tz: TimeZone>(
        &self,
        from: &DateTime<Tz>,
        required: Duration,
    ) -> Option<Duration> {
        if self.windows.is_empty() {
            return None;
        }
        let req_minutes = ((required.num_seconds() + 59) / 60).max(1) as usize;
        if req_minutes > MINUTES_PER_WEEK {
            return None;
        }
        if self.is_always_open() {
            return Some(Duration::zero());
        }

        let week_min = Self::get_week_minute(from.weekday(), from.hour(), from.minute());
        let diff_min = self.find_next_contiguous_open_minute(week_min, req_minutes)?;

        let sub_seconds = from.second() as i64;
        let sub_nanos = from.nanosecond() as i64;
        Some(
            Duration::minutes(diff_min as i64)
                - Duration::seconds(sub_seconds)
                - Duration::nanoseconds(sub_nanos),
        )
    }

    /// Returns the exact timestamp when a job of duration `duration` can start.
    pub fn when<Tz: TimeZone>(&self, from: &DateTime<Tz>, duration: Duration) -> Option<DateTime<Tz>> {
        let wait = self.get_time_to_open_for_duration(from, duration)?;
        Some(from.clone() + wait)
    }

    /// Returns the end timestamp of the current open shift.
    pub fn get_current_shift_end<Tz: TimeZone>(&self, dt: &DateTime<Tz>) -> Option<DateTime<Tz>> {
        if self.windows.is_empty() {
            return None;
        }
        if self.is_always_open() {
            return Some(dt.clone() + Duration::weeks(52));
        }
        if !self.is_open(dt) {
            return None;
        }

        let week_min = Self::get_week_minute(dt.weekday(), dt.hour(), dt.minute());
        let diff_min = self.find_current_shift_end_minute(week_min);

        let sub_seconds = dt.second() as i64;
        let sub_nanos = dt.nanosecond() as i64;
        Some(
            dt.clone()
                + Duration::minutes(diff_min as i64)
                - Duration::seconds(sub_seconds)
                - Duration::nanoseconds(sub_nanos),
        )
    }

    /// Returns (is_currently_open, duration_until_next_state_change).
    pub fn next_dur<Tz: TimeZone>(&self, dt: &DateTime<Tz>) -> (bool, Duration) {
        if self.windows.is_empty() {
            return (false, Duration::zero());
        }
        if self.is_always_open() {
            return (true, Duration::days(365));
        }

        let week_min = Self::get_week_minute(dt.weekday(), dt.hour(), dt.minute());
        let word = week_min >> 6;
        let mask = 1u64 << (week_min & 63);
        let currently_open = (self.bitmask[word] & mask) != 0;

        let diff_min = if currently_open {
            self.find_current_shift_end_minute(week_min)
        } else {
            self.find_next_open_minute(week_min).unwrap_or(0)
        };

        let sub_seconds = dt.second() as i64;
        let sub_nanos = dt.nanosecond() as i64;
        let dur = Duration::minutes(diff_min as i64)
            - Duration::seconds(sub_seconds)
            - Duration::nanoseconds(sub_nanos);
        (currently_open, dur)
    }

    /// Returns (is_currently_open, next_transition_timestamp).
    pub fn next_date<Tz: TimeZone>(&self, dt: &DateTime<Tz>) -> (bool, DateTime<Tz>) {
        let (is_open, dur) = self.next_dur(dt);
        (is_open, dt.clone() + dur)
    }

    #[inline(always)]
    fn get_week_minute(weekday: Weekday, hour: u32, minute: u32) -> usize {
        let day_idx = weekday.num_days_from_monday() as usize; // Monday = 0 .. Sunday = 6
        day_idx * 1440 + (hour as usize) * 60 + (minute as usize)
    }

    #[inline]
    fn find_next_open_minute(&self, start_week_minute: usize) -> Option<usize> {
        let mut target = start_week_minute;
        let mut count = 0;
        while count < MINUTES_PER_WEEK {
            let word_idx = target >> 6;
            let bit_idx = target & 63;
            let word_bits = if word_idx == BITMASK_WORDS - 1 {
                self.bitmask[word_idx] & ((1u64 << 32) - 1)
            } else {
                self.bitmask[word_idx]
            };
            let shifted = word_bits >> bit_idx;
            if shifted != 0 {
                let advance = shifted.trailing_zeros() as usize;
                count += advance;
                return if count < MINUTES_PER_WEEK {
                    Some(count)
                } else {
                    None
                };
            }
            let valid_in_word = if word_idx == BITMASK_WORDS - 1 { 32 } else { 64 };
            let step = valid_in_word - bit_idx;
            count += step;
            target = (target + step) % MINUTES_PER_WEEK;
        }
        None
    }

    #[inline]
    fn find_current_shift_end_minute(&self, start_week_minute: usize) -> usize {
        let mut target = start_week_minute;
        let mut count = 0;
        while count < MINUTES_PER_WEEK {
            let word_idx = target >> 6;
            let bit_idx = target & 63;
            let inv = if word_idx == BITMASK_WORDS - 1 {
                (!self.bitmask[word_idx]) & ((1u64 << 32) - 1)
            } else {
                !self.bitmask[word_idx]
            };
            let shifted = inv >> bit_idx;
            if shifted != 0 {
                let advance = shifted.trailing_zeros() as usize;
                count += advance;
                return if count <= MINUTES_PER_WEEK {
                    count
                } else {
                    MINUTES_PER_WEEK
                };
            }
            let valid_in_word = if word_idx == BITMASK_WORDS - 1 { 32 } else { 64 };
            let step = valid_in_word - bit_idx;
            count += step;
            target = (target + step) % MINUTES_PER_WEEK;
        }
        MINUTES_PER_WEEK
    }

    #[inline]
    fn find_next_contiguous_open_minute(
        &self,
        start_week_minute: usize,
        req_minutes: usize,
    ) -> Option<usize> {
        let mut count = 0;
        let mut target = start_week_minute;

        while count < MINUTES_PER_WEEK {
            let word = target >> 6;
            let mask = 1u64 << (target & 63);
            if (self.bitmask[word] & mask) == 0 {
                let wait = self.find_next_open_minute(target)?;
                count += wait;
                target = (target + wait) % MINUTES_PER_WEEK;
                if count >= MINUTES_PER_WEEK {
                    break;
                }
            }

            let shift_len = self.find_current_shift_end_minute(target);
            if shift_len >= req_minutes {
                return Some(count);
            }

            count += shift_len;
            target = (target + shift_len) % MINUTES_PER_WEEK;
        }
        None
    }

    fn parse_uncached(expression: &str) -> Self {
        let mut minutes = vec![false; MINUTES_PER_WEEK];
        let rules: Vec<&str> = expression.split(';').collect();

        for rule in rules {
            let mut r = rule.trim();
            if r.is_empty() {
                continue;
            }

            let lower = r.to_ascii_lowercase();
            let is_off = lower.contains("off") || lower.contains("closed");

            let cleaned = r
                .replace("off", "")
                .replace("OFF", "")
                .replace("closed", "")
                .replace("CLOSED", "");
            r = cleaned.trim();
            if r.is_empty() {
                continue;
            }

            let mut days = Vec::new();
            let mut time_intervals = Vec::new();

            if !Self::parse_rule_tokens(r, &mut days, &mut time_intervals) {
                continue;
            }

            if days.is_empty() && time_intervals.is_empty() {
                continue;
            }
            if days.is_empty() {
                for d in 0..7 {
                    days.push(d);
                }
            }
            if time_intervals.is_empty() {
                time_intervals.push((0, 1440));
            }

            for &day in &days {
                for &(start, end) in &time_intervals {
                    let start_min = day * 1440 + start;
                    if end > 1440 {
                        // Overnight shift
                        let actual_end = day * 1440 + end;
                        for m in start_min..actual_end {
                            minutes[m % MINUTES_PER_WEEK] = !is_off;
                        }
                    } else if start > end {
                        // Inverted overnight shift
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
        let mut win_list = Vec::new();
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

        OpenHours {
            raw: expression.to_string(),
            windows: win_list,
            bitmask: bm,
        }
    }

    fn parse_rule_tokens(
        rule: &str,
        days: &mut Vec<usize>,
        time_intervals: &mut Vec<(usize, usize)>,
    ) -> bool {
        for part in rule.split_whitespace() {
            let p = part.trim();
            if p.is_empty() {
                continue;
            }

            if let Some(first_char) = p.chars().next() {
                if first_char.is_ascii_digit() || first_char == '+' {
                    if !Self::parse_time_intervals(p, time_intervals) {
                        return false;
                    }
                } else if first_char.is_ascii_alphabetic() {
                    if !Self::parse_days(p, days) {
                        return false;
                    }
                } else {
                    return false;
                }
            }
        }
        true
    }

    fn parse_days(days_str: &str, days: &mut Vec<usize>) -> bool {
        for part in days_str.split(',') {
            let p = part.trim().to_ascii_lowercase();
            if p.is_empty() {
                continue;
            }

            if p.contains('-') {
                let range: Vec<&str> = p.split('-').collect();
                if range.len() == 2 {
                    if let (Some(d1), Some(mut d2)) =
                        (Self::parse_day(range[0]), Self::parse_day(range[1]))
                    {
                        if d2 < d1 {
                            d2 += 7;
                        }
                        for d in d1..=d2 {
                            let actual = d % 7;
                            if !days.contains(&actual) {
                                days.push(actual);
                            }
                        }
                    } else {
                        return false;
                    }
                } else {
                    return false;
                }
            } else if let Some(d) = Self::parse_day(&p) {
                if !days.contains(&d) {
                    days.push(d);
                }
            } else {
                return false;
            }
        }
        true
    }

    fn parse_day(day: &str) -> Option<usize> {
        match day {
            "mo" => Some(0),
            "tu" => Some(1),
            "we" => Some(2),
            "th" => Some(3),
            "fr" => Some(4),
            "sa" => Some(5),
            "su" => Some(6),
            _ => None,
        }
    }

    fn parse_time_intervals(time_str: &str, intervals: &mut Vec<(usize, usize)>) -> bool {
        for part in time_str.split(',') {
            let p = part.trim();
            if p.is_empty() {
                continue;
            }

            if let Some(stripped) = p.strip_suffix('+') {
                if let Some(start) = Self::parse_minute_of_day(stripped) {
                    intervals.push((start, 1440));
                } else {
                    return false;
                }
            } else if p.contains('-') {
                let range: Vec<&str> = p.split('-').collect();
                if range.len() == 2 {
                    if let (Some(start), Some(mut end)) = (
                        Self::parse_minute_of_day(range[0]),
                        Self::parse_minute_of_day(range[1]),
                    ) {
                        if end == 0 || end < start {
                            end += 1440;
                        }
                        intervals.push((start, end));
                    } else {
                        return false;
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

    fn parse_minute_of_day(time: &str) -> Option<usize> {
        let parts: Vec<&str> = time.split(':').collect();
        if parts.len() >= 2 {
            let h: usize = parts[0].trim().parse().ok()?;
            let m: usize = parts[1].trim().parse().ok()?;
            if h <= 24 && m < 60 {
                return Some(h * 60 + m);
            }
        }
        None
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

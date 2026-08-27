# Reference tests from opening_hours.js

`tests/reference_tests.rs` ports the point-in-time open/closed assertions from
the original
[opening_hours.js test suite](https://github.com/opening-hours/opening_hours.js/blob/main/test/test.js).

The reference suite is made up of `test.addTest(...)` (336), `addShouldFail`
(30), `addShouldWarn` (4), `addStructuredWarnings` (35), `addCompMatchingRule`
(5), `addPrettifyValue` (19), `addEqualTo` (6), and `addNextChangeTest` (3),
plus 14 `test/unit` cases. Under the "don't grow the public API" constraint,
only the interval-based `addTest` cases can be ported here, and only the ones
this parser already evaluates identically.

## What was ported

For every `test.addTest(...)` block in the reference suite whose expression
variants this parser accepts **and** evaluates to the **same open intervals**,
we assert `is_open` at: every expected-interval boundary (`start-1min`, `start`,
`start+1min`, midpoint, `end-1min`, `end`), the query-window start, and a coarse
daily grid inside the window.

That yields **48 portable reference cases** of which this repo covers **40**.
The exercising grammar is: simple + comma/`;`-separated time ranges, `off`/
`closed` overrides, always-closed schedules, overnight spans, day-selector
lists and wrap-around ranges, "no time means all day" selectors, 24h schedules
and `24/7`.

## What was NOT ported (outside this parser's grammar / the public API)

- open-end / variable end (`17:00+`, `sunset`, `dawn`, `14:00-sunset+`, …)
- am/pm, dot and unicode time-separator tolerance
- "additional"/"exception" rules that re-select the same weekday via `,`/`;`
- holidays (`PH`, `SH`) and region/*Nominatim* data
- variable days (month/week/day numbers, `Mo[1]`, `Jul-Aug`, `2025 Jul 27`)
- comments (`"..."`), `prettifyValue`, warnings, `isWeekStable`
- the `addShouldFail`/`addShouldWarn`/`addStructuredWarnings`/
  `addPrettifyValue`/`addEqualTo`/`addCompMatchingRule`/`addNextChangeTest`
  cases — they rely on APIs this library does not expose.

## Rust-specific exclusions

This parser does not implement the following, so those reference variants are
skipped (see the per-case `// NOTE:` comments in `tests/reference_tests.rs`):

- bare `open` anywhere (whole expression or inside a `;`-expression)
- `24/7` as a rule **inside** a `;`-separated expression (only a standalone
  `24/7` expression is recognized), e.g. `24/7; Mo 15:00-16:00 off`,
  `24/7; 24/7`, `12:00-13:00; 24/7`
- trailing `open` / `24/7` used as an all-day alias after a day selector
  (e.g. `Mo,We 24/7`, `Mo,We open`)
- the no-space form `We22:00-02:00` (day + time must be space-separated)

Other portable cases (`Weekdays`, `Omitted time`, `Full range` day selectors,
`Mo,We` / `Mo,We 00:00-24:00`) are fully covered.

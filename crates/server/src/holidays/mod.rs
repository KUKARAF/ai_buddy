//! Holiday / low-availability awareness for the LLM planner.
//!
//! This module turns the [`holidays`] crate into a *prompt block* that nudges an
//! LLM planner away from scheduling milestone due-dates on days when people are
//! realistically away or at reduced capacity.
//!
//! # Why "reduced availability" framing (and an explicit Dec 24–Jan 1 blackout)
//!
//! An experiment showed that handing the planner a *raw list of public holidays*
//! alone did **not** stop it from scheduling work straight across the Christmas /
//! New Year period. The reason is structural: in most countries **Dec 24 and
//! Dec 31 are not public holidays**, so they never appear in a holiday feed —
//! yet in practice almost nobody makes progress between Christmas Eve and New
//! Year's Day. A planner that only sees "Dec 25" and "Jan 1" happily parks a
//! deadline on Dec 29.
//!
//! Two fixes, both encoded here:
//!
//!  1. **Framing.** We present these dates as periods of *reduced availability*
//!     (lost working time to plan around), not as a neutral list of "holidays".
//!     The prompt explicitly tells the planner to avoid due-dates on *and
//!     adjacent to* them and to account for the lost time.
//!  2. **A universal year-end blackout.** On top of each country's real public
//!     holidays we always add a generic `Dec 24..=Dec 31` + `Jan 1` blackout for
//!     every year overlapping the window, regardless of country, labelled
//!     "Holiday season (reduced availability)". Real holiday names win on any
//!     date they coincide with.
//!
//! The public API is deliberately string-based (ISO country codes in, rendered
//! prompt text out) so that `chrono` — used internally for date math — does not
//! leak to callers, which elsewhere use the `time` crate.

use chrono::{Datelike, NaiveDate};
use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};

/// Label used for the universal year-end (Dec 24 – Jan 1) blackout.
const BLACKOUT_LABEL: &str = "Holiday season (reduced availability)";

/// Fallback loaded-year range used if the current year cannot be determined.
const FALLBACK_Y0: i32 = 2024;
const FALLBACK_Y1: i32 = 2032;

/// Records whether the global holiday DB was initialized successfully. We only
/// ever attempt initialization once (via `get_or_init`); if it fails we store
/// `false` and the functions degrade gracefully (still emitting the year-end
/// blackout, just without country-specific public holidays).
static INIT_OK: OnceLock<bool> = OnceLock::new();

/// Best-effort current (UTC) year, derived from the system clock without panicking.
fn current_year() -> Option<i32> {
    let secs = SystemTime::now().duration_since(UNIX_EPOCH).ok()?.as_secs();
    let secs = i64::try_from(secs).ok()?;
    let dt = chrono::DateTime::from_timestamp(secs, 0)?;
    Some(dt.year())
}

/// Year range to load into the holiday DB: `[current_year - 1, current_year + 7)`.
fn loaded_year_range() -> std::ops::Range<i32> {
    match current_year() {
        Some(y) => (y - 1)..(y + 7),
        None => FALLBACK_Y0..FALLBACK_Y1,
    }
}

/// Initialize the global holiday database exactly once (loading *all* countries
/// for the configured year range). Returns `true` if the DB is usable.
fn ensure_db() -> bool {
    *INIT_OK.get_or_init(|| {
        let range = loaded_year_range();
        // Omitting `.countries()` loads all ~100 countries; fine for ~8 years.
        holidays::Builder::new().years(range).init().is_ok()
    })
}

/// Map an ISO-3166-1 alpha-2 code (case-insensitive) to the holidays crate
/// [`holidays::Country`]. Returns `None` for unknown or malformed codes.
pub fn country_from_iso(code: &str) -> Option<holidays::Country> {
    let upper = code.trim().to_ascii_uppercase();
    if upper.is_empty() {
        return None;
    }
    // Preferred path: the crate implements FromStr over its known codes.
    if let Ok(country) = upper.parse::<holidays::Country>() {
        return Some(country);
    }
    // Fallback for the common codes, in case FromStr ever misses one.
    match upper.as_str() {
        "DE" => Some(holidays::Country::DE),
        "US" => Some(holidays::Country::US),
        "GB" => Some(holidays::Country::GB),
        "FR" => Some(holidays::Country::FR),
        "PL" => Some(holidays::Country::PL),
        "ES" => Some(holidays::Country::ES),
        "IT" => Some(holidays::Country::IT),
        "NL" => Some(holidays::Country::NL),
        "CA" => Some(holidays::Country::CA),
        "AU" => Some(holidays::Country::AU),
        _ => None,
    }
}

/// A single low-availability date with a human-readable label.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LowAvailability {
    pub date: NaiveDate,
    pub label: String,
}

/// All low-availability dates within `[start, end]` inclusive:
///
///  - every public holiday in-window for `country` (when `Some` and known),
///    labelled by its name;
///  - PLUS a universal "Holiday season (reduced availability)" blackout for
///    `Dec 24..=Dec 31` of every overlapping year and `Jan 1` of the following
///    year, regardless of country.
///
/// The result is sorted ascending and de-duplicated by date; where a real
/// public holiday and the generic blackout fall on the same day, the real
/// holiday's name wins.
pub fn low_availability(
    country: Option<holidays::Country>,
    start: NaiveDate,
    end: NaiveDate,
) -> Vec<LowAvailability> {
    use std::collections::BTreeMap;

    if start > end {
        return Vec::new();
    }

    // BTreeMap keeps dates sorted and unique; first insertion wins, so we insert
    // real (named) holidays before the generic blackout fallback.
    let mut by_date: BTreeMap<NaiveDate, String> = BTreeMap::new();

    // 1) Real public holidays (named) — highest priority.
    let db_ready = ensure_db();
    if db_ready {
        if let Some(country) = country {
            // `holidays::iter` treats `until` as EXCLUSIVE, so bump by one day to
            // make `end` inclusive. If `end` is the max representable date, fall
            // back to `end` (worst case: the final day's holiday is omitted).
            let until = end.succ_opt().unwrap_or(end);
            if let Ok(iter) = holidays::iter(country, start, until) {
                for holiday in iter {
                    if holiday.date >= start && holiday.date <= end {
                        by_date.entry(holiday.date).or_insert(holiday.name);
                    }
                }
            }
            // An Err (e.g. year outside loaded data) is treated as "no holidays".
        }
    }

    // 2) Universal year-end blackout: Dec 24..=31 of Y, plus Jan 1 of Y+1.
    //    Starting a year early picks up a January window whose blackout belongs
    //    to the previous calendar year's span.
    let y_lo = start.year() - 1;
    let y_hi = end.year();
    for y in y_lo..=y_hi {
        for day in 24u32..=31 {
            if let Some(date) = NaiveDate::from_ymd_opt(y, 12, day) {
                if date >= start && date <= end {
                    by_date
                        .entry(date)
                        .or_insert_with(|| BLACKOUT_LABEL.to_string());
                }
            }
        }
        if let Some(date) = NaiveDate::from_ymd_opt(y + 1, 1, 1) {
            if date >= start && date <= end {
                by_date
                    .entry(date)
                    .or_insert_with(|| BLACKOUT_LABEL.to_string());
            }
        }
    }

    by_date
        .into_iter()
        .map(|(date, label)| LowAvailability { date, label })
        .collect()
}

/// Render the planner prompt block. Returns `None` when `items` is empty.
///
/// The text frames the dates as *reduced availability*, instructs the planner to
/// avoid placing (or abutting) milestone due-dates on them, calls out the
/// Dec 24 – Jan 1 period explicitly, and discourages weekend deadlines.
pub fn prompt_block(items: &[LowAvailability]) -> Option<String> {
    if items.is_empty() {
        return None;
    }

    let mut lines = String::new();
    for item in items {
        // `NaiveDate`'s Display is ISO-8601 (YYYY-MM-DD).
        lines.push_str(&format!("- {} — {}\n", item.date, item.label));
    }

    let text = format!(
        "The dates below fall in periods of REDUCED AVAILABILITY — public \
holidays, holiday seasons, and other low-availability periods. Treat them as \
working time that is largely lost, not as neutral calendar notes:\n\
\n\
{lines}\n\
When building the plan:\n\
- Do NOT place any milestone due_date ON any of the dates above, and avoid \
scheduling a due_date immediately ADJACENT to them (the day before or the day \
after) — people are typically away or at reduced capacity around these dates.\n\
- Account for the lost time: because these are periods of reduced availability, \
spread the effort so that no deadline depends on work happening during them.\n\
- Explicitly treat Dec 24 – Jan 1 (the Christmas / New Year period) as a \
low-availability holiday period every year, even though Dec 24 and Dec 31 are \
not themselves public holidays — expect little to no progress across that whole \
span.\n\
- Avoid setting hard deadlines on weekends (Saturday or Sunday); move them to a \
nearby weekday instead.\n"
    );

    Some(text)
}

/// Convenience for callers: parse `today` / `deadline` (`"YYYY-MM-DD"`), build
/// the low-availability list for the ISO country, and render the prompt block.
/// Returns `None` if either date fails to parse or there is nothing to say.
pub fn prompt_block_for(iso_country: Option<&str>, today: &str, deadline: &str) -> Option<String> {
    let start = NaiveDate::parse_from_str(today.trim(), "%Y-%m-%d").ok()?;
    let end = NaiveDate::parse_from_str(deadline.trim(), "%Y-%m-%d").ok()?;
    let country = iso_country.and_then(country_from_iso);
    let items = low_availability(country, start, end);
    prompt_block(&items)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ymd(y: i32, m: u32, d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, d).expect("valid test date")
    }

    #[test]
    fn de_window_includes_christmas_and_blackout_days() {
        let start = ymd(2026, 10, 1);
        let end = ymd(2027, 1, 31);
        let items = low_availability(Some(holidays::Country::DE), start, end);
        let dates: Vec<NaiveDate> = items.iter().map(|i| i.date).collect();

        // Real public holiday.
        assert!(
            dates.contains(&ymd(2026, 12, 25)),
            "expected Christmas Day 2026-12-25"
        );
        // Generic year-end blackout days (not public holidays themselves).
        assert!(
            dates.contains(&ymd(2026, 12, 24)),
            "expected blackout 2026-12-24"
        );
        assert!(
            dates.contains(&ymd(2026, 12, 31)),
            "expected blackout 2026-12-31"
        );

        // On Christmas Day the *real* holiday name must win over the blackout.
        let xmas = items
            .iter()
            .find(|i| i.date == ymd(2026, 12, 25))
            .expect("christmas entry present");
        assert_ne!(
            xmas.label, BLACKOUT_LABEL,
            "real holiday name should win over the generic blackout"
        );
        assert!(!xmas.label.is_empty());
    }

    #[test]
    fn country_from_iso_is_case_insensitive_and_rejects_junk() {
        assert_eq!(country_from_iso("de"), Some(holidays::Country::DE));
        assert_eq!(country_from_iso("DE"), Some(holidays::Country::DE));
        assert_eq!(country_from_iso("Us"), Some(holidays::Country::US));
        assert_eq!(country_from_iso("  gb  "), Some(holidays::Country::GB));
        assert_eq!(country_from_iso("ZZ"), None);
        assert_eq!(country_from_iso(""), None);
    }

    #[test]
    fn prompt_block_empty_is_none() {
        assert!(prompt_block(&[]).is_none());
    }

    #[test]
    fn prompt_block_contains_required_framing() {
        let items = vec![LowAvailability {
            date: ymd(2026, 12, 25),
            label: "Christmas".to_string(),
        }];
        let block = prompt_block(&items).expect("non-empty yields text");
        let lower = block.to_lowercase();

        assert!(
            lower.contains("reduced availability"),
            "must frame as reduced availability"
        );
        assert!(
            lower.contains("dec 24") && lower.contains("jan 1"),
            "must call out the Dec 24 – Jan 1 period"
        );
        assert!(lower.contains("weekend"), "must mention weekends");
        // The listed date renders as "YYYY-MM-DD — label".
        assert!(
            block.contains("2026-12-25 — Christmas"),
            "must list each date as 'YYYY-MM-DD — label'"
        );
    }

    #[test]
    fn blackout_is_country_agnostic() {
        let start = ymd(2026, 12, 1);
        let end = ymd(2026, 12, 31);
        let items = low_availability(None, start, end);
        let dates: Vec<NaiveDate> = items.iter().map(|i| i.date).collect();

        assert!(
            dates.contains(&ymd(2026, 12, 24)),
            "year-end blackout applies with no country"
        );
        assert!(
            dates.contains(&ymd(2026, 12, 31)),
            "year-end blackout applies with no country"
        );
        // With no country there are no named public holidays, so every entry is
        // the generic blackout label.
        assert!(items.iter().all(|i| i.label == BLACKOUT_LABEL));
    }

    #[test]
    fn results_are_sorted_and_deduplicated() {
        let start = ymd(2026, 10, 1);
        let end = ymd(2027, 1, 31);
        let items = low_availability(Some(holidays::Country::DE), start, end);

        // Strictly ascending by date (also proves de-duplication).
        assert!(
            items
                .windows(2)
                .all(|w| matches!((w.first(), w.get(1)), (Some(a), Some(b)) if a.date < b.date)),
            "entries must be sorted ascending and unique by date"
        );

        // Belt and suspenders: every date is unique.
        let mut seen = std::collections::HashSet::new();
        for item in &items {
            assert!(seen.insert(item.date), "duplicate date {}", item.date);
        }
    }

    #[test]
    fn prompt_block_for_parses_and_renders() {
        let block = prompt_block_for(Some("DE"), "2026-10-01", "2027-01-31")
            .expect("should render for a December-spanning window");
        assert!(block.to_lowercase().contains("reduced availability"));

        // Unparseable dates yield None.
        assert!(prompt_block_for(Some("DE"), "nonsense", "2027-01-31").is_none());
        // A window with nothing to say (no holidays, no blackout) yields None.
        assert!(prompt_block_for(Some("DE"), "2026-06-01", "2026-06-05").is_none());
    }
}

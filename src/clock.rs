//! User-TZ-aware "now" and "today" for graf date math.
//!
//! Graf's todo machinery computes "today" for completion-log headers,
//! recurrence advancement, horizon filters, and timestamp formatting.
//! Running that math in process-local time is wrong when the server is
//! UTC and the user is in America/New_York — in the evening hours past
//! local midnight, the system's "today" is already tomorrow in the
//! user's reality.
//!
//! See `~/src/brenn/docs/designs/graf-user-tz.md` for the full design.
//!
//! # Environment
//!
//! - `GRAF_USER_TZ`: IANA zone name (e.g. `America/New_York`). The
//!   authoritative source when set. Invalid → panic.
//! - `GRAF_NOW_OVERRIDE`: RFC 3339 UTC timestamp. Test-only clock pin.
//!   Invalid → panic.
//! - Unset `GRAF_USER_TZ`: falls back to `iana_time_zone::get_timezone()`.
//!   If that also fails, panic with a message pointing at the env var.
//!
//! # LLM override
//!
//! All mutator entry points that use "today" as a default also accept
//! an explicit `today: Option<NaiveDate>` parameter. The LLM passes
//! this when it has better information than the server about the
//! user's local date (e.g. the user said they're traveling). `Clock`
//! provides the fallback.

use std::sync::Once;

use chrono::{DateTime, NaiveDate, TimeZone};
use chrono_tz::Tz;

/// A user-timezone-aware clock.
///
/// Built by `from_env()`; constructed per public graf entry point
/// (construction is cheap — one env-var lookup + a static-table `Tz`
/// parse, plus a `/etc/localtime` syscall on the `GRAF_USER_TZ`-absent
/// path).
pub struct Clock {
    tz: Tz,
    /// Optional RFC 3339 UTC instant that overrides the real clock.
    /// Only set when `GRAF_NOW_OVERRIDE` is present. Tests use this;
    /// production never sets it.
    override_now_utc: Option<DateTime<chrono::Utc>>,
}

/// Gates the "resolved timezone" log line to once per process.
static LOG_ONCE: Once = Once::new();

impl Clock {
    /// Build from environment variables. See module docs.
    ///
    /// # Panics
    ///
    /// - `GRAF_USER_TZ` present but not a parseable IANA zone.
    /// - `GRAF_USER_TZ` absent and `iana_time_zone::get_timezone()` fails.
    /// - `GRAF_USER_TZ` absent, system zone resolves to a string that
    ///   isn't in the chrono-tz database.
    /// - `GRAF_NOW_OVERRIDE` present but not parseable as RFC 3339.
    ///
    pub fn from_env() -> Self {
        Self::from_values(
            std::env::var("GRAF_USER_TZ").ok(),
            std::env::var("GRAF_NOW_OVERRIDE").ok(),
        )
    }

    /// Resolve a `Clock` from the raw `GRAF_USER_TZ` / `GRAF_NOW_OVERRIDE`
    /// values. All parsing, fallback, panic, and logging behavior lives here.
    ///
    /// # Panics
    ///
    /// See `from_env`.
    fn from_values(user_tz: Option<String>, now_override: Option<String>) -> Self {
        let (tz, source) = match user_tz {
            Some(name) => {
                let tz: Tz = name.parse().unwrap_or_else(|e| {
                    panic!("invalid GRAF_USER_TZ {name:?}: {e}");
                });
                (tz, "GRAF_USER_TZ")
            }
            None => {
                let sys_name = iana_time_zone::get_timezone().unwrap_or_else(|e| {
                    panic!(
                        "GRAF_USER_TZ not set and system timezone is unresolvable \
                         ({e}). Set GRAF_USER_TZ to an IANA zone name (e.g. \
                         America/New_York) to proceed."
                    );
                });
                let tz: Tz = sys_name.parse().unwrap_or_else(|e| {
                    panic!(
                        "system timezone {sys_name:?} is not a known IANA zone: {e}. \
                         Set GRAF_USER_TZ to a valid zone name to override."
                    );
                });
                (tz, "system")
            }
        };

        let override_now_utc = now_override.map(|s| {
            let dt = DateTime::parse_from_rfc3339(&s).unwrap_or_else(|e| {
                panic!("invalid GRAF_NOW_OVERRIDE {s:?}: {e}");
            });
            dt.with_timezone(&chrono::Utc)
        });

        // Only log when we actually fell back to the system zone — if
        // the caller explicitly set `GRAF_USER_TZ`, that's the boring
        // case (brenn sets this on every graf subprocess spawn; logging
        // it per-spawn into brenn's logs adds noise, not signal).
        // The fallback case is the one worth surfacing: it's the only
        // path where graf might be resolving a zone the operator
        // didn't intend.
        if source == "system" {
            LOG_ONCE.call_once(|| {
                // Graf has no tracing setup; stderr is the idiomatic channel
                // for informational diagnostics here.
                eprintln!(
                    "graf: GRAF_USER_TZ unset — using system timezone {}",
                    tz.name(),
                );
            });
        }

        Self {
            tz,
            override_now_utc,
        }
    }

    /// The current "today" as a `NaiveDate` in the user's zone.
    pub fn today(&self) -> NaiveDate {
        self.now().date_naive()
    }

    /// The current instant in the user's zone.
    ///
    /// Respects `GRAF_NOW_OVERRIDE` when set.
    pub fn now(&self) -> DateTime<Tz> {
        let utc_now = match self.override_now_utc {
            Some(ts) => ts,
            None => chrono::Utc::now(),
        };
        self.tz.from_utc_datetime(&utc_now.naive_utc())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn today_in_ny_is_yesterday_of_utc_after_utc_midnight() {
        // At 2026-04-17T02:00:00Z, it's still 2026-04-16 22:00 in NY.
        let clock = Clock::from_values(
            Some("America/New_York".into()),
            Some("2026-04-17T02:00:00Z".into()),
        );
        assert_eq!(clock.today(), NaiveDate::from_ymd_opt(2026, 4, 16).unwrap(),);
    }

    #[test]
    fn today_in_auckland_is_tomorrow_of_utc_before_utc_midnight() {
        // At 2026-04-16T14:00:00Z, it's already 2026-04-17 02:00 in Auckland
        // (UTC+12 typically).
        let clock = Clock::from_values(
            Some("Pacific/Auckland".into()),
            Some("2026-04-16T14:00:00Z".into()),
        );
        assert_eq!(clock.today(), NaiveDate::from_ymd_opt(2026, 4, 17).unwrap(),);
    }

    #[test]
    #[should_panic(expected = "invalid GRAF_USER_TZ")]
    fn invalid_user_tz_panics() {
        let _ = Clock::from_values(Some("Not/AZone".into()), None);
    }

    #[test]
    #[should_panic(expected = "invalid GRAF_NOW_OVERRIDE")]
    fn invalid_now_override_panics() {
        let _ = Clock::from_values(Some("UTC".into()), Some("not-a-timestamp".into()));
    }

    /// With no timezone input, the clock resolves the system zone via
    /// `iana_time_zone` and `today()` matches `chrono::Local::now().date_naive()`.
    /// Comparing against a before/after pair absorbs the local-midnight window.
    ///
    /// Turning an unset env var into `None` happens in `from_env`, which is
    /// deliberately uncovered: testing it requires mutating the process
    /// environment, which races every implicit `Clock::from_env()` reader in
    /// the suite.
    #[test]
    fn absent_user_tz_uses_system_zone() {
        let before = chrono::Local::now().date_naive();
        let clock = Clock::from_values(None, None);
        let got = clock.today();
        let after = chrono::Local::now().date_naive();
        // got must equal either before or after (covers the midnight-race case).
        assert!(
            got == before || got == after,
            "Clock::today()={got} not in [{before}, {after}]",
        );
    }

    #[test]
    fn now_respects_override() {
        let clock = Clock::from_values(Some("UTC".into()), Some("2026-04-17T02:00:00Z".into()));
        let now = clock.now();
        assert_eq!(
            now.naive_utc(),
            chrono::NaiveDate::from_ymd_opt(2026, 4, 17)
                .unwrap()
                .and_hms_opt(2, 0, 0)
                .unwrap(),
        );
    }
}

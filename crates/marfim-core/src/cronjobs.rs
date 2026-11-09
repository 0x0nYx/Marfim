//! W10.2 (§G, Hermes cron doctrine): scheduled jobs — natural-language
//! schedules, pause audit, missed-run catch-up, per-job model pinning,
//! and the `[SILENT]`/`[CRON_FAILURE]` markers.
//!
//! The FIVE schedule forms (exactly these parse; everything else fails
//! closed with the did-you-mean of the forms):
//!   1. `every N minutes`        (N ≥ 1)
//!   2. `every N hours`          (N ≥ 1)
//!   3. `daily at HH:MM`
//!   4. `weekdays at HH:MM`
//!   5. `weekly on <day> at HH:MM`  (<day> ∈ mon..sun)
//!
//! Time is unix MS UTC; `next_after(last_ms, now_ms)` is a pure
//! function — the runner owns the wall clock, the math owns the truth.
//! All timezone-less (UTC) — documented honestly (a TZ surface would
//! need the owner's IANA database; the forms carry no zone).

use serde::{Deserialize, Serialize};

/// Marker semantics (Hermes): a run whose prompt starts with `[SILENT]`
/// reports nothing on success; any failed run surfaces as
/// `[CRON_FAILURE]` in the journal/event — never silently dropped.
pub const SILENT_MARKER: &str = "[SILENT]";
pub const CRON_FAILURE_MARKER: &str = "[CRON_FAILURE]";

/// Catch-up cap: a missed job catches up ONCE per open (a backlog of N
/// missed runs never becomes N immediate runs).
pub const MAX_CATCHUP_PER_OPEN: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum Schedule {
    EveryMinutes(u64),
    EveryHours(u64),
    DailyAt { hour: u8, minute: u8 },
    WeekdaysAt { hour: u8, minute: u8 },
    WeeklyOn { day: u8, hour: u8, minute: u8 }, // day: 0=Mon..6=Sun
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CronJob {
    pub id: String,
    pub name: String,
    pub prompt: String,
    pub schedule: Schedule,
    /// per-job model pinning (None = the session default chain)
    pub model: Option<String>,
    /// paused with reason (the audit trail: Some(reason) = paused)
    pub paused: Option<String>,
    pub last_run_ms: Option<u64>,
    pub created_ms: u64,
    /// one-shot: the job disables itself after its first run
    pub one_shot: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ScheduleError(pub String);

impl std::fmt::Display for ScheduleError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "bad schedule: {}", self.0)
    }
}

/// Parse one of the five natural-language forms. Deterministic; errors
/// carry the accepted forms.
pub fn parse_schedule(s: &str) -> Result<Schedule, ScheduleError> {
    let s = s.trim().to_lowercase();
    let err = |hint: &str| {
        ScheduleError(format!(
            "{hint:?} — the five accepted forms are: \"every N minutes\" | \"every N hours\" | \
         \"daily at HH:MM\" | \"weekdays at HH:MM\" | \"weekly on <day> at HH:MM\""
        ))
    };
    if let Some(rest) = s.strip_prefix("every ") {
        let (n, unit) = rest.split_once(' ').ok_or_else(|| err(&s))?;
        let n: u64 = n.parse().map_err(|_| err(&s))?;
        if n == 0 {
            return Err(err("interval must be ≥ 1"));
        }
        match unit.trim_end_matches('s') {
            "minute" => return Ok(Schedule::EveryMinutes(n)),
            "hour" => return Ok(Schedule::EveryHours(n)),
            _ => return Err(err(&s)),
        }
    }
    for (prefix, mk) in [
        (
            "daily at ",
            (|h: u8, m: u8| Schedule::DailyAt { hour: h, minute: m }) as fn(u8, u8) -> Schedule,
        ),
        (
            "weekdays at ",
            (|h: u8, m: u8| Schedule::WeekdaysAt { hour: h, minute: m }) as fn(u8, u8) -> Schedule,
        ),
    ] {
        if let Some(clock) = s.strip_prefix(prefix) {
            let (h, m) = parse_clock(clock).ok_or_else(|| err(&s))?;
            return Ok(mk(h, m));
        }
    }
    if let Some(rest) = s.strip_prefix("weekly on ") {
        let (day, clock) = rest.split_once(" at ").ok_or_else(|| err(&s))?;
        let d = parse_day(day).ok_or_else(|| err(&s))?;
        let (h, m) = parse_clock(clock).ok_or_else(|| err(&s))?;
        return Ok(Schedule::WeeklyOn {
            day: d,
            hour: h,
            minute: m,
        });
    }
    Err(err(&s))
}

fn parse_clock(s: &str) -> Option<(u8, u8)> {
    let (h, m) = s.trim().split_once(':')?;
    let h: u8 = h.parse().ok()?;
    let m: u8 = m.parse().ok()?;
    if h > 23 || m > 59 {
        return None;
    }
    Some((h, m))
}

fn parse_day(s: &str) -> Option<u8> {
    match s.trim() {
        "mon" | "monday" => Some(0),
        "tue" | "tuesday" => Some(1),
        "wed" | "wednesday" => Some(2),
        "thu" | "thursday" => Some(3),
        "fri" | "friday" => Some(4),
        "sat" | "saturday" => Some(5),
        "sun" | "sunday" => Some(6),
        _ => None,
    }
}

/// Days-since-epoch → (y, m, d) (Hinnant; same math as the Bedrock wire).
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// (y, m, d) → days-since-epoch (Hinnant inverse). Used by the humane
/// timestamp renderer's round-trip test.
#[cfg(test)]
fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = (y - era * 400) as u64;
    let mp = if m > 2 { m - 3 } else { m + 9 } as u64;
    let doy = (153 * mp + 2) / 5 + d as u64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe as i64 - 719_468
}

/// Weekday of a unix-ms instant: 0=Monday..6=Sunday.
pub fn weekday_of(ms: u64) -> u8 {
    let days = (ms / 86_400_000) as i64;
    // 1970-01-01 was a Thursday (3 in Mon=0 indexing)
    (((days % 7) + 7 + 3) % 7) as u8
}

fn at_clock(ms: u64, hour: u8, minute: u8) -> u64 {
    let day_start = (ms / 86_400_000) * 86_400_000;
    day_start + (hour as u64) * 3_600_000 + (minute as u64) * 60_000
}

impl CronJob {
    /// Human-readable schedule form (deterministic; for the /cron list).
    pub fn schedule_humane(&self) -> String {
        match &self.schedule {
            Schedule::EveryMinutes(n) => format!("every {n} min"),
            Schedule::EveryHours(n) => format!("every {n} h"),
            Schedule::DailyAt { hour, minute } => format!("daily at {hour:02}:{minute:02} UTC"),
            Schedule::WeekdaysAt { hour, minute } => {
                format!("weekdays at {hour:02}:{minute:02} UTC")
            }
            Schedule::WeeklyOn { day, hour, minute } => {
                let d = ["mon", "tue", "wed", "thu", "fri", "sat", "sun"][*day as usize];
                format!("weekly on {d} at {hour:02}:{minute:02} UTC")
            }
        }
    }
}

impl CronBook {
    /// Load the persisted book; a missing or corrupt file reads as an
    /// empty book (the runner recreates it on the first save).
    pub fn load_best_effort(path: &std::path::Path) -> Self {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    /// Persist the book (create parents; the file IS the audit source
    /// for paused_reason).
    pub fn save(&self, path: &std::path::Path) -> crate::Result<()> {
        if let Some(p) = path.parent() {
            std::fs::create_dir_all(p)?;
        }
        std::fs::write(path, serde_json::to_string_pretty(self)?)?;
        Ok(())
    }
}

/// Human-readable unix-ms timestamp (UTC, minute resolution) for the
/// /cron surface. Deterministic civil math — no wall clock, no TZ db.
pub fn humane_unix_ms(ms: u64) -> String {
    let secs = ms / 1000;
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    let (y, m, d) = civil_from_days(days);
    format!(
        "{y:04}-{m:02}-{d:02} {:02}:{:02} UTC",
        rem / 3600,
        (rem % 3600) / 60
    )
}

impl Schedule {
    /// The next fire strictly AFTER `last_ms`, never before `now_ms`
    /// (a schedule that fell behind does not fire in the past — the
    /// catch-up decision belongs to the runner, not the math).
    pub fn next_after(&self, last_ms: u64, now_ms: u64) -> u64 {
        let floor = last_ms.max(now_ms);
        match self {
            Schedule::EveryMinutes(n) => {
                let step = n * 60_000;
                let last_slot = last_ms / step;
                let mut next = (last_slot + 1) * step;
                if next < floor {
                    next = ((floor / step) + 1) * step;
                }
                next
            }
            Schedule::EveryHours(n) => {
                let step = n * 3_600_000;
                let mut next = (last_ms / step + 1) * step;
                if next < floor {
                    next = ((floor / step) + 1) * step;
                }
                next
            }
            Schedule::DailyAt { hour, minute } => {
                let today = at_clock(floor, *hour, *minute);
                if today > floor {
                    today
                } else {
                    today + 86_400_000
                }
            }
            Schedule::WeekdaysAt { hour, minute } => {
                let mut cand = at_clock(floor, *hour, *minute);
                loop {
                    let wd = weekday_of(cand);
                    if wd <= 4 && cand > floor {
                        return cand;
                    }
                    cand += 86_400_000;
                }
            }
            Schedule::WeeklyOn { day, hour, minute } => {
                let mut cand = at_clock(floor, *hour, *minute);
                loop {
                    let wd = weekday_of(cand) as i64;
                    let want = *day as i64;
                    if wd == want && cand > floor {
                        return cand;
                    }
                    // step to the same clock tomorrow (wraps weeks naturally)
                    cand += 86_400_000;
                }
            }
        }
    }
}

/// The catch-up decision: given the job's persisted state, should the
/// runner fire a catch-up run NOW (and for which fire time)? A paused
/// job never catches up (the reason stays recorded); a job whose next
/// fire is in the past catches up AT MOST [`MAX_CATCHUP_PER_OPEN`] time
/// per open; one-shot jobs catch up only if they never ran.
pub fn catchup_decision(job: &CronJob, now_ms: u64, caught_up_this_open: u32) -> Option<u64> {
    if job.paused.is_some() || caught_up_this_open >= MAX_CATCHUP_PER_OPEN {
        return None;
    }
    if job.one_shot && job.last_run_ms.is_some() {
        return None;
    }
    let fire = job
        .schedule
        .next_after(job.last_run_ms.unwrap_or(0), now_ms);
    // next_after clamps to >= now; a catch-up is due only when the
    // schedule ACTUALLY fell behind (the unclamped next fire < now).
    let unclamped = match &job.schedule {
        Schedule::EveryMinutes(n) => {
            let step = n * 60_000;
            (job.last_run_ms.unwrap_or(0) / step + 1) * step
        }
        Schedule::EveryHours(n) => {
            let step = n * 3_600_000;
            (job.last_run_ms.unwrap_or(0) / step + 1) * step
        }
        _ => fire,
    };
    (unclamped <= now_ms).then_some(fire)
}

/// A tiny in-memory book (the persistence format is this struct as
/// JSON; the runner owns the file).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CronBook {
    pub jobs: Vec<CronJob>,
    next_id: u64,
}

impl CronBook {
    pub fn add(
        &mut self,
        name: &str,
        prompt: &str,
        schedule: Schedule,
        model: Option<String>,
        now_ms: u64,
        one_shot: bool,
    ) -> String {
        self.next_id += 1;
        let id = format!("job{}", self.next_id);
        self.jobs.push(CronJob {
            id: id.clone(),
            name: name.to_string(),
            prompt: prompt.to_string(),
            schedule,
            model,
            paused: None,
            last_run_ms: None,
            created_ms: now_ms,
            one_shot,
        });
        id
    }

    pub fn pause(&mut self, id: &str, reason: &str) -> bool {
        match self.jobs.iter_mut().find(|j| j.id == id) {
            Some(j) => {
                j.paused = Some(if reason.trim().is_empty() {
                    "no reason given".into()
                } else {
                    reason.trim().to_string()
                });
                true
            }
            None => false,
        }
    }

    pub fn resume(&mut self, id: &str) -> bool {
        match self.jobs.iter_mut().find(|j| j.id == id) {
            Some(j) => {
                j.paused = None;
                true
            }
            None => false,
        }
    }

    pub fn mark_ran(&mut self, id: &str, now_ms: u64) {
        if let Some(j) = self.jobs.iter_mut().find(|j| j.id == id) {
            j.last_run_ms = Some(now_ms);
            if j.one_shot {
                j.paused = Some("one-shot job completed".into());
            }
        }
    }

    /// Jobs due at `now_ms` (not paused, next fire ≤ now), with their
    /// fire time. Deterministic order = book order.
    pub fn due(&self, now_ms: u64, caught_up: u32) -> Vec<(String, u64)> {
        self.jobs
            .iter()
            .filter_map(|j| catchup_decision(j, now_ms, caught_up).map(|f| (j.id.clone(), f)))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // 2026-01-05 is a Monday (epoch day 20454 = 2026-01-01, a Thursday).
    const MON_NOON: u64 = 20458 * 86_400_000 + 12 * 3_600_000;

    #[test]
    fn weekday_math_matches_the_gregorian_truth() {
        assert_eq!(weekday_of(MON_NOON), 0, "2026-01-05 is a Monday");
        assert_eq!(weekday_of(MON_NOON + 86_400_000), 1);
        assert_eq!(weekday_of(MON_NOON + 6 * 86_400_000), 6, "Sunday");
        // 1970-01-01 was a Thursday
        assert_eq!(weekday_of(0), 3);
    }

    #[test]
    fn parses_all_five_forms_and_nothing_else() {
        assert_eq!(
            parse_schedule("every 5 minutes"),
            Ok(Schedule::EveryMinutes(5))
        );
        assert_eq!(parse_schedule("every 2 hours"), Ok(Schedule::EveryHours(2)));
        assert_eq!(
            parse_schedule("Daily at 09:30"),
            Ok(Schedule::DailyAt {
                hour: 9,
                minute: 30
            })
        );
        assert_eq!(
            parse_schedule("weekdays at 08:00"),
            Ok(Schedule::WeekdaysAt { hour: 8, minute: 0 })
        );
        assert_eq!(
            parse_schedule("weekly on fri at 17:45"),
            Ok(Schedule::WeeklyOn {
                day: 4,
                hour: 17,
                minute: 45
            })
        );
        // failures carry the forms hint
        let e = parse_schedule("every fortnight").unwrap_err();
        assert!(e.0.contains("five accepted forms"));
        assert!(parse_schedule("every 0 minutes").is_err());
        assert!(parse_schedule("daily at 24:00").is_err());
        assert!(parse_schedule("weekly on funday at 10:00").is_err());
        assert!(parse_schedule("at noon").is_err());
    }

    #[test]
    fn interval_next_after_is_strictly_increasing() {
        let s = Schedule::EveryMinutes(15);
        let t0 = 1_800_000; // an exact 15-min slot
        assert_eq!(s.next_after(t0, t0), t0 + 900_000);
        // a job that fell way behind does not fire in the past
        let now = t0 + 10 * 900_000;
        let next = s.next_after(t0, now);
        assert!(next >= now);
        assert_eq!(next % 900_000, 0, "stays on the slot grid");
    }

    #[test]
    fn daily_next_skips_to_tomorrow_when_passed() {
        let s = Schedule::DailyAt {
            hour: 9,
            minute: 30,
        };
        // MON_NOON is past 09:30 → next is tomorrow 09:30 (noon + 21.5h)
        let next = s.next_after(0, MON_NOON);
        assert_eq!(next, MON_NOON + 86_400_000 - 2 * 3_600_000 - 30 * 60_000);
        // before 09:30 today → today 09:30
        let early = MON_NOON - 4 * 3_600_000;
        assert_eq!(s.next_after(0, early), early + 3_600_000 + 30 * 60_000);
    }

    #[test]
    fn weekdays_next_skips_weekends() {
        let s = Schedule::WeekdaysAt {
            hour: 10,
            minute: 0,
        };
        // Friday 12:00 → next is Monday 10:00
        let fri = MON_NOON + 4 * 86_400_000;
        let next = s.next_after(0, fri);
        assert_eq!(next, fri + 3 * 86_400_000 - 2 * 3_600_000);
        assert_eq!(weekday_of(next), 0);
    }

    #[test]
    fn weekly_next_lands_on_the_named_day() {
        let s = Schedule::WeeklyOn {
            day: 4,
            hour: 17,
            minute: 45,
        }; // Friday
        let next = s.next_after(0, MON_NOON);
        assert_eq!(weekday_of(next), 4);
        assert!(next > MON_NOON);
        // from that Friday, the next is +7 days
        let next2 = s.next_after(next, next);
        assert_eq!(next2 - next, 7 * 86_400_000);
    }

    #[test]
    fn pause_audit_records_reason_and_resume_clears() {
        let mut b = CronBook::default();
        let id = b.add(
            "nightly",
            "run the checks",
            Schedule::DailyAt { hour: 3, minute: 0 },
            Some("deepseek-chat".into()),
            10,
            false,
        );
        assert!(b.pause(&id, "provider incident"));
        assert_eq!(b.jobs[0].paused.as_deref(), Some("provider incident"));
        assert!(b.due(10_000_000, 0).is_empty(), "paused jobs never fire");
        assert!(b.pause(&id, ""));
        assert_eq!(b.jobs[0].paused.as_deref(), Some("no reason given"));
        assert!(b.resume(&id));
        assert_eq!(b.jobs[0].paused, None);
        // per-job model pinning carried through
        assert_eq!(b.jobs[0].model.as_deref(), Some("deepseek-chat"));
        // unknown ids refused
        assert!(!b.pause("nope", "r"));
        assert!(!b.resume("nope"));
    }

    #[test]
    fn catchup_fires_once_and_not_for_paused_or_spent_one_shots() {
        let mut b = CronBook::default();
        let id = b.add("n", "p", Schedule::EveryMinutes(10), None, 0, false);
        b.mark_ran(&id, 100);
        // now far past the next fire → catch up
        let due = b.due(100 + 60 * 60_000, 0);
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].0, id);
        // the SECOND catch-up in the same open is refused
        assert!(b.due(100 + 60 * 60_000, 1).is_empty());
        // one-shot that already ran never catches up
        let mut b2 = CronBook::default();
        let o = b2.add("o", "p", Schedule::EveryMinutes(10), None, 0, true);
        b2.mark_ran(&o, 100);
        assert!(b2.due(999_999_999, 0).is_empty());
        // mark_ran retires a one-shot with an honest pause reason
        assert_eq!(b2.jobs[0].paused.as_deref(), Some("one-shot job completed"));
    }

    #[test]
    fn fresh_job_without_last_run_catches_up_from_creation() {
        let mut b = CronBook::default();
        let id = b.add("n", "p", Schedule::EveryMinutes(10), None, 0, false);
        let due = b.due(60 * 60_000, 0);
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].0, id);
    }

    #[test]
    fn markers_are_exact() {
        assert_eq!(SILENT_MARKER, "[SILENT]");
        assert_eq!(CRON_FAILURE_MARKER, "[CRON_FAILURE]");
    }

    #[test]
    fn civil_round_trip_matches_bedrock_vector() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(20_475), (2026, 1, 22));
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(days_from_civil(2026, 1, 22), 20_475);
        assert_eq!(days_from_civil(2024, 1, 9), 19_731);
    }
}

//! Pure logic for the Focus timer, Water reminder and To-do list (std only, so it
//! can be unit-tested on its own:
//!     rustc --edition 2021 --test src/features.rs -o /tmp/features_test && /tmp/features_test

use std::time::{Duration, Instant};

// ---------------------------------------------------------------------------
// Focus timer (Pomodoro)
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Phase {
    Focus,
    Break,
}

pub struct FocusTimer {
    pub phase: Phase,
    pub focus_min: u32,
    pub break_min: u32,
    pub running: bool,
    pub sessions: u32,
    end: Option<Instant>,
    left: Duration,
}

impl FocusTimer {
    pub fn new() -> Self {
        Self {
            phase: Phase::Focus,
            focus_min: 25,
            break_min: 5,
            running: false,
            sessions: 0,
            end: None,
            left: Duration::from_secs(25 * 60),
        }
    }

    pub fn total(&self) -> Duration {
        let m = match self.phase {
            Phase::Focus => self.focus_min,
            Phase::Break => self.break_min,
        };
        Duration::from_secs(m as u64 * 60)
    }

    pub fn remaining(&self) -> Duration {
        match (self.running, self.end) {
            (true, Some(e)) => e.saturating_duration_since(Instant::now()),
            _ => self.left,
        }
    }

    /// 0.0 = just started, 1.0 = finished.
    pub fn progress(&self) -> f32 {
        let t = self.total().as_secs_f32();
        if t <= 0.0 {
            return 0.0;
        }
        (1.0 - self.remaining().as_secs_f32() / t).clamp(0.0, 1.0)
    }

    pub fn start_pause(&mut self) {
        if self.running {
            self.left = self.remaining();
            self.running = false;
            self.end = None;
        } else {
            if self.left.is_zero() {
                self.left = self.total();
            }
            self.end = Some(Instant::now() + self.left);
            self.running = true;
        }
    }

    pub fn reset(&mut self) {
        self.running = false;
        self.end = None;
        self.left = self.total();
    }

    pub fn set_phase(&mut self, phase: Phase) {
        self.phase = phase;
        self.reset();
    }

    /// Changes the length of the current phase (only while it isn't running).
    pub fn adjust_minutes(&mut self, delta: i32) {
        if self.running {
            return;
        }
        let slot = match self.phase {
            Phase::Focus => &mut self.focus_min,
            Phase::Break => &mut self.break_min,
        };
        *slot = (*slot as i32 + delta).clamp(1, 120) as u32;
        self.left = self.total();
    }

    /// Call every frame. Returns the phase that has just finished, if any; the timer
    /// then switches to the other phase (paused, ready for the next Start).
    pub fn tick(&mut self) -> Option<Phase> {
        if self.running && self.remaining().is_zero() {
            let done = self.phase;
            if done == Phase::Focus {
                self.sessions += 1;
            }
            self.phase = if done == Phase::Focus {
                Phase::Break
            } else {
                Phase::Focus
            };
            self.reset();
            return Some(done);
        }
        None
    }

    /// Test helper: pretend `secs` seconds are left.
    #[cfg(test)]
    fn force_left(&mut self, secs: u64) {
        self.left = Duration::from_secs(secs);
        if self.running {
            self.end = Some(Instant::now() + self.left);
        }
    }
}

pub fn fmt_mmss(d: Duration) -> String {
    let s = d.as_secs() + if d.subsec_millis() > 0 { 1 } else { 0 };
    format!("{:02}:{:02}", s / 60, s % 60)
}

// ---------------------------------------------------------------------------
// Water
// ---------------------------------------------------------------------------

pub struct Water {
    pub goal_ml: u32,
    pub bottle_ml: u32,
    pub drank_ml: u32,
    pub interval_min: u32,
    pub from_hour: u32,
    pub to_hour: u32,
    /// "YYYY-MM-DD" the `drank_ml` counter belongs to.
    pub date: String,
    last_remind: Instant,
}

impl Water {
    pub fn new(today: &str) -> Self {
        Self {
            goal_ml: 2000,
            bottle_ml: 500,
            drank_ml: 0,
            interval_min: 60,
            from_hour: 9,
            to_hour: 21,
            date: today.to_string(),
            last_remind: Instant::now(),
        }
    }

    /// Starts a new day's count when the date changes.
    pub fn roll_over(&mut self, today: &str) -> bool {
        if self.date != today {
            self.date = today.to_string();
            self.drank_ml = 0;
            return true;
        }
        false
    }

    pub fn drink(&mut self) {
        self.drank_ml += self.bottle_ml;
        self.last_remind = Instant::now();
    }

    pub fn undo(&mut self) {
        self.drank_ml = self.drank_ml.saturating_sub(self.bottle_ml);
    }

    pub fn fraction(&self) -> f32 {
        if self.goal_ml == 0 {
            return 0.0;
        }
        (self.drank_ml as f32 / self.goal_ml as f32).clamp(0.0, 1.0)
    }

    /// True when a reminder is due now (inside the active hours, goal not reached yet,
    /// and `interval_min` minutes since the last drink/reminder).
    pub fn reminder_due(&mut self, hour: u32) -> bool {
        let in_hours = hour >= self.from_hour && hour < self.to_hour;
        let waited =
            self.last_remind.elapsed() >= Duration::from_secs(self.interval_min as u64 * 60);
        if in_hours && waited && self.drank_ml < self.goal_ml {
            self.last_remind = Instant::now();
            return true;
        }
        false
    }
}

// ---------------------------------------------------------------------------
// To-do
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
pub struct Task {
    pub text: String,
    /// "YYYY-MM-DDTHH:MM" local time, lexicographically comparable.
    pub due: Option<String>,
    pub done: bool,
    pub notified: bool,
}

/// Splits "call mom 14:30" / "pay rent at 9:05pm" into (text, Some((hour, minute))).
/// Only a time at the very end of the input counts.
pub fn parse_task_input(input: &str) -> (String, Option<(u32, u32)>) {
    let words: Vec<&str> = input.split_whitespace().collect();
    let Some(last) = words.last() else {
        return (String::new(), None);
    };
    let lower = last.to_ascii_lowercase();
    let (digits, pm, am) = if let Some(d) = lower.strip_suffix("pm") {
        (d.to_string(), true, false)
    } else if let Some(d) = lower.strip_suffix("am") {
        (d.to_string(), false, true)
    } else {
        (lower.clone(), false, false)
    };
    let parsed = digits.split_once(':').and_then(|(h, m)| {
        let h: u32 = h.parse().ok()?;
        let m: u32 = m.parse().ok()?;
        Some((h, m))
    });
    if let Some((mut h, m)) = parsed {
        if pm || am {
            if h == 0 || h > 12 {
                return (input.trim().to_string(), None);
            }
            h %= 12;
            if pm {
                h += 12;
            }
        }
        if h < 24 && m < 60 {
            let mut rest = words[..words.len() - 1].to_vec();
            // drop a dangling "at"
            if rest.last().map_or(false, |w| w.eq_ignore_ascii_case("at")) {
                rest.pop();
            }
            return (rest.join(" "), Some((h, m)));
        }
    }
    (input.trim().to_string(), None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_input_parsing() {
        assert_eq!(parse_task_input("buy milk"), ("buy milk".into(), None));
        assert_eq!(
            parse_task_input("call mom 14:30"),
            ("call mom".into(), Some((14, 30)))
        );
        assert_eq!(
            parse_task_input("pay rent at 9:05pm"),
            ("pay rent".into(), Some((21, 5)))
        );
        assert_eq!(
            parse_task_input("standup 12:00am"),
            ("standup".into(), Some((0, 0)))
        );
        assert_eq!(parse_task_input("noon 12:00pm").1, Some((12, 0)));
        assert_eq!(parse_task_input("bad 25:00"), ("bad 25:00".into(), None));
        assert_eq!(parse_task_input("   "), (String::new(), None));
        assert_eq!(
            parse_task_input("ratio 3:2"),
            ("ratio".into(), Some((3, 2)))
        );
    }

    #[test]
    fn focus_timer_cycle() {
        let mut t = FocusTimer::new();
        assert_eq!(t.remaining(), Duration::from_secs(25 * 60));
        assert!(t.tick().is_none());
        t.start_pause();
        assert!(t.running);
        t.force_left(0);
        assert_eq!(t.tick(), Some(Phase::Focus));
        assert_eq!(t.phase, Phase::Break);
        assert!(!t.running);
        assert_eq!(t.sessions, 1);
        assert_eq!(t.remaining(), Duration::from_secs(5 * 60));
        t.start_pause();
        t.force_left(0);
        assert_eq!(t.tick(), Some(Phase::Break));
        assert_eq!(t.phase, Phase::Focus);
        assert_eq!(t.sessions, 1);
    }

    #[test]
    fn focus_timer_pause_and_adjust() {
        let mut t = FocusTimer::new();
        t.adjust_minutes(5);
        assert_eq!(t.focus_min, 30);
        t.adjust_minutes(-100);
        assert_eq!(t.focus_min, 1);
        t.start_pause();
        t.adjust_minutes(5); // ignored while running
        assert_eq!(t.focus_min, 1);
        t.start_pause();
        assert!(!t.running);
        assert!(t.remaining() <= Duration::from_secs(60));
        assert_eq!(fmt_mmss(Duration::from_secs(61)), "01:01");
        assert_eq!(fmt_mmss(Duration::from_millis(59_100)), "01:00");
    }

    #[test]
    fn water_logic() {
        let mut w = Water::new("2026-10-20");
        w.drink();
        assert_eq!(w.drank_ml, 500);
        assert!((w.fraction() - 0.25).abs() < 1e-6);
        w.undo();
        w.undo();
        assert_eq!(w.drank_ml, 0);
        w.drank_ml = 900;
        assert!(!w.roll_over("2026-10-20"));
        assert!(w.roll_over("2026-10-21"));
        assert_eq!(w.drank_ml, 0);
        // not due right after creation
        assert!(!w.reminder_due(12));
        w.interval_min = 0;
        assert!(!w.reminder_due(3)); // outside active hours
        assert!(w.reminder_due(12));
        w.drank_ml = w.goal_ml;
        assert!(!w.reminder_due(12)); // goal reached
    }
}

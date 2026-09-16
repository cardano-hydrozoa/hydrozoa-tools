//! Alert rules: turn the current state into a ranked list of things an
//! operator should act on. Evaluated on every draw (pure reads, cheap).
//!
//! Every alert carries a stable `key` naming the condition, separate from
//! the display text. Transition tracking (raised/cleared, durations) diffs
//! on the key, so a live counter in the text does not flap the alert.

use crate::config::Config;
use crate::state::App;
use chrono::Utc;

pub struct Alert {
    pub key: String,
    pub crit: bool,
    pub text: String,
}

fn warn(key: String, text: String) -> Alert {
    Alert {
        key,
        crit: false,
        text,
    }
}

fn crit(key: String, text: String) -> Alert {
    Alert {
        key,
        crit: true,
        text,
    }
}

pub fn compute(app: &App, cfg: &Config) -> Vec<Alert> {
    let mut out = Vec::new();
    let now = Utc::now();

    for h in &app.heads {
        if h.reachable == Some(false) {
            out.push(crit(
                format!("{}:unreachable", h.name),
                format!("{}: unreachable", h.name),
            ));
            continue;
        }
        if h.reachable.is_none() {
            continue; // not polled yet — unknown, not down
        }
        match h.ready.as_deref() {
            Some("active") | None => {}
            Some(other) => out.push(crit(
                format!("{}:status", h.name),
                format!("{}: status {}", h.name, other),
            )),
        }
        if let Some(s) = &h.stats {
            // Liveness only: how long since the last hard confirmation. This is
            // NOT a fallback proxy — actual fallback risk is the fallback_at
            // alert below, which reads the head's real deadline and so scales
            // with the head's window automatically. These thresholds are absolute
            // wall-clock, tuned to the expected stack cadence, not the fallback
            // window, so a longer fallback regime does not make them fire early.
            let hard_age = s.stacks.seconds_since_last_hard_confirm;
            if s.stacks.total > 0 || hard_age > 0 {
                let key = format!("{}:hard-confirm", h.name);
                if hard_age >= cfg.hard_confirm_crit_secs {
                    out.push(crit(
                        key,
                        format!("{}: no hard confirmation for {}s", h.name, hard_age),
                    ));
                } else if hard_age >= cfg.hard_confirm_warn_secs {
                    out.push(warn(
                        key,
                        format!("{}: no hard confirmation for {}s", h.name, hard_age),
                    ));
                }
            }
            // Backpressure window nearly exhausted (capacity ~= max headroom seen).
            if h.max_headroom > 0 {
                let ratio = s.sequencer_headroom as f64 / h.max_headroom as f64;
                let key = format!("{}:headroom", h.name);
                if ratio < 0.02 {
                    out.push(crit(
                        key,
                        format!(
                            "{}: sequencer headroom exhausted ({})",
                            h.name, s.sequencer_headroom
                        ),
                    ));
                } else if ratio < 0.10 {
                    out.push(warn(
                        key,
                        format!(
                            "{}: sequencer headroom low ({} of {})",
                            h.name, s.sequencer_headroom, h.max_headroom
                        ),
                    ));
                }
            }
            if let Some(at) = h.backpressure_seen_at
                && at.elapsed().as_secs() < 60
            {
                out.push(warn(
                    format!("{}:backpressure", h.name),
                    format!(
                        "{}: backpressure rejections in the last minute (total {})",
                        h.name, s.local_requests.rejected_backpressure
                    ),
                ));
            }
        }
        if let Some(b) = &h.latest_block
            && let Some(fb) = b.fallback_at
        {
            let secs_left = (fb - now).num_seconds();
            let key = format!("{}:fallback", h.name);
            if secs_left <= cfg.fallback_crit_secs {
                out.push(crit(
                    key,
                    format!(
                        "{}: fallback tx window opens in {} — head at risk of closing",
                        h.name,
                        fmt_secs(secs_left)
                    ),
                ));
            } else if secs_left <= cfg.fallback_warn_secs {
                out.push(warn(
                    key,
                    format!(
                        "{}: fallback tx window opens in {}",
                        h.name,
                        fmt_secs(secs_left)
                    ),
                ));
            }
        }
    }

    out.sort_by_key(|a| !a.crit);
    out
}

fn fmt_secs(s: i64) -> String {
    if s < 0 {
        return "now".to_string();
    }
    let (h, m, sec) = (s / 3600, (s % 3600) / 60, s % 60);
    if h > 0 {
        format!("{h}h{m:02}m")
    } else if m > 0 {
        format!("{m}m{sec:02}s")
    } else {
        format!("{sec}s")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::state::{App, HeadStats, StackStats};

    fn cfg() -> Config {
        Config {
            heads: Vec::new(),
            transparent_background: false,
            hard_confirm_warn_secs: 1800,
            hard_confirm_crit_secs: 3600,
            fallback_warn_secs: 1800,
            fallback_crit_secs: 600,
        }
    }

    /// Only the three fields these rules read; everything else stays default.
    fn stats(hard_age: i64, stacks_total: u64, headroom: i64) -> HeadStats {
        HeadStats {
            stacks: StackStats {
                total: stacks_total,
                seconds_since_last_hard_confirm: hard_age,
                ..Default::default()
            },
            sequencer_headroom: headroom,
            ..Default::default()
        }
    }

    /// One head, reachable and active, with whatever stats the case needs.
    fn app_with(stats: Option<HeadStats>) -> App {
        let mut app = App::new(vec!["head-0".into()]);
        app.heads[0].reachable = Some(true);
        app.heads[0].ready = Some("active".into());
        app.heads[0].stats = stats;
        app
    }

    fn keys(alerts: &[Alert]) -> Vec<&str> {
        alerts.iter().map(|a| a.key.as_str()).collect()
    }

    #[test]
    fn a_healthy_head_raises_nothing() {
        assert!(compute(&app_with(Some(HeadStats::default())), &cfg()).is_empty());
    }

    /// A head that has never been polled is unknown, not down. Reporting it as
    /// down would make every start of hztop flash a critical alert.
    #[test]
    fn an_unpolled_head_is_not_reported_as_down() {
        let app = App::new(vec!["head-0".into()]);
        assert!(app.heads[0].reachable.is_none());
        assert!(compute(&app, &cfg()).is_empty());
    }

    #[test]
    fn an_unreachable_head_is_critical() {
        let mut app = app_with(None);
        app.heads[0].reachable = Some(false);
        let out = compute(&app, &cfg());
        assert_eq!(keys(&out), vec!["head-0:unreachable"]);
        assert!(out[0].crit);
    }

    /// Unreachable short-circuits: a head we cannot reach has stale stats, and
    /// deriving further alerts from them would report yesterday's state as now.
    #[test]
    fn an_unreachable_head_raises_nothing_else() {
        let mut app = app_with(Some(stats(99_999, 1, 0)));
        app.heads[0].reachable = Some(false);
        assert_eq!(compute(&app, &cfg()).len(), 1);
    }

    #[test]
    fn a_head_not_active_is_critical() {
        let mut app = app_with(Some(HeadStats::default()));
        app.heads[0].ready = Some("initializing".into());
        assert_eq!(keys(&compute(&app, &cfg())), vec!["head-0:status"]);
    }

    #[test]
    fn hard_confirmation_age_escalates_through_warn_to_crit() {
        for (age, want_crit) in [(1799, None), (1800, Some(false)), (3600, Some(true))] {
            let out = compute(&app_with(Some(stats(age, 1, 0))), &cfg());
            match want_crit {
                None => assert!(out.is_empty(), "age {age} should raise nothing"),
                Some(crit) => {
                    assert_eq!(keys(&out), vec!["head-0:hard-confirm"], "age {age}");
                    assert_eq!(out[0].crit, crit, "age {age}");
                }
            }
        }
    }

    /// A head with no stacks yet and no elapsed time has not missed anything.
    /// Without this guard a fresh head reports a hard-confirm alert at boot.
    #[test]
    fn a_head_with_no_stacks_yet_raises_no_hard_confirm_alert() {
        let out = compute(&app_with(Some(HeadStats::default())), &cfg());
        assert!(!keys(&out).contains(&"head-0:hard-confirm"));
    }

    /// Headroom is judged against the most ever seen, so it means nothing
    /// until a baseline exists -- otherwise a head at boot reads as exhausted.
    #[test]
    fn headroom_is_not_judged_before_a_baseline_exists() {
        let mut app = app_with(Some(HeadStats::default()));
        app.heads[0].max_headroom = 0;
        assert!(!keys(&compute(&app, &cfg())).contains(&"head-0:headroom"));
    }

    #[test]
    fn headroom_escalates_as_the_window_fills() {
        for (headroom, want) in [(500, None), (50, Some(false)), (5, Some(true))] {
            let mut app = app_with(Some(stats(0, 0, headroom)));
            app.heads[0].max_headroom = 1000;
            let out = compute(&app, &cfg());
            let found = out.iter().find(|a| a.key == "head-0:headroom");
            match want {
                None => assert!(found.is_none(), "headroom {headroom}"),
                Some(crit) => assert_eq!(found.expect("expected alert").crit, crit),
            }
        }
    }

    #[test]
    fn critical_alerts_sort_before_warnings() {
        // hard-confirm at the warn threshold, headroom deep into crit.
        let mut app = app_with(Some(stats(1800, 1, 5)));
        app.heads[0].max_headroom = 1000;
        let out = compute(&app, &cfg());
        assert!(out.len() >= 2, "expected both alerts, got {:?}", keys(&out));
        assert!(out[0].crit, "the critical alert should lead");
        assert!(!out[out.len() - 1].crit);
    }

    /// The display text carries live counters, so transition tracking diffs on
    /// the key instead. Two heads must therefore never share one.
    #[test]
    fn alert_keys_are_unique_per_head() {
        let mut app = App::new(vec!["head-0".into(), "head-1".into()]);
        for h in app.heads.iter_mut() {
            h.reachable = Some(false);
        }
        let out = compute(&app, &cfg());
        assert_eq!(keys(&out), vec!["head-0:unreachable", "head-1:unreachable"]);
    }
}

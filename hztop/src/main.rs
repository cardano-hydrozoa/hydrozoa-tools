mod alerts;
mod config;
mod persist;
mod poll;
mod snapshot;
mod state;
mod ui;

use clap::Parser;
use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers};
use state::{ALERT_LOG_CAP, AlertEvent, App, Screen, Shared};
use std::collections::HashSet;
use std::io::IsTerminal;
use std::sync::{Arc, Mutex};
use std::time::Duration;

fn main() -> anyhow::Result<()> {
    let args = config::Args::parse();
    let cfg = config::load_config(&args.config)?;
    if cfg.heads.is_empty() {
        anyhow::bail!("no heads configured (config key: heads)");
    }

    let (saved_screen, saved_log) = persist::load();
    let screen = args
        .screen
        .as_deref()
        .and_then(|s| s.chars().next())
        .and_then(Screen::from_key)
        .or(saved_screen)
        .unwrap_or_default();

    let app: Shared = Arc::new(Mutex::new(App::new(
        cfg.heads.iter().map(|h| h.name.clone()).collect(),
    )));
    {
        let mut a = app.lock().unwrap();
        a.screen = screen;
        a.alert_log = saved_log;
    }

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .worker_threads(2)
        .build()?;
    poll::spawn_all(&rt, &cfg, &app);

    // One-shot machine-readable mode. Explicit with --json, and the default
    // whenever stdout is not a terminal: a TUI cannot draw into a pipe, and
    // silently producing escape codes is worse than producing data.
    if args.json || !std::io::stdout().is_terminal() {
        return print_json(&cfg, &app, Duration::from_millis(args.json_timeout_ms));
    }

    let mut terminal = ratatui::init();
    let result = run(&mut terminal, &cfg, &app);
    ratatui::restore();
    {
        let a = app.lock().unwrap();
        persist::save(a.screen, &a.alert_log);
    }
    result
}

/// Print one JSON snapshot and exit.
///
/// The pollers each do their first pass immediately but finish at different
/// times, so wait for the one a diagnosis usually turns on -- every head
/// probed -- before printing. `timeout` bounds that: a head that never answers
/// delays the snapshot, it does not withhold it, and its own `reachable` field
/// then says so.
fn print_json(cfg: &config::Config, app: &Shared, timeout: Duration) -> anyhow::Result<()> {
    let started = std::time::Instant::now();
    let deadline = started + timeout;
    let mut complete = false;
    loop {
        {
            let a = app.lock().unwrap();
            if a.heads.iter().all(|h| h.reachable.is_some()) {
                complete = true;
                break;
            }
        }
        if std::time::Instant::now() >= deadline {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    // Alerts are derived, not polled: without this the snapshot would report
    // an empty alert list no matter what is wrong. A one-shot run does not
    // persist them -- it is a read, and an alert log is the terminal's.
    {
        let mut a = app.lock().unwrap();
        a.alerts = alerts::compute(&a, cfg);
    }
    let a = app.lock().unwrap();
    let mut body = snapshot::build(&a);
    // Say so when a head never answered, rather than letting a consumer read
    // a half-initialized snapshot as fact.
    body["complete"] = serde_json::json!(complete);
    body["waited_ms"] = serde_json::json!(started.elapsed().as_millis() as u64);
    println!("{}", serde_json::to_string_pretty(&body)?);
    Ok(())
}

/// Recompute alerts and append raised/cleared transitions to the log. Runs
/// once per UI tick; the draw only reads the stored results.
fn refresh_alerts(app: &Shared, cfg: &config::Config) {
    let mut a = app.lock().unwrap();
    let fresh = alerts::compute(&a, cfg);
    // Diff on the stable key, never the text: display text may embed live
    // counters and must not flap the alert.
    let old: HashSet<&str> = a.alerts.iter().map(|al| al.key.as_str()).collect();
    let new: HashSet<&str> = fresh.iter().map(|al| al.key.as_str()).collect();
    let now = chrono::Utc::now().timestamp();
    let mut events: Vec<AlertEvent> = Vec::new();
    for al in &fresh {
        if !old.contains(al.key.as_str()) {
            events.push(AlertEvent {
                at_unix: now,
                key: al.key.clone(),
                text: al.text.clone(),
                crit: al.crit,
                cleared: false,
            });
        }
    }
    for al in &a.alerts {
        if !new.contains(al.key.as_str()) {
            events.push(AlertEvent {
                at_unix: now,
                key: al.key.clone(),
                text: al.text.clone(),
                crit: al.crit,
                cleared: true,
            });
        }
    }
    let changed = !events.is_empty();
    for e in events {
        a.alert_log.push_back(e);
    }
    while a.alert_log.len() > ALERT_LOG_CAP {
        a.alert_log.pop_front();
    }
    a.alerts = fresh;
    if changed {
        persist::save(a.screen, &a.alert_log);
    }
}

fn run(
    terminal: &mut ratatui::DefaultTerminal,
    cfg: &config::Config,
    app: &Shared,
) -> anyhow::Result<()> {
    loop {
        refresh_alerts(app, cfg);
        terminal.draw(|f| {
            let a = app.lock().unwrap();
            ui::draw(f, &a, cfg);
        })?;
        if crossterm::event::poll(Duration::from_millis(250))?
            && let Event::Key(k) = crossterm::event::read()?
        {
            if k.kind != KeyEventKind::Press {
                continue;
            }
            match k.code {
                KeyCode::Char('q') | KeyCode::Esc => return Ok(()),
                KeyCode::Char('c') if k.modifiers.contains(KeyModifiers::CONTROL) => {
                    return Ok(());
                }
                KeyCode::Tab => {
                    let mut a = app.lock().unwrap();
                    a.selected = (a.selected + 1) % a.heads.len();
                }
                KeyCode::BackTab => {
                    let mut a = app.lock().unwrap();
                    a.selected = (a.selected + a.heads.len() - 1) % a.heads.len();
                }
                KeyCode::Right => {
                    let mut a = app.lock().unwrap();
                    a.screen = a.screen.next();
                }
                KeyCode::Left => {
                    let mut a = app.lock().unwrap();
                    a.screen = a.screen.prev();
                }
                KeyCode::Char(ch) => {
                    let by_number = match ch {
                        '1' => Some(Screen::Consensus),
                        '2' => Some(Screen::Alerts),
                        _ => None,
                    };
                    // `c` is both a screen key and, with control held, quit --
                    // handled above, so a bare `c` reaching here is the screen.
                    let target = Screen::from_key(ch).or(by_number);
                    if let Some(s) = target {
                        app.lock().unwrap().screen = s;
                    }
                }
                _ => {}
            }
        }
    }
}

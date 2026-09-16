//! Rendering. One draw per tick from the shared state; btop-style panels
//! with rounded borders, sparklines, gauges, and a big headline rate figure.

use crate::alerts::Alert;
use crate::config::Config;
use crate::state::{App, HeadState, RateStats, Screen};
use chrono::Utc;
use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Gauge, Paragraph, Row, Sparkline, Table};
use tui_big_text::{BigText, PixelSize};

const CYAN: Color = Color::Rgb(0, 215, 255);
const PINK: Color = Color::Rgb(255, 95, 215);
const GREEN: Color = Color::Rgb(95, 255, 135);
const YELLOW: Color = Color::Rgb(255, 215, 95);
const RED: Color = Color::Rgb(255, 95, 95);
const DIM: Color = Color::Rgb(130, 130, 150);
const BORDER: Color = Color::Rgb(96, 88, 160);
const TEXT: Color = Color::Rgb(220, 220, 230);
/// Painted across the whole frame (unless transparent_background) so the app
/// looks identical in every terminal theme — some stacks render
/// default-background cells inconsistently behind styled text.
const BG: Color = Color::Rgb(13, 16, 23);

fn panel(title: &str, accent: Color) -> Block<'_> {
    Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(BORDER))
        .title(Line::from(vec![
            Span::styled("▎", Style::default().fg(accent)),
            Span::styled(
                title.to_string(),
                Style::default().fg(accent).add_modifier(Modifier::BOLD),
            ),
            Span::styled("▕", Style::default().fg(accent)),
        ]))
}

fn label(s: &str) -> Span<'static> {
    Span::styled(format!("{s:<11}"), Style::default().fg(DIM))
}

fn val(s: String, c: Color) -> Span<'static> {
    Span::styled(s, Style::default().fg(c).add_modifier(Modifier::BOLD))
}

fn plain(s: String) -> Span<'static> {
    Span::styled(s, Style::default().fg(TEXT))
}

fn dim(s: String) -> Span<'static> {
    Span::styled(s, Style::default().fg(DIM))
}

pub fn fmt_num(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

fn fmt_secs(s: i64) -> String {
    if s < 0 {
        return "now".into();
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

fn rate_line(r: &RateStats) -> String {
    format!(
        "{:>6.1}  1m {:>6.1} · 5m {:>6.1} · 15m {:>6.1}",
        r.now, r.load1m, r.load5m, r.load15m
    )
}

fn age_color(age: i64, warn: i64, crit: i64) -> Color {
    if age >= crit {
        RED
    } else if age >= warn {
        YELLOW
    } else {
        GREEN
    }
}

pub fn draw(f: &mut Frame, app: &App, cfg: &Config) {
    if !cfg.transparent_background {
        f.render_widget(Block::default().style(Style::default().bg(BG)), f.area());
    }
    // Persistent chrome: title+tabs, alerts, then the selected head's own
    // summary. The themed screen below gets everything that's left.
    let alert_h = (app.alerts.len().clamp(1, 4) as u16) + 2;
    let head_h = 9;
    let rows = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(alert_h),
        Constraint::Length(head_h),
        Constraint::Fill(1),
    ])
    .split(f.area());

    draw_title(f, rows[0], app.screen);
    draw_alerts(f, rows[1], app);

    let head = &app.heads[app.selected.min(app.heads.len() - 1)];
    let row_a =
        Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)]).split(rows[2]);
    draw_head(f, row_a[0], head, cfg);
    draw_confirmation(f, row_a[1], head);

    match app.screen {
        Screen::Consensus => draw_screen_consensus(f, rows[3], app, head),
        Screen::Alerts => draw_alert_log(f, rows[3], app),
    }
}

fn draw_screen_consensus(f: &mut Frame, area: Rect, app: &App, head: &HeadState) {
    let heads_h = app.heads.len() as u16 + 3;
    let rows = Layout::vertical([
        Constraint::Length(heads_h),
        Constraint::Length(7),
        Constraint::Length(11),
        Constraint::Fill(1),
    ])
    .split(area);
    draw_heads_strip(f, rows[0], app);
    draw_throughput(f, rows[1], head);
    let mid =
        Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)]).split(rows[2]);
    draw_consensus(f, mid[0], head);
    draw_peers(f, mid[1], head);
    draw_slowest(f, rows[3], head);
}

/// Every head side by side — the place where tip divergence between peers
/// becomes visible without cycling.
fn draw_heads_strip(f: &mut Frame, area: Rect, app: &App) {
    let tips: Vec<u64> = app
        .heads
        .iter()
        .filter_map(|h| h.latest_block.as_ref().map(|b| b.number))
        .collect();
    let skew = match (tips.iter().max(), tips.iter().min()) {
        (Some(max), Some(min)) if tips.len() > 1 => Some(max - min),
        _ => None,
    };
    let mut rows: Vec<Row> = Vec::new();
    for (i, h) in app.heads.iter().enumerate() {
        let sel = if i == app.selected { "▶" } else { " " };
        let tip = h
            .latest_block
            .as_ref()
            .map(|b| format!("#{}", fmt_num(b.number)))
            .unwrap_or_else(|| "—".into());
        let hard = h
            .hard_tip
            .map(|n| format!("#{}", fmt_num(n)))
            .unwrap_or_else(|| "—".into());
        let rate = h
            .stats
            .as_ref()
            .map(|s| format!("{:.1}", s.local_requests.rate.now))
            .unwrap_or_else(|| "—".into());
        let ready = h.ready.clone().unwrap_or_else(|| "?".into());
        let ready_color = match (h.reachable, ready.as_str()) {
            (Some(true), "active") => GREEN,
            (Some(false), _) => RED,
            _ => YELLOW,
        };
        rows.push(Row::new(vec![
            Span::styled(sel.to_string(), Style::default().fg(PINK)),
            Span::styled(h.name.clone(), Style::default().fg(CYAN)),
            plain(tip),
            plain(hard),
            plain(rate),
            Span::styled(ready, Style::default().fg(ready_color)),
        ]));
    }
    let skew_color = match skew {
        Some(s) if s > 100 => RED,
        Some(s) if s > 10 => YELLOW,
        _ => GREEN,
    };
    let title = match skew {
        Some(s) => format!("HEADS · skew {} blk", fmt_num(s)),
        None => "HEADS".to_string(),
    };
    let table = Table::new(
        rows,
        [
            Constraint::Length(2),
            Constraint::Length(10),
            Constraint::Length(14),
            Constraint::Length(14),
            Constraint::Length(8),
            Constraint::Fill(1),
        ],
    )
    .header(
        Row::new(vec!["", "head", "tip", "hard", "req/s", "status"])
            .style(Style::default().fg(DIM).add_modifier(Modifier::BOLD)),
    )
    .block(panel(&title, skew_color));
    f.render_widget(table, area);
}

fn draw_title(f: &mut Frame, area: Rect, screen: Screen) {
    let mut left = vec![
        Span::styled(
            " HZTOP ",
            Style::default()
                .fg(Color::Black)
                .bg(PINK)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("  "),
    ];
    for s in Screen::ALL {
        if s == screen {
            left.push(Span::styled(
                format!(" {} {} ", s.key(), s.title()),
                Style::default()
                    .fg(Color::Black)
                    .bg(CYAN)
                    .add_modifier(Modifier::BOLD),
            ));
        } else {
            left.push(dim(format!(" {} {} ", s.key(), s.title())));
        }
    }
    let right = Line::from(vec![
        dim("q quit · tab head · ←→ screen · ".into()),
        Span::styled(
            Utc::now().format("%H:%M:%S UTC").to_string(),
            Style::default().fg(CYAN),
        ),
        Span::raw(" "),
    ]);
    f.render_widget(Paragraph::new(Line::from(left)), area);
    f.render_widget(Paragraph::new(right).alignment(Alignment::Right), area);
}

/// How long an active alert has been raised, from its newest raise event.
fn active_for(app: &App, key: &str) -> Option<i64> {
    let raise = app
        .alert_log
        .iter()
        .rev()
        .find(|e| !e.cleared && e.ident() == key)?;
    Some((Utc::now().timestamp() - raise.at_unix).max(0))
}

fn draw_alerts(f: &mut Frame, area: Rect, app: &App) {
    let alerts: &[Alert] = &app.alerts;
    let (accent, lines) = if alerts.is_empty() {
        (
            GREEN,
            vec![Line::from(vec![Span::styled(
                "✓ all clear",
                Style::default().fg(GREEN).add_modifier(Modifier::BOLD),
            )])],
        )
    } else {
        let worst = if alerts.iter().any(|a| a.crit) {
            RED
        } else {
            YELLOW
        };
        let lines = alerts
            .iter()
            .take(4)
            .map(|a| {
                let (mark, color) = if a.crit {
                    ("✗ ", RED)
                } else {
                    ("! ", YELLOW)
                };
                let age = active_for(app, &a.key)
                    .filter(|s| *s >= 60)
                    .map(|s| format!("  · for {}", fmt_secs(s)))
                    .unwrap_or_default();
                Line::from(vec![
                    Span::styled(
                        mark,
                        Style::default().fg(color).add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(a.text.clone(), Style::default().fg(color)),
                    dim(age),
                ])
            })
            .collect();
        (worst, lines)
    };
    let title = if alerts.is_empty() {
        "ALERTS".to_string()
    } else {
        format!("ALERTS ({})", alerts.len())
    };
    f.render_widget(Paragraph::new(lines).block(panel(&title, accent)), area);
}

fn draw_head(f: &mut Frame, area: Rect, h: &HeadState, cfg: &Config) {
    let mut lines: Vec<Line> = Vec::new();
    match &h.latest_block {
        Some(b) => {
            let status_color = match b.status.as_str() {
                "HARD_CONFIRMED" => GREEN,
                "SOFT_CONFIRMED" => CYAN,
                _ => YELLOW,
            };
            lines.push(Line::from(vec![
                label("block"),
                val(format!("#{}", fmt_num(b.number)), TEXT),
                dim(format!(" ({})", b.block_type)),
            ]));
            lines.push(Line::from(vec![
                label("version"),
                val(format!("{}.{}", b.version_major, b.version_minor), PINK),
            ]));
            lines.push(Line::from(vec![
                label("status"),
                Span::styled(
                    b.status.clone(),
                    Style::default()
                        .fg(status_color)
                        .add_modifier(Modifier::BOLD),
                ),
            ]));
            let now = Utc::now();
            if let Some(fb) = b.fallback_at {
                let left = (fb - now).num_seconds();
                let color = if left <= cfg.fallback_crit_secs {
                    RED
                } else if left <= cfg.fallback_warn_secs {
                    YELLOW
                } else {
                    GREEN
                };
                lines.push(Line::from(vec![
                    label("fallback"),
                    val(format!("in {}", fmt_secs(left)), color),
                ]));
            }
            if let Some(fm) = b.forced_major_at {
                let left = (fm - now).num_seconds();
                lines.push(Line::from(vec![
                    label("forced maj"),
                    plain(format!("in {}", fmt_secs(left))),
                ]));
            }
        }
        None => lines.push(Line::from(dim("waiting for block details…".into()))),
    }
    if let Some(s) = &h.stats {
        lines.push(Line::from(vec![
            label("uptime"),
            plain(fmt_secs(s.uptime_seconds as i64)),
        ]));
        lines.push(Line::from(vec![
            label("requests"),
            plain(format!(
                "{:.1}/s now · {:.1}/s 1m",
                s.local_requests.rate.now, s.local_requests.rate.load1m
            )),
        ]));
        lines.push(Line::from(vec![
            label("mempool"),
            plain(format!(
                "{} · headroom {}",
                fmt_num(s.mempool_size.max(0) as u64),
                fmt_num(s.sequencer_headroom.max(0) as u64)
            )),
        ]));
    }
    let title = format!("HEAD · {}", h.name);
    f.render_widget(Paragraph::new(lines).block(panel(&title, PINK)), area);
}

fn draw_confirmation(f: &mut Frame, area: Rect, h: &HeadState) {
    let mut lines: Vec<Line> = Vec::new();
    match &h.latest_block {
        Some(b) => lines.push(Line::from(vec![
            label("tip blk"),
            val(format!("#{}", fmt_num(b.number)), CYAN),
            dim(format!("  {}", b.status)),
        ])),
        None => lines.push(Line::from(vec![label("tip blk"), dim("—".into())])),
    }
    match h.hard_tip {
        Some(hard) => {
            let behind = h
                .latest_block
                .as_ref()
                .map(|b| format!("  ({} behind tip)", fmt_num(b.number.saturating_sub(hard))))
                .unwrap_or_default();
            lines.push(Line::from(vec![
                label("hard blk"),
                val(format!("#{}", fmt_num(hard)), GREEN),
                dim(behind),
            ]));
        }
        None => lines.push(Line::from(vec![
            label("hard blk"),
            dim("none hard-confirmed yet".into()),
        ])),
    }
    if let Some(s) = &h.stats {
        let age = s.stacks.seconds_since_last_hard_confirm;
        lines.push(Line::from(vec![
            label("last hard"),
            val(format!("{} ago", fmt_secs(age)), age_color(age, 600, 1800)),
            dim(if s.stacks.last_stack_number < 0 {
                "  no stacks yet".to_string()
            } else {
                format!("  stack #{}", s.stacks.last_stack_number)
            }),
        ]));
        lines.push(Line::from(vec![
            label("cadence"),
            plain(format!(
                "μ {:.0}s · {:.0} blk/stack (max {})",
                s.stacks.mean_inter_stack_gap_seconds,
                s.stacks.avg_blocks_absorbed,
                s.stacks.max_blocks_absorbed
            )),
        ]));
        lines.push(Line::from(vec![
            label("soft lat"),
            plain(format!(
                "{:.0}ms avg over {} blocks",
                s.block_timings.soft_consensus.avg_millis, s.block_timings.soft_consensus.count
            )),
        ]));
    }
    f.render_widget(
        Paragraph::new(lines).block(panel("CONFIRMATION", GREEN)),
        area,
    );
}

fn draw_throughput(f: &mut Frame, area: Rect, h: &HeadState) {
    let block = panel("THROUGHPUT", YELLOW);
    let inner = block.inner(area);
    f.render_widget(block, area);

    let cols = Layout::horizontal([Constraint::Length(26), Constraint::Fill(1)]).split(inner);

    // Headline figure: the head's own accepted-request rate.
    let big_val = h
        .stats
        .as_ref()
        .map(|s| s.local_requests.rate.now.round() as u64)
        .unwrap_or(0);
    let big_area = Layout::vertical([Constraint::Length(4), Constraint::Length(1)]).split(cols[0]);
    let big = BigText::builder()
        .pixel_size(PixelSize::Quadrant)
        .style(Style::default().fg(YELLOW).add_modifier(Modifier::BOLD))
        .lines(vec![Line::from(fmt_num(big_val))])
        .build();
    f.render_widget(big, big_area[0]);
    f.render_widget(
        Paragraph::new(Line::from(dim("req/s (head)".to_string()))).alignment(Alignment::Center),
        big_area[1],
    );

    // Sparkline rows: label+numbers on the left, live graph on the right.
    let rows = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .split(cols[1]);
    // Slice the history to the sparkline's actual width: Sparkline renders
    // the first N samples of its slice, so an oversized slice would pin the
    // view to the oldest data instead of scrolling.
    let spark = |f: &mut Frame,
                 area: Rect,
                 name: &str,
                 text: String,
                 ring: &crate::state::Ring,
                 color: Color| {
        let split = Layout::horizontal([Constraint::Length(58), Constraint::Fill(1)]).split(area);
        f.render_widget(
            Paragraph::new(Line::from(vec![label(name), plain(text)])),
            split[0],
        );
        f.render_widget(
            Sparkline::default()
                .data(ring.tail(split[1].width as usize))
                .style(Style::default().fg(color)),
            split[1],
        );
    };
    if let Some(s) = &h.stats {
        spark(
            f,
            rows[0],
            "req/s",
            rate_line(&s.local_requests.rate),
            &h.req_hist,
            CYAN,
        );
        spark(
            f,
            rows[1],
            "blocks/s",
            rate_line(&s.blocks.block_rate),
            &h.blk_hist_x10,
            PINK,
        );
        spark(
            f,
            rows[2],
            "mempool",
            format!("{:>7}", fmt_num(s.mempool_size.max(0) as u64)),
            &h.mempool_hist,
            YELLOW,
        );
    }
}

fn draw_consensus(f: &mut Frame, area: Rect, h: &HeadState) {
    let block = panel("CONSENSUS & MEMPOOL", CYAN);
    let inner = block.inner(area);
    f.render_widget(block, area);
    let Some(s) = &h.stats else {
        f.render_widget(Paragraph::new(dim("head unreachable".into())), inner);
        return;
    };
    let rows = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(5),
    ])
    .split(inner);

    let mempool = s.mempool_size.max(0) as u64;
    let capacity = (h.max_headroom.max(s.sequencer_headroom + s.mempool_size)).max(1) as u64;
    let ratio = (mempool as f64 / capacity as f64).clamp(0.0, 1.0);
    let gauge_color = if ratio > 0.9 {
        RED
    } else if ratio > 0.6 {
        YELLOW
    } else {
        GREEN
    };
    let gsplit = Layout::horizontal([Constraint::Length(11), Constraint::Fill(1)]).split(rows[0]);
    f.render_widget(Paragraph::new(Line::from(label("mempool"))), gsplit[0]);
    f.render_widget(
        Gauge::default()
            .ratio(ratio)
            .label(Span::styled(
                format!("{} / {}", fmt_num(mempool), fmt_num(capacity)),
                Style::default().fg(TEXT),
            ))
            .gauge_style(Style::default().fg(gauge_color).bg(Color::Rgb(40, 40, 55))),
        gsplit[1],
    );

    let headroom_color =
        if h.max_headroom > 0 && (s.sequencer_headroom as f64 / h.max_headroom as f64) < 0.1 {
            RED
        } else {
            TEXT
        };
    f.render_widget(
        Paragraph::new(Line::from(vec![
            label("headroom"),
            val(fmt_num(s.sequencer_headroom.max(0) as u64), headroom_color),
            dim(format!("  leader drain {}", s.leader_mempool_drain)),
        ])),
        rows[1],
    );

    let rejects_style = if s.local_requests.rejected_backpressure > 0 {
        YELLOW
    } else {
        TEXT
    };
    let lines = vec![
        Line::from(vec![
            label("rejects"),
            Span::styled(
                format!(
                    "screening {} · backpressure {}",
                    fmt_num(s.local_requests.rejected_screening),
                    fmt_num(s.local_requests.rejected_backpressure)
                ),
                Style::default().fg(rejects_style),
            ),
        ]),
        Line::from(vec![
            label("timings"),
            plain(format!(
                "lead {:.0}ms · replay {:.0}ms · soft {:.0}ms",
                s.block_timings.lead.avg_millis,
                s.block_timings.replay.avg_millis,
                s.block_timings.soft_consensus.avg_millis
            )),
        ]),
        Line::from(vec![
            label("blocks"),
            plain(format!(
                "minor {} · major {} · {:.1} req/blk (max {})",
                fmt_num(s.blocks.minor),
                fmt_num(s.blocks.major),
                s.blocks.avg_events,
                fmt_num(s.blocks.max_events)
            )),
        ]),
        Line::from(vec![
            label("accepted"),
            plain(format!(
                "{} local requests",
                fmt_num(s.local_requests.total)
            )),
        ]),
    ];
    f.render_widget(Paragraph::new(lines), rows[2]);
}

fn draw_alert_log(f: &mut Frame, area: Rect, app: &App) {
    let mut lines: Vec<Line> = Vec::new();
    let log: Vec<_> = app.alert_log.iter().collect();
    for (i, e) in log.iter().enumerate().rev() {
        let t = chrono::DateTime::from_timestamp(e.at_unix, 0)
            .map(|d| d.format("%m-%d %H:%M:%S").to_string())
            .unwrap_or_else(|| "?".into());
        let (mark, color) = match (e.cleared, e.crit) {
            (true, _) => ("✓ cleared ", GREEN),
            (false, true) => ("✗ raised  ", RED),
            (false, false) => ("! raised  ", YELLOW),
        };
        // A cleared event pairs with the nearest earlier raise of the same
        // key; the gap is how long the alert was active.
        let lasted = if e.cleared {
            log[..i]
                .iter()
                .rev()
                .find(|p| !p.cleared && p.ident() == e.ident())
                .map(|p| format!("  · lasted {}", fmt_secs((e.at_unix - p.at_unix).max(0))))
                .unwrap_or_default()
        } else {
            String::new()
        };
        lines.push(Line::from(vec![
            dim(format!("{t}  ")),
            Span::styled(
                mark,
                Style::default().fg(color).add_modifier(Modifier::BOLD),
            ),
            Span::styled(e.text.clone(), Style::default().fg(color)),
            dim(lasted),
        ]));
    }
    if lines.is_empty() {
        lines.push(Line::from(dim("no alert transitions recorded yet".into())));
    }
    let title = format!("ALERT HISTORY ({})", app.alert_log.len());
    f.render_widget(Paragraph::new(lines).block(panel(&title, YELLOW)), area);
}

fn draw_peers(f: &mut Frame, area: Rect, h: &HeadState) {
    let mut rows: Vec<Row> = Vec::new();
    if let Some(s) = &h.stats {
        rows.push(Row::new(vec![
            Span::styled("local", Style::default().fg(CYAN)),
            plain(fmt_num(s.local_requests.total)),
            plain(format!("{:.1}", s.local_requests.rate.now)),
            plain(format!("{:.1}", s.local_requests.rate.load1m)),
            plain(format!("{:.1}", s.local_requests.rate.load5m)),
            Span::styled("● self", Style::default().fg(CYAN)),
        ]));
        for p in &s.peer_requests {
            let live = p.rate.load1m > 0.0;
            let (mark, color) = if live {
                ("● live", GREEN)
            } else {
                ("○ idle", DIM)
            };
            rows.push(Row::new(vec![
                plain(format!("peer {}", p.peer)),
                plain(fmt_num(p.total)),
                plain(format!("{:.1}", p.rate.now)),
                plain(format!("{:.1}", p.rate.load1m)),
                plain(format!("{:.1}", p.rate.load5m)),
                Span::styled(mark, Style::default().fg(color)),
            ]));
        }
    }
    let table = Table::new(
        rows,
        [
            Constraint::Length(8),
            Constraint::Length(12),
            Constraint::Length(8),
            Constraint::Length(8),
            Constraint::Length(8),
            Constraint::Fill(1),
        ],
    )
    .header(
        Row::new(vec!["src", "total", "now", "1m", "5m", "link"])
            .style(Style::default().fg(DIM).add_modifier(Modifier::BOLD)),
    )
    .block(panel("REQUEST SOURCES", CYAN));
    f.render_widget(table, area);
}

fn draw_slowest(f: &mut Frame, area: Rect, h: &HeadState) {
    let mut rows: Vec<Row> = Vec::new();
    if let Some(s) = &h.stats {
        for t in s.block_timings.lead.top.iter().take(8) {
            let color = if t.millis >= 1000 {
                RED
            } else if t.millis >= 200 {
                YELLOW
            } else {
                GREEN
            };
            rows.push(Row::new(vec![
                plain(format!("#{}", fmt_num(t.block_number))),
                Span::styled(
                    format!("{}ms", fmt_num(t.millis)),
                    Style::default().fg(color),
                ),
                plain(format!("{} reqs", fmt_num(t.requests))),
            ]));
        }
    }
    let table = Table::new(
        rows,
        [
            Constraint::Length(14),
            Constraint::Length(12),
            Constraint::Fill(1),
        ],
    )
    .header(
        Row::new(vec!["block", "lead time", "requests"])
            .style(Style::default().fg(DIM).add_modifier(Modifier::BOLD)),
    )
    .block(panel("SLOWEST BLOCKS", PINK));
    f.render_widget(table, area);
}

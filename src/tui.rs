use crate::{
    config::Config,
    health::Health,
    state::{self, HostState, State},
    storage::{self, Bucket},
};
use anyhow::{Result, ensure};
use crossterm::{
    cursor::Show,
    event::{self, Event, KeyCode, KeyEventKind, KeyModifiers},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{
    Frame, Terminal,
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    symbols,
    text::Line,
    widgets::{
        Axis, Block, Borders, Cell, Chart, Dataset, GraphType, Paragraph, Row, Table, TableState,
        Wrap,
    },
};
use std::{
    io::{self, IsTerminal},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Range {
    #[default]
    Hour,
    Day,
    Week,
}
impl Range {
    pub fn seconds(self) -> i64 {
        match self {
            Self::Hour => 3600,
            Self::Day => 86400,
            Self::Week => 604800,
        }
    }
    fn label(self) -> &'static str {
        match self {
            Self::Hour => "1h",
            Self::Day => "24h",
            Self::Week => "7d",
        }
    }
}
pub fn stale(h: &HostState, c: &Config, now: i64) -> bool {
    h.failures > 0
        || h.last_success.is_none_or(|t| {
            now.saturating_sub(t)
                > (c.poll_interval_seconds
                    .saturating_mul(2)
                    .saturating_add(c.ssh_timeout_seconds))
                .min(i64::MAX as u64) as i64
        })
}
fn health_color(h: Health) -> Color {
    match h {
        Health::Ok => Color::Green,
        Health::Warning => Color::Yellow,
        Health::Critical => Color::Red,
        Health::Unknown => Color::Gray,
    }
}
fn value(v: Option<f64>, suffix: &str) -> String {
    v.map_or_else(|| "-".into(), |v| format!("{v:.1}{suffix}"))
}
fn optional<T: std::fmt::Display>(v: Option<T>) -> String {
    v.map_or_else(|| "-".into(), |v| v.to_string())
}
fn uptime(seconds: Option<f64>) -> String {
    seconds.map_or_else(
        || "-".into(),
        |v| {
            let hours = v as u64 / 3600;
            format!("{}d {}h", hours / 24, hours % 24)
        },
    )
}
fn rate(v: Option<f64>) -> String {
    match v {
        None => "-".into(),
        Some(v) if v >= 1048576. => format!("{:.1} MiB/s", v / 1048576.),
        Some(v) if v >= 1024. => format!("{:.1} KiB/s", v / 1024.),
        Some(v) => format!("{v:.1} B/s"),
    }
}

#[derive(Default)]
pub struct App {
    pub state: State,
    pub names: Vec<String>,
    pub selected: usize,
    pub range: Range,
    pub buckets: Vec<Bucket>,
    pub state_error: Option<String>,
    pub history_error: Option<String>,
    focus: usize,
    graph_page: usize,
    details_scroll: u16,
    history_end: i64,
    max_sample_gap: i64,
    table: TableState,
}
impl App {
    fn refresh(&mut self, c: &Config, width: usize) {
        match state::read(&c.data_dir) {
            Ok(s) => {
                self.state = s;
                self.state_error = None;
            }
            Err(e) => self.state_error = Some(format!("state unavailable: {e}")),
        }
        let old_name = self.names.get(self.selected).cloned();
        self.names = c
            .hosts
            .iter()
            .filter(|h| h.enabled)
            .map(|h| h.name.clone())
            .collect();
        for name in &self.names {
            self.state.hosts.entry(name.clone()).or_default();
        }
        self.selected = old_name
            .and_then(|n| self.names.iter().position(|v| *v == n))
            .unwrap_or(self.selected)
            .min(self.names.len().saturating_sub(1));
        self.refresh_history(c, width);
    }
    fn refresh_history(&mut self, c: &Config, width: usize) {
        self.max_sample_gap = c
            .poll_interval_seconds
            .saturating_mul(2)
            .saturating_add(c.ssh_timeout_seconds)
            .min(i64::MAX as u64) as i64;
        self.buckets.clear();
        self.history_error = None;
        if let Some(name) = self.names.get(self.selected) {
            let now = crate::now();
            self.history_end = now;
            match storage::history(&c.data_dir, name, now - self.range.seconds(), now, width) {
                Ok(b) => self.buckets = b,
                Err(e) => self.history_error = Some(format!("history unavailable: {e}")),
            }
        }
    }
    fn selected_host(&self) -> Option<(&str, &HostState)> {
        let name = self.names.get(self.selected)?;
        Some((name, self.state.hosts.get(name)?))
    }
    fn is_stale(&self, h: &HostState, c: &Config, now: i64) -> bool {
        self.state_error.is_some() || stale(h, c, now)
    }
}

struct Screen;
impl Drop for Screen {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen, Show);
    }
}
struct Signals(Vec<signal_hook::SigId>);
impl Drop for Signals {
    fn drop(&mut self) {
        for id in self.0.drain(..) {
            signal_hook::low_level::unregister(id);
        }
    }
}
pub fn run(c: &Config) -> Result<()> {
    ensure!(
        io::stdout().is_terminal() && io::stdin().is_terminal(),
        "tui requires an interactive terminal"
    );
    let quit = Arc::new(AtomicBool::new(false));
    let mut signals = Signals(Vec::new());
    for signal in [signal_hook::consts::SIGTERM, signal_hook::consts::SIGINT] {
        signals
            .0
            .push(signal_hook::flag::register(signal, quit.clone())?);
    }
    enable_raw_mode()?;
    let _screen = Screen;
    execute!(io::stdout(), EnterAlternateScreen)?;
    let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;
    terminal.clear()?;
    let mut app = App::default();
    let mut last = Instant::now();
    let mut width = terminal.size()?.width as usize / 2;
    app.refresh(c, width);
    while !quit.load(Ordering::Relaxed) {
        let new_width = terminal.size()?.width as usize / 2;
        if last.elapsed() >= Duration::from_secs(1) || width != new_width {
            width = new_width;
            app.refresh(c, width);
            last = Instant::now();
        }
        terminal.draw(|f| render(f, &mut app, c, crate::now()))?;
        if event::poll(Duration::from_millis(100))?
            && let Event::Key(key) = event::read()?
            && key.kind != KeyEventKind::Release
        {
            let mut reload = false;
            match key.code {
                KeyCode::Char('q') | KeyCode::Esc => break,
                KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => break,
                KeyCode::Up => {
                    app.selected = app.selected.saturating_sub(1);
                    app.details_scroll = 0;
                    reload = true;
                }
                KeyCode::Down => {
                    app.selected = (app.selected + 1).min(app.names.len().saturating_sub(1));
                    app.details_scroll = 0;
                    reload = true;
                }
                KeyCode::Enter => app.focus = 1,
                KeyCode::Tab => app.focus = (app.focus + 1) % 3,
                KeyCode::Left => {
                    app.graph_page = (app.graph_page + 6) % 7;
                    app.focus = 2;
                }
                KeyCode::Right => {
                    app.graph_page = (app.graph_page + 1) % 7;
                    app.focus = 2;
                }
                KeyCode::PageDown => {
                    app.details_scroll = app.details_scroll.saturating_add(5);
                    app.focus = 1;
                }
                KeyCode::PageUp => {
                    app.details_scroll = app.details_scroll.saturating_sub(5);
                    app.focus = 1;
                }
                KeyCode::Char('1') => {
                    app.range = Range::Hour;
                    reload = true;
                }
                KeyCode::Char('2') => {
                    app.range = Range::Day;
                    reload = true;
                }
                KeyCode::Char('3') => {
                    app.range = Range::Week;
                    reload = true;
                }
                KeyCode::Char('r') => {
                    app.refresh(c, width);
                    last = Instant::now();
                }
                _ => {}
            }
            if reload {
                app.refresh_history(c, width);
            }
        }
    }
    Ok(())
}
fn block(title: impl Into<Line<'static>>, focus: bool) -> Block<'static> {
    Block::default()
        .borders(Borders::ALL)
        .title(title)
        .border_style(Style::default().fg(if focus { Color::Cyan } else { Color::DarkGray }))
}
pub fn render(f: &mut Frame, app: &mut App, c: &Config, now: i64) {
    let area = f.area();
    if area.width < 55 || area.height < 12 {
        f.render_widget(
            Paragraph::new("Terminal too small (minimum 55 x 12). q: quit"),
            area,
        );
        return;
    }
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(
                (app.names.len() as u16 + 3)
                    .clamp(4, 10)
                    .min(area.height / 3),
            ),
            Constraint::Min(1),
            Constraint::Length(2),
        ])
        .split(area);
    let data = app
        .names
        .iter()
        .map(|name| {
            let h = &app.state.hosts[name];
            let old = app.is_stale(h, c, now);
            let m = h.metrics.as_ref();
            let health = if old && h.health != Health::Critical {
                Health::Unknown
            } else {
                h.health
            };
            Row::new(vec![
                Cell::from(name.clone()),
                Cell::from(format!("{health}{}", if old { "/STALE" } else { "" })).style(
                    Style::default().fg(if old {
                        Color::Yellow
                    } else {
                        health_color(health)
                    }),
                ),
                Cell::from(value(m.and_then(|m| m.cpu_percent), "%")),
                Cell::from(value(m.and_then(|m| m.memory_percent), "%")),
                Cell::from(value(m.and_then(|m| m.max_disk_used_percent()), "%")),
                Cell::from(value(m.and_then(|m| m.load1), "")),
                Cell::from(
                    h.latency_ms
                        .map_or_else(|| "-".into(), |v| format!("{v}ms")),
                ),
            ])
        })
        .collect::<Vec<_>>();
    let widths = [
        Constraint::Min(9),
        Constraint::Length(16),
        Constraint::Length(7),
        Constraint::Length(7),
        Constraint::Length(7),
        Constraint::Length(7),
        Constraint::Length(9),
    ];
    let table = Table::new(data, widths)
        .header(
            Row::new(["HOST", "STATE", "CPU", "RAM", "DISK", "LOAD", "SSH"])
                .style(Style::default().add_modifier(Modifier::BOLD)),
        )
        .block(block("HOST-WATCH", app.focus == 0))
        .row_highlight_style(
            Style::default()
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("▶ ");
    app.table.select(if app.names.is_empty() {
        None
    } else {
        Some(app.selected)
    });
    f.render_stateful_widget(table, rows[0], &mut app.table);
    if area.width >= 110 && area.height >= 24 {
        let panels = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(35), Constraint::Percentage(65)])
            .split(rows[1]);
        details(f, app, c, now, panels[0]);
        graphs(f, app, c, panels[1]);
    } else if app.focus == 1 {
        details(f, app, c, now, rows[1]);
    } else {
        graphs(f, app, c, rows[1]);
    }
    let footer = if let Some(err) = &app.state_error {
        format!("{err} | q: quit")
    } else {
        format!(
            "Range: {} | ↑↓ host  Enter details  Tab panel  ←→ graphs  1/2/3 range\nr refresh  PgUp/PgDn details  q quit | lines: samples, gaps: no data",
            app.range.label()
        )
    };
    f.render_widget(Paragraph::new(footer).wrap(Wrap { trim: true }), rows[2]);
}
fn details(f: &mut Frame, app: &App, c: &Config, now: i64, area: Rect) {
    let Some((name, h)) = app.selected_host() else {
        f.render_widget(
            Paragraph::new("No enabled hosts").block(block("DETAILS", app.focus == 1)),
            area,
        );
        return;
    };
    let old = app.is_stale(h, c, now);
    let mut text = vec![
        format!("Host: {name}"),
        format!(
            "State: {}{}",
            h.health,
            if old {
                " / STALE (last known values)"
            } else {
                ""
            }
        ),
        format!("Last success: {}", optional(h.last_success)),
        format!("SSH round trip: {} ms", optional(h.latency_ms)),
    ];
    if let Some(m) = &h.metrics {
        text.extend([
            format!("Hostname: {}", m.hostname.as_deref().unwrap_or("-")),
            format!("Kernel: {}", m.kernel.as_deref().unwrap_or("-")),
            format!("Uptime: {}", uptime(m.uptime_seconds)),
            format!("CPU cores: {}", optional(m.cpu_count)),
            format!("Processes: {}", optional(m.processes)),
            format!("Failed units: {}", optional(m.failed_units)),
            format!(
                "CPU: {} RAM: {}",
                value(m.cpu_percent, "%"),
                value(m.memory_percent, "%")
            ),
            format!(
                "Load: {} / {} / {}",
                value(m.load1, ""),
                value(m.load5, ""),
                value(m.load15, "")
            ),
            format!(
                "RX: {}  TX: {}",
                rate(m.rx_bytes_per_sec),
                rate(m.tx_bytes_per_sec)
            ),
            format!("CPU temperature: {}", value(m.cpu_temperature_c, " C")),
            "FILESYSTEMS".into(),
        ]);
        for disk in &m.disks {
            text.push(format!(
                "{}  {:.1}% ({} KiB available)",
                disk.mount, disk.used_percent, disk.available_kb
            ));
        }
    } else {
        text.push("Metrics: -".into());
    }
    text.extend(h.reasons.iter().cloned());
    // Focused details can occupy the whole lower panel on narrow terminals.
    f.render_widget(
        Paragraph::new(text.join("\n"))
            .scroll((
                app.details_scroll.min(text.len().saturating_sub(1) as u16),
                0,
            ))
            .wrap(Wrap { trim: true })
            .block(block("DETAILS", app.focus == 1)),
        area,
    );
}
const TITLES: [&str; 7] = [
    "CPU %",
    "RAM %",
    "Load1",
    "SSH round trip ms",
    "Disk (most used) %",
    "RX (incoming)",
    "TX (outgoing)",
];
fn graphs(f: &mut Frame, app: &App, c: &Config, area: Rect) {
    if let Some(e) = &app.history_error {
        f.render_widget(
            Paragraph::new(e.as_str())
                .wrap(Wrap { trim: true })
                .block(block("HISTORY", app.focus == 2)),
            area,
        );
        return;
    }
    let count = if area.height >= 32 {
        7
    } else if area.height >= 16 {
        4
    } else if area.height >= 10 {
        2
    } else {
        1
    };
    let columns = if count > 1 && area.width >= 55 { 2 } else { 1 };
    let row_count = (count as usize).div_ceil(columns);
    let heights = vec![Constraint::Ratio(1, row_count as u32); row_count];
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints(heights)
        .split(area);
    for j in 0..count as usize {
        let cells = Layout::default()
            .direction(Direction::Horizontal)
            .constraints(vec![Constraint::Ratio(1, columns as u32); columns])
            .split(rows[j / columns]);
        let index = (app.graph_page + j) % 7;
        graph(
            f,
            app,
            index,
            cells[j % columns],
            app.focus == 2,
            warning_threshold(c, index),
        );
    }
}
/// Empty drawing buckets are not failed polls. Only explicit missing samples or
/// a gap beyond the polling tolerance should interrupt the line.
fn segments(buckets: &[Bucket], index: usize, start: i64, gap: i64) -> Vec<Vec<(f64, f64)>> {
    let mut result: Vec<Vec<(f64, f64)>> = Vec::new();
    let mut previous = None;
    for b in buckets {
        let (Some(value), Some(first), Some(last)) =
            (b.values[index], b.first_at[index], b.last_at[index])
        else {
            continue;
        };
        if previous.is_none_or(|t| first - t > gap) || b.break_before[index] {
            result.push(Vec::new());
        }
        let x = first as f64 / 2. + last as f64 / 2. - start as f64;
        result.last_mut().expect("segment exists").push((x, value));
        previous = Some(last);
    }
    result
}
fn graph_value(v: f64, index: usize, scale: f64, unit: &str) -> String {
    match index {
        0 | 1 | 4 => format!("{v:.1}%"),
        2 => format!("{v:.2}"),
        _ => format!("{:.1} {unit}", v / scale),
    }
}
fn warning_threshold(c: &Config, index: usize) -> Option<f64> {
    match index {
        0 => Some(c.thresholds.cpu_warning),
        1 => Some(c.thresholds.memory_warning),
        4 => Some(c.thresholds.disk_warning),
        _ => None,
    }
}
fn graph(f: &mut Frame, app: &App, index: usize, area: Rect, focus: bool, warning: Option<f64>) {
    let start = app.history_end - app.range.seconds();
    let mut lines = segments(&app.buckets, index, start, app.max_sample_gap);
    let values = lines.iter().flatten().map(|p| p.1).collect::<Vec<_>>();
    let Some(last) = values.last().copied() else {
        f.render_widget(
            Paragraph::new("No samples yet / unknown").block(block(
                format!("{} ({})", TITLES[index], app.range.label()),
                focus,
            )),
            area,
        );
        return;
    };
    let (min, max) = Bucket::extrema(&app.buckets, index).unwrap_or_else(|| {
        (
            values.iter().copied().reduce(f64::min).unwrap_or(0.),
            values.iter().copied().reduce(f64::max).unwrap_or(0.),
        )
    });
    let (scale, unit) = match index {
        5 | 6 if max >= 1048576. => (1048576., "MiB/s"),
        5 | 6 if max >= 1024. => (1024., "KiB/s"),
        5 | 6 => (1., "B/s"),
        3 => (1., "ms"),
        _ => (1., ""),
    };
    for line in &mut lines {
        for point in line {
            point.1 /= scale;
        }
    }
    let ceiling = if matches!(index, 0 | 1 | 4) {
        100.
    } else {
        (max / scale * 1.15).max(1.)
    };
    let color = match index {
        0 => Color::Green,
        1 => Color::Magenta,
        2 => Color::Yellow,
        3 => Color::Cyan,
        4 => Color::Blue,
        5 => Color::Green,
        _ => Color::Yellow,
    };
    let warning_points = warning.map(|y| {
        // One tiny Braille dot roughly every four terminal columns.
        let steps = (area.width.saturating_sub(10) / 4).max(2) as usize;
        (0..=steps)
            .map(|i| (app.range.seconds() as f64 * i as f64 / steps as f64, y))
            .collect::<Vec<_>>()
    });
    let mut datasets = Vec::new();
    if let Some(points) = &warning_points {
        datasets.push(
            Dataset::default()
                .marker(symbols::Marker::Braille)
                .graph_type(GraphType::Scatter)
                .style(
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::DIM),
                )
                .data(points),
        );
    }
    datasets.extend(
        lines
            .iter()
            .map(|line| {
                Dataset::default()
                    .marker(symbols::Marker::Braille)
                    .graph_type(GraphType::Line)
                    .style(Style::default().fg(color).add_modifier(Modifier::BOLD))
                    .data(line)
            })
            .collect::<Vec<_>>(),
    );
    let mut title = format!(
        "{} | last {}",
        TITLES[index],
        graph_value(last, index, scale, unit)
    );
    if let Some(threshold) = warning {
        title.push_str(&format!(" | warn {threshold:.0}%"));
    }
    let avg = Bucket::average(&app.buckets, index)
        .map_or_else(|| "-".into(), |v| graph_value(v, index, scale, unit));
    let summary = format!(
        "min {} / avg {} / max {}",
        graph_value(min, index, scale, unit),
        avg,
        graph_value(max, index, scale, unit)
    );
    let label = |v: f64| {
        if matches!(index, 0 | 1 | 4) {
            format!("{v:.0}%")
        } else if unit.is_empty() {
            format!("{v:.1}")
        } else {
            format!("{v:.1} {unit}")
        }
    };
    let time_label = |fraction: f64| {
        let seconds = (app.range.seconds() as f64 * (1. - fraction)) as i64;
        if seconds == 0 {
            "now".into()
        } else if seconds >= 86400 {
            format!("-{}d", seconds / 86400)
        } else if seconds >= 3600 && seconds % 3600 == 0 {
            format!("-{}h", seconds / 3600)
        } else {
            format!("-{}m", seconds / 60)
        }
    };
    let xlabels = if area.width >= 45 {
        (0..=4)
            .map(|i| time_label(i as f64 / 4.))
            .collect::<Vec<_>>()
    } else {
        vec![time_label(0.), time_label(1.)]
    };
    let chart = Chart::new(datasets)
        .block(block(title, focus).title_bottom(Line::from(summary)))
        .x_axis(
            Axis::default()
                .bounds([0., app.range.seconds() as f64])
                .labels(xlabels)
                .style(Style::default().fg(Color::Gray)),
        )
        .y_axis(
            Axis::default()
                .bounds([0., ceiling])
                .labels([label(0.), label(ceiling / 2.), label(ceiling)])
                .style(Style::default().fg(Color::Gray)),
        );
    f.render_widget(chart, area);
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;
    fn config() -> Config {
        Config {
            hosts: vec![crate::config::Host {
                name: "fixture".into(),
                ssh_host: None,
                enabled: true,
            }],
            ..Default::default()
        }
    }
    fn point(at: i64, value: f64) -> Bucket {
        let mut b = Bucket::default();
        b.values[0] = Some(value);
        b.first_at[0] = Some(at);
        b.last_at[0] = Some(at);
        b
    }
    #[test]
    fn lines_bridge_empty_buckets_but_not_missing_polls() {
        let mut b = vec![point(60, 10.), Bucket::default(), point(120, 20.)];
        assert_eq!(segments(&b, 0, 0, 130), vec![vec![(60., 10.), (120., 20.)]]);
        b[2].break_before[0] = true;
        assert_eq!(segments(&b, 0, 0, 130).len(), 2);
        b[2] = point(300, 30.);
        assert_eq!(segments(&b, 0, 0, 130).len(), 2);
        assert!(segments(&[], 0, 0, 130).is_empty());
    }
    #[test]
    fn charts_show_values_units_and_time_ticks() {
        let mut app = App {
            history_end: 3600,
            max_sample_gap: 130,
            ..Default::default()
        };
        app.buckets = vec![point(3000, 10.), point(3060, 20.)];
        for b in &mut app.buckets {
            b.values[5] = Some(2097152.);
            b.first_at[5] = b.first_at[0];
            b.last_at[5] = b.last_at[0];
        }
        let mut terminal = Terminal::new(TestBackend::new(100, 20)).unwrap();
        terminal
            .draw(|f| graph(f, &app, 5, f.area(), false, None))
            .unwrap();
        let text = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol())
            .collect::<String>();
        for label in ["RX (incoming)", "last 2.0 MiB/s", "-30m", "min 2.0 MiB/s"] {
            assert!(text.contains(label), "missing {label}");
        }
        terminal
            .draw(|f| graph(f, &app, 0, f.area(), false, Some(90.)))
            .unwrap();
        let text = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol())
            .collect::<String>();
        assert!(text.contains("last 20.0%"));
        assert!(text.contains("50%"));
        assert!(text.contains("warn 90%"));
        let threshold_pixels = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .filter(|c| c.fg == Color::Yellow && c.symbol() != " ")
            .count();
        assert!(
            (10..=30).contains(&threshold_pixels),
            "threshold must be visible but sparse"
        );
        let mut c = config();
        c.thresholds.cpu_warning = 71.;
        c.thresholds.memory_warning = 72.;
        c.thresholds.disk_warning = 73.;
        for (index, expected) in [
            (0, Some(71.)),
            (1, Some(72.)),
            (4, Some(73.)),
            (2, None),
            (3, None),
            (5, None),
            (6, None),
        ] {
            assert_eq!(warning_threshold(&c, index), expected);
        }
    }
    #[test]
    fn stale_detection_and_table() {
        let c = config();
        let mut app = App::default();
        app.names.push("fixture".into());
        let h = HostState {
            health: Health::Ok,
            last_success: Some(1000),
            ..Default::default()
        };
        assert!(!stale(&h, &c, 1050));
        assert!(stale(&h, &c, 1200));
        app.state.hosts.insert("fixture".into(), h);
        let mut term = Terminal::new(TestBackend::new(120, 40)).unwrap();
        term.draw(|f| render(f, &mut app, &c, 1200)).unwrap();
        let buffer = term.backend().buffer();
        let text = buffer
            .content
            .iter()
            .map(|c| c.symbol())
            .collect::<String>();
        assert!(text.contains("UNKNOWN/STALE"));
        assert!(text.contains("No samples yet / unknown"));
    }
    #[test]
    fn missing_metrics_small_empty_and_history_failure() {
        let tmp = tempfile::tempdir().unwrap();
        let mut c = config();
        c.data_dir = tmp.path().into();
        let mut app = App::default();
        app.refresh(&c, 80);
        assert!(
            app.history_error
                .as_deref()
                .unwrap()
                .contains("history unavailable")
        );
        assert!(!tmp.path().join("history.db").exists());
        for (w, h) in [(1, 1), (10, 3), (55, 12), (80, 24), (120, 40), (200, 70)] {
            let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
            for focus in 0..3 {
                app.focus = focus;
                term.draw(|f| render(f, &mut app, &c, 1000)).unwrap();
            }
        }
        std::fs::write(tmp.path().join("state.json"), "broken").unwrap();
        app.refresh(&c, 80);
        assert!(app.state_error.is_some());
        c.hosts.clear();
        app.refresh(&c, 80);
        let mut term = Terminal::new(TestBackend::new(80, 24)).unwrap();
        term.draw(|f| render(f, &mut app, &c, 1000)).unwrap();
    }
}

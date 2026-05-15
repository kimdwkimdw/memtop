use std::{
    io::{self, Stdout},
    sync::mpsc::{self, Receiver, TryRecvError},
    thread,
    time::{Duration, Instant},
};

use anyhow::Result;
use crossterm::{
    event::{
        self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyEventKind,
        KeyModifiers, MouseButton, MouseEventKind,
    },
    execute,
    terminal::{
        EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
        size as terminal_size,
    },
};
use ratatui::{
    Frame, Terminal,
    backend::CrosstermBackend,
    buffer::Buffer,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Widget, Wrap},
};

use crate::{
    Args, Snapshot, collect_snapshot,
    human::{format_kib, percent, truncate},
    procfs::MemoryMetric,
    project::{ProcessNode, ProjectNode},
    treemap::{self, Area},
};

pub fn run(args: Args) -> Result<()> {
    let mut terminal = setup_terminal()?;
    let result = run_loop(&mut terminal, args);
    restore_terminal(&mut terminal)?;
    result
}

struct AppState {
    args: Args,
    snapshot: Snapshot,
    selected_project: usize,
    selected_process: usize,
    zoomed_project: Option<ProjectIdentity>,
    pending_refresh: Option<Receiver<SnapshotResult>>,
    last_refresh: Instant,
    last_error: Option<String>,
}

type SnapshotResult = std::result::Result<Snapshot, String>;

#[derive(Debug, Clone, PartialEq, Eq)]
struct ProjectIdentity {
    name: String,
    path: String,
}

#[derive(Debug, Clone, Copy)]
struct AppAreas {
    treemap: Rect,
    details: Rect,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Hit {
    Project(usize),
    Process {
        project_index: usize,
        process_index: usize,
    },
}

#[derive(Debug, Clone, Copy)]
struct ProjectTileOptions {
    total: u64,
    color: Color,
    selected: bool,
    selected_process: Option<usize>,
    top_processes: usize,
}

impl AppState {
    fn new(args: Args) -> Result<Self> {
        let startup_metric = if args.metric == MemoryMetric::Rss {
            args.metric
        } else {
            MemoryMetric::Rss
        };
        let snapshot = collect_snapshot(
            args.min_memory_kib,
            startup_metric,
            args.metric,
            args.scan_threads,
        )?;
        let mut state = Self {
            args,
            snapshot,
            selected_project: 0,
            selected_process: 0,
            zoomed_project: None,
            pending_refresh: None,
            last_refresh: Instant::now(),
            last_error: None,
        };
        if state.snapshot.metric != state.snapshot.requested_metric {
            state.start_refresh();
        }
        Ok(state)
    }

    fn visible_projects(&self) -> &[ProjectNode] {
        let end = self.args.top_projects.min(self.snapshot.projects.len());
        &self.snapshot.projects[..end]
    }

    fn refresh(&mut self) {
        self.start_refresh();
    }

    fn start_refresh(&mut self) {
        if self.pending_refresh.is_some() {
            return;
        }

        let min_memory_kib = self.args.min_memory_kib;
        let metric = self.args.metric;
        let scan_threads = self.args.scan_threads;
        let (sender, receiver) = mpsc::channel();
        thread::spawn(move || {
            let result = collect_snapshot(min_memory_kib, metric, metric, scan_threads)
                .map_err(|error| error.to_string());
            let _ = sender.send(result);
        });

        self.pending_refresh = Some(receiver);
        self.last_refresh = Instant::now();
    }

    fn poll_refresh(&mut self) {
        let result = match self.pending_refresh.as_ref() {
            Some(receiver) => match receiver.try_recv() {
                Ok(result) => Some(result),
                Err(TryRecvError::Empty) => None,
                Err(TryRecvError::Disconnected) => {
                    Some(Err("snapshot worker disconnected".to_string()))
                }
            },
            None => None,
        };

        let Some(result) = result else {
            return;
        };
        self.pending_refresh = None;

        match result {
            Ok(snapshot) => {
                self.snapshot = snapshot;
                self.last_error = None;
                self.restore_zoomed_project();
                self.clamp_selection();
            }
            Err(error) => {
                self.last_error = Some(error.to_string());
            }
        }
        self.last_refresh = Instant::now();
    }

    fn select_next(&mut self) {
        if self.is_zoomed() {
            let len = self.visible_process_count();
            if len > 0 {
                self.selected_process = (self.selected_process + 1).min(len - 1);
            }
            return;
        }

        let len = self.visible_projects().len();
        if len > 0 {
            self.select_project((self.selected_project + 1).min(len - 1));
        }
    }

    fn select_previous(&mut self) {
        if self.is_zoomed() {
            self.selected_process = self.selected_process.saturating_sub(1);
        } else {
            self.select_project(self.selected_project.saturating_sub(1));
        }
    }

    fn select_project(&mut self, index: usize) {
        self.selected_project = index.min(self.visible_projects().len().saturating_sub(1));
        self.selected_process = 0;
        self.clamp_selection();
    }

    fn select_process(&mut self, index: usize) {
        self.selected_process = index.min(self.visible_process_count().saturating_sub(1));
    }

    fn zoom_in(&mut self) {
        if let Some(project) = self.selected_project() {
            self.zoomed_project = Some(ProjectIdentity {
                name: project.name.clone(),
                path: project.path.clone(),
            });
            self.clamp_selection();
        }
    }

    fn zoom_out(&mut self) {
        self.zoomed_project = None;
        self.clamp_selection();
    }

    fn is_zoomed(&self) -> bool {
        self.zoomed_project.is_some()
    }

    fn is_refreshing(&self) -> bool {
        self.pending_refresh.is_some()
    }

    fn selected_project(&self) -> Option<&ProjectNode> {
        self.visible_projects().get(self.selected_project)
    }

    fn selected_process(&self) -> Option<&ProcessNode> {
        self.selected_project().and_then(|project| {
            project
                .processes
                .iter()
                .take(self.args.top_processes)
                .nth(self.selected_process)
        })
    }

    fn visible_process_count(&self) -> usize {
        self.selected_project()
            .map(|project| project.processes.len().min(self.args.top_processes))
            .unwrap_or(0)
    }

    fn handle_click(&mut self, x: u16, y: u16, terminal_area: Rect) {
        let areas = app_areas(terminal_area);
        let inner = inner_rect(areas.treemap);
        let hit = if self.is_zoomed() {
            self.hit_zoomed_view(inner, x, y)
        } else {
            hit_project_view(
                self.visible_projects(),
                self.args.top_processes,
                inner,
                x,
                y,
            )
        };

        match hit {
            Some(Hit::Project(project_index)) => self.select_project(project_index),
            Some(Hit::Process {
                project_index,
                process_index,
            }) => {
                self.select_project(project_index);
                self.select_process(process_index);
            }
            None => {}
        }
    }

    fn hit_zoomed_view(&self, area: Rect, x: u16, y: u16) -> Option<Hit> {
        let project = self.selected_project()?;
        let process_index = hit_processes(project, self.args.top_processes, area, x, y)?;
        Some(Hit::Process {
            project_index: self.selected_project,
            process_index,
        })
    }

    fn restore_zoomed_project(&mut self) {
        let Some(identity) = self.zoomed_project.clone() else {
            return;
        };

        if let Some(index) = self
            .visible_projects()
            .iter()
            .position(|project| project.name == identity.name && project.path == identity.path)
        {
            self.selected_project = index;
        } else {
            self.zoomed_project = None;
        }
    }

    fn clamp_selection(&mut self) {
        self.selected_project = self
            .selected_project
            .min(self.visible_projects().len().saturating_sub(1));
        self.selected_process = self
            .selected_process
            .min(self.visible_process_count().saturating_sub(1));
    }
}

fn setup_terminal() -> Result<Terminal<CrosstermBackend<Stdout>>> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    terminal.clear()?;
    Ok(terminal)
}

fn restore_terminal(terminal: &mut Terminal<CrosstermBackend<Stdout>>) -> Result<()> {
    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        DisableMouseCapture,
        LeaveAlternateScreen
    )?;
    terminal.show_cursor()?;
    Ok(())
}

fn run_loop(terminal: &mut Terminal<CrosstermBackend<Stdout>>, args: Args) -> Result<()> {
    let mut state = AppState::new(args)?;
    let refresh_interval = Duration::from_millis(state.args.interval_ms.max(250));

    loop {
        state.poll_refresh();
        terminal.draw(|frame| draw(frame, &state))?;

        if event::poll(Duration::from_millis(100))? {
            match event::read()? {
                Event::Key(key) if should_handle_key(key) => match key.code {
                    KeyCode::Char('q') | KeyCode::Esc => break,
                    KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => break,
                    KeyCode::Char('r') => state.refresh(),
                    KeyCode::Enter | KeyCode::Char('z') => state.zoom_in(),
                    KeyCode::Backspace | KeyCode::Char('x') => state.zoom_out(),
                    KeyCode::Down | KeyCode::Char('j') => state.select_next(),
                    KeyCode::Up | KeyCode::Char('k') => state.select_previous(),
                    _ => {}
                },
                Event::Mouse(mouse) => {
                    if matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left)) {
                        let (width, height) = terminal_size()?;
                        state.handle_click(mouse.column, mouse.row, Rect::new(0, 0, width, height));
                    }
                }
                _ => {}
            }
        }

        if state.last_refresh.elapsed() >= refresh_interval {
            state.refresh();
        }
    }

    Ok(())
}

fn should_handle_key(key: KeyEvent) -> bool {
    matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat)
}

fn draw(frame: &mut Frame<'_>, state: &AppState) {
    let area = frame.area();
    let areas = app_areas(area);
    let sections = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(8),
            Constraint::Length(3),
        ])
        .split(area);

    frame.render_widget(header(state), sections[0]);

    let projects = state.visible_projects();
    frame.render_widget(
        TreemapWidget {
            projects,
            selected_project: state.selected_project,
            selected_process: state.selected_process,
            top_processes: state.args.top_processes,
            zoomed: state.is_zoomed(),
        },
        areas.treemap,
    );
    frame.render_widget(details(state), areas.details);
    frame.render_widget(footer(state), sections[2]);
}

fn app_areas(area: Rect) -> AppAreas {
    let sections = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(8),
            Constraint::Length(3),
        ])
        .split(area);

    let body = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(72), Constraint::Percentage(28)])
        .split(sections[1]);

    AppAreas {
        treemap: body[0],
        details: body[1],
    }
}

fn header(state: &AppState) -> Paragraph<'_> {
    let observed = state.snapshot.observed_memory_kib;
    let total = state.snapshot.mem_total_kib;
    let used = state
        .snapshot
        .mem_total_kib
        .saturating_sub(state.snapshot.mem_available_kib);
    let metric_note = if state.snapshot.metric != state.snapshot.requested_metric {
        format!(
            " preview, loading {}",
            state.snapshot.requested_metric.label()
        )
    } else if state.is_refreshing() {
        format!(" refreshing {}", state.args.metric.label())
    } else {
        String::new()
    };
    let fallback = if state.snapshot.fallback_process_count > 0 {
        format!("  {} RSS fallback", state.snapshot.fallback_process_count)
    } else {
        String::new()
    };

    let lines = vec![
        Line::from(vec![
            Span::styled(
                "memtop",
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(format!(
                "  process {} sum {}{}  system used {} ({:.1}% of total)",
                state.snapshot.metric.label(),
                format_kib(observed),
                metric_note,
                format_kib(used),
                percent(used, total)
            )),
        ]),
        Line::raw(format!(
            "{} projects  {} processes >= {} {}  refresh {}ms  mode {}{}",
            state.snapshot.projects.len(),
            state.snapshot.filtered_process_count,
            format_kib(state.args.min_memory_kib),
            state.snapshot.metric.label(),
            state.args.interval_ms,
            if state.is_zoomed() {
                "project zoom"
            } else {
                "projects"
            },
            fallback
        )),
    ];

    Paragraph::new(lines).block(Block::default().borders(Borders::BOTTOM))
}

fn details(state: &AppState) -> Paragraph<'_> {
    let projects = state.visible_projects();
    let mut lines = Vec::new();

    if let Some(project) = projects.get(state.selected_project) {
        lines.push(Line::from(Span::styled(
            project.name.clone(),
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        )));
        lines.push(Line::raw(project.path.clone()));
        lines.push(Line::raw(format!(
            "{}  {:.1}% displayed {}",
            format_kib(project.total_memory_kib),
            percent(project.total_memory_kib, state.snapshot.observed_memory_kib),
            state.snapshot.metric.label()
        )));

        if state.is_zoomed() {
            lines.push(Line::raw("zoomed into project processes"));
        }
        lines.push(Line::raw(""));

        if let Some(process) = state.selected_process() {
            lines.push(Line::from(Span::styled(
                "Selected process",
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            )));
            lines.push(Line::from(vec![
                Span::styled(
                    format!("{:>8}", format_kib(process.memory_kib)),
                    Style::default().fg(Color::LightGreen),
                ),
                Span::raw(format!("  pid {}  {}", process.pid, process.name)),
            ]));
            lines.push(Line::raw(format!("  {}", truncate(&process.command, 180))));
            lines.push(Line::raw(""));
        }

        for (index, process) in project
            .processes
            .iter()
            .take(state.args.top_processes)
            .enumerate()
        {
            let marker = if index == state.selected_process {
                ">"
            } else {
                " "
            };
            lines.push(Line::from(vec![
                Span::raw(marker),
                Span::styled(
                    format!("{:>8}", format_kib(process.memory_kib)),
                    Style::default().fg(Color::LightGreen),
                ),
                Span::raw(format!("  pid {}  {}", process.pid, process.name)),
            ]));
            lines.push(Line::raw(format!("  {}", truncate(&process.command, 180))));
        }
    } else {
        lines.push(Line::raw("No processes matched the current filter."));
    }

    Paragraph::new(lines)
        .block(Block::default().title("Selection").borders(Borders::ALL))
        .wrap(Wrap { trim: true })
}

fn footer(state: &AppState) -> Paragraph<'_> {
    let error = state
        .last_error
        .as_ref()
        .map(|error| format!("  last error: {error}"))
        .unwrap_or_default();
    Paragraph::new(Line::raw(format!(
        "q quit  click select  up/down move  Enter/z zoom in  Backspace/x zoom out  r refresh  top {} projects / {} processes{}",
        state.args.top_projects, state.args.top_processes, error
    )))
    .block(Block::default().borders(Borders::TOP))
}

struct TreemapWidget<'a> {
    projects: &'a [ProjectNode],
    selected_project: usize,
    selected_process: usize,
    top_processes: usize,
    zoomed: bool,
}

impl Widget for TreemapWidget<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let title = if self.zoomed {
            self.projects
                .get(self.selected_project)
                .map(|project| format!("Treemap > {}", project.name))
                .unwrap_or_else(|| "Treemap".to_string())
        } else {
            "Treemap".to_string()
        };
        let block = Block::default().title(title).borders(Borders::ALL);
        let inner = block.inner(area);
        block.render(area, buf);

        if self.projects.is_empty() || inner.width == 0 || inner.height == 0 {
            write_text(buf, inner, "No process memory to display", Style::default());
            return;
        }

        if self.zoomed {
            if let Some(project) = self.projects.get(self.selected_project) {
                draw_zoomed_project(
                    buf,
                    inner,
                    project,
                    self.selected_process,
                    self.top_processes,
                    palette(self.selected_project),
                );
            }
            return;
        }

        let weights: Vec<u64> = self
            .projects
            .iter()
            .map(|project| project.total_memory_kib)
            .collect();
        let total = weights.iter().sum::<u64>();
        let tiles = treemap::layout(&weights, to_area(inner));

        for tile in tiles {
            let project = &self.projects[tile.index];
            let rect = to_rect(tile.area);
            let selected = tile.index == self.selected_project;
            let color = palette(tile.index);
            draw_project(
                buf,
                rect,
                project,
                ProjectTileOptions {
                    total,
                    color,
                    selected,
                    selected_process: selected.then_some(self.selected_process),
                    top_processes: self.top_processes,
                },
            );
        }
    }
}

fn draw_project(buf: &mut Buffer, rect: Rect, project: &ProjectNode, options: ProjectTileOptions) {
    let color = options.color;
    let style = Style::default().bg(color).fg(Color::Black);
    fill_rect(buf, rect, style);

    if rect.width >= 4 && rect.height >= 3 {
        draw_border(buf, rect, options.selected);
    }

    let text_style = if options.selected {
        Style::default()
            .bg(color)
            .fg(Color::White)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default()
            .bg(color)
            .fg(Color::Black)
            .add_modifier(Modifier::BOLD)
    };

    if rect.width >= 8 && rect.height >= 2 {
        let title = format!(
            "{} {} {:.1}%",
            project.name,
            format_kib(project.total_memory_kib),
            percent(project.total_memory_kib, options.total)
        );
        write_text(
            buf,
            Rect {
                x: rect.x.saturating_add(1),
                y: rect.y.saturating_add(1),
                width: rect.width.saturating_sub(2),
                height: 1,
            },
            &title,
            text_style,
        );
    }

    if rect.width < 12 || rect.height < 6 {
        return;
    }

    let process_area = Rect {
        x: rect.x.saturating_add(1),
        y: rect.y.saturating_add(2),
        width: rect.width.saturating_sub(2),
        height: rect.height.saturating_sub(3),
    };
    let processes: Vec<_> = project
        .processes
        .iter()
        .take(options.top_processes)
        .collect();
    let process_weights: Vec<u64> = processes.iter().map(|process| process.memory_kib).collect();

    for tile in treemap::layout(&process_weights, to_area(process_area)) {
        let process = processes[tile.index];
        let process_rect = to_rect(tile.area);
        draw_process_tile(
            buf,
            process_rect,
            process,
            dim_color(color),
            options.selected_process == Some(tile.index),
        );
    }
}

fn draw_zoomed_project(
    buf: &mut Buffer,
    rect: Rect,
    project: &ProjectNode,
    selected_process: usize,
    top_processes: usize,
    color: Color,
) {
    let processes: Vec<_> = project.processes.iter().take(top_processes).collect();
    if processes.is_empty() {
        write_text(buf, rect, "No processes to display", Style::default());
        return;
    }

    let total = processes
        .iter()
        .map(|process| process.memory_kib)
        .sum::<u64>();
    let weights: Vec<u64> = processes.iter().map(|process| process.memory_kib).collect();
    let tiles = treemap::layout(&weights, to_area(rect));

    for tile in tiles {
        let process = processes[tile.index];
        let process_rect = to_rect(tile.area);
        draw_process_tile(
            buf,
            process_rect,
            process,
            color_for_process(color, tile.index),
            tile.index == selected_process,
        );

        if process_rect.width >= 14 && process_rect.height >= 3 {
            let label = format!(
                "{:.1}% of {}",
                percent(process.memory_kib, total),
                project.name
            );
            write_text(
                buf,
                Rect {
                    x: process_rect.x.saturating_add(1),
                    y: process_rect.y.saturating_add(1),
                    width: process_rect.width.saturating_sub(2),
                    height: 1,
                },
                &label,
                Style::default()
                    .bg(color_for_process(color, tile.index))
                    .fg(Color::White),
            );
        }
    }
}

fn draw_process_tile(
    buf: &mut Buffer,
    rect: Rect,
    process: &ProcessNode,
    color: Color,
    selected: bool,
) {
    let process_style = Style::default().bg(color).fg(Color::White);
    fill_rect(buf, rect, process_style);

    if rect.width >= 4 && rect.height >= 3 {
        draw_border(buf, rect, selected);
    }

    if rect.width >= 10 && rect.height >= 2 {
        let label = format!("{} {}", process.name, format_kib(process.memory_kib));
        write_text(
            buf,
            Rect {
                x: rect.x.saturating_add(1),
                y: rect.y,
                width: rect.width.saturating_sub(2),
                height: 1,
            },
            &label,
            process_style,
        );
    }
}

fn hit_project_view(
    projects: &[ProjectNode],
    top_processes: usize,
    area: Rect,
    x: u16,
    y: u16,
) -> Option<Hit> {
    if !rect_contains(area, x, y) {
        return None;
    }

    let weights: Vec<u64> = projects
        .iter()
        .map(|project| project.total_memory_kib)
        .collect();

    for tile in treemap::layout(&weights, to_area(area)) {
        let project_rect = to_rect(tile.area);
        if !rect_contains(project_rect, x, y) {
            continue;
        }

        let project = &projects[tile.index];
        if project_rect.width >= 12 && project_rect.height >= 6 {
            let process_area = Rect {
                x: project_rect.x.saturating_add(1),
                y: project_rect.y.saturating_add(2),
                width: project_rect.width.saturating_sub(2),
                height: project_rect.height.saturating_sub(3),
            };

            if let Some(process_index) = hit_processes(project, top_processes, process_area, x, y) {
                return Some(Hit::Process {
                    project_index: tile.index,
                    process_index,
                });
            }
        }

        return Some(Hit::Project(tile.index));
    }

    None
}

fn hit_processes(
    project: &ProjectNode,
    top_processes: usize,
    area: Rect,
    x: u16,
    y: u16,
) -> Option<usize> {
    if !rect_contains(area, x, y) {
        return None;
    }

    let weights: Vec<u64> = project
        .processes
        .iter()
        .take(top_processes)
        .map(|process| process.memory_kib)
        .collect();

    treemap::layout(&weights, to_area(area))
        .into_iter()
        .find(|tile| rect_contains(to_rect(tile.area), x, y))
        .map(|tile| tile.index)
}

fn fill_rect(buf: &mut Buffer, rect: Rect, style: Style) {
    for y in rect.y..rect.y.saturating_add(rect.height) {
        for x in rect.x..rect.x.saturating_add(rect.width) {
            buf[(x, y)].set_symbol(" ").set_style(style);
        }
    }
}

fn draw_border(buf: &mut Buffer, rect: Rect, selected: bool) {
    let style = if selected {
        Style::default()
            .fg(Color::White)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::Black)
    };
    let right = rect.x + rect.width - 1;
    let bottom = rect.y + rect.height - 1;

    for x in rect.x..=right {
        buf[(x, rect.y)].set_symbol("-").set_style(style);
        buf[(x, bottom)].set_symbol("-").set_style(style);
    }
    for y in rect.y..=bottom {
        buf[(rect.x, y)].set_symbol("|").set_style(style);
        buf[(right, y)].set_symbol("|").set_style(style);
    }
    buf[(rect.x, rect.y)].set_symbol("+").set_style(style);
    buf[(right, rect.y)].set_symbol("+").set_style(style);
    buf[(rect.x, bottom)].set_symbol("+").set_style(style);
    buf[(right, bottom)].set_symbol("+").set_style(style);
}

fn write_text(buf: &mut Buffer, area: Rect, text: &str, style: Style) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let truncated = truncate_chars(text, area.width as usize);
    buf.set_stringn(area.x, area.y, truncated, area.width as usize, style);
}

fn truncate_chars(text: &str, max_chars: usize) -> String {
    truncate(text, max_chars)
}

fn inner_rect(rect: Rect) -> Rect {
    Rect {
        x: rect.x.saturating_add(1),
        y: rect.y.saturating_add(1),
        width: rect.width.saturating_sub(2),
        height: rect.height.saturating_sub(2),
    }
}

fn rect_contains(rect: Rect, x: u16, y: u16) -> bool {
    x >= rect.x
        && x < rect.x.saturating_add(rect.width)
        && y >= rect.y
        && y < rect.y.saturating_add(rect.height)
}

fn to_area(rect: Rect) -> Area {
    Area {
        x: rect.x,
        y: rect.y,
        width: rect.width,
        height: rect.height,
    }
}

fn to_rect(area: Area) -> Rect {
    Rect {
        x: area.x,
        y: area.y,
        width: area.width,
        height: area.height,
    }
}

fn palette(index: usize) -> Color {
    const COLORS: [Color; 8] = [
        Color::Rgb(73, 145, 214),
        Color::Rgb(75, 171, 116),
        Color::Rgb(224, 171, 70),
        Color::Rgb(205, 103, 103),
        Color::Rgb(151, 118, 205),
        Color::Rgb(67, 168, 185),
        Color::Rgb(191, 132, 74),
        Color::Rgb(118, 150, 92),
    ];
    COLORS[index % COLORS.len()]
}

fn dim_color(color: Color) -> Color {
    match color {
        Color::Rgb(red, green, blue) => Color::Rgb(red / 2, green / 2, blue / 2),
        other => other,
    }
}

fn color_for_process(project_color: Color, index: usize) -> Color {
    match project_color {
        Color::Rgb(red, green, blue) => {
            let factor = 58 + ((index as u16 * 17) % 28);
            Color::Rgb(
                ((red as u16 * factor) / 100) as u8,
                ((green as u16 * factor) / 100) as u8,
                ((blue as u16 * factor) / 100) as u8,
            )
        }
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hit_project_title_selects_project() {
        let projects = sample_projects();
        let hit = hit_project_view(&projects, 2, Rect::new(0, 0, 100, 20), 2, 1);
        assert_eq!(hit, Some(Hit::Project(0)));
    }

    #[test]
    fn hit_nested_process_selects_process() {
        let projects = sample_projects();
        let hit = hit_project_view(&projects, 2, Rect::new(0, 0, 100, 20), 2, 3);
        assert_eq!(
            hit,
            Some(Hit::Process {
                project_index: 0,
                process_index: 0,
            })
        );
    }

    #[test]
    fn process_hit_test_returns_none_without_visible_processes() {
        let projects = sample_projects();
        let project = &projects[0];
        let hit = hit_processes(project, 0, Rect::new(0, 0, 100, 20), 2, 3);
        assert_eq!(hit, None);
    }

    #[test]
    fn selected_process_respects_top_process_limit() {
        let mut state = sample_state(0);
        assert!(state.selected_process().is_none());

        state.args.top_processes = 1;
        assert_eq!(state.selected_process().unwrap().pid, 1);

        state.selected_process = 1;
        assert!(state.selected_process().is_none());
    }

    fn sample_state(top_processes: usize) -> AppState {
        AppState {
            args: Args {
                interval_ms: 2000,
                min_memory_kib: 1024,
                metric: MemoryMetric::Pss,
                scan_threads: 4,
                top_projects: 24,
                top_processes,
                once: false,
            },
            snapshot: Snapshot {
                metric: MemoryMetric::Pss,
                requested_metric: MemoryMetric::Pss,
                mem_total_kib: 100,
                mem_available_kib: 50,
                observed_memory_kib: 100,
                filtered_process_count: 3,
                fallback_process_count: 0,
                projects: sample_projects(),
            },
            selected_project: 0,
            selected_process: 0,
            zoomed_project: None,
            pending_refresh: None,
            last_refresh: Instant::now(),
            last_error: None,
        }
    }

    fn sample_projects() -> Vec<ProjectNode> {
        vec![
            ProjectNode {
                name: "alpha".to_string(),
                path: "~/prj/alpha".to_string(),
                total_memory_kib: 80,
                processes: vec![
                    ProcessNode {
                        pid: 1,
                        ppid: 0,
                        name: "large".to_string(),
                        command: "large".to_string(),
                        memory_kib: 60,
                    },
                    ProcessNode {
                        pid: 2,
                        ppid: 0,
                        name: "small".to_string(),
                        command: "small".to_string(),
                        memory_kib: 20,
                    },
                ],
            },
            ProjectNode {
                name: "beta".to_string(),
                path: "~/prj/beta".to_string(),
                total_memory_kib: 20,
                processes: vec![ProcessNode {
                    pid: 3,
                    ppid: 0,
                    name: "other".to_string(),
                    command: "other".to_string(),
                    memory_kib: 20,
                }],
            },
        ]
    }
}

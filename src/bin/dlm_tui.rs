use anyhow::Result;
use crossterm::{
    event::{self, Event, KeyCode},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use grab_cli::download::async_impl;
use grab_cli::download::progress::ProgressMsg;

use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    prelude::*,
    widgets::{Block, Borders, Cell, Clear, Paragraph, Row, Table, Wrap},
};
use reqwest::Client;
use std::{io, time::Duration};
use tokio::sync::mpsc::{self, Receiver, Sender};

/// Row information shown in the table
#[derive(Clone)]
struct RowData {
    id: usize,
    url: String,
    file: String,            // current filename on disk (displayed and used)
    status: String,          // "Queued" | "Running" | "Paused" | "Canceled" | "Done" | "Error"
    done: u64,               // bytes downloaded so far
    total: Option<u64>,      // total bytes if known
    detail: Option<String>,  // full error/details for Info modal
    resumable: Option<bool>, // whether server supports resume (Accept-Ranges)
}

/// Application modes
enum UiMode {
    Normal,
    Input(String),                       // buffer while typing a URL
    Info(String),                        // info popup text for selected row
    Renaming(String),                    // inline filename editing buffer
    ConfirmDelete { delete_file: bool }, // confirmation modal (d/D)
}

// Using shared ProgressMsg from crate::download::progress

/// Main application state
struct App {
    rows: Vec<RowData>,
    selected: usize,
    mode: UiMode,
    // Track active tasks so we can pause/cancel/delete
    tasks: std::collections::HashMap<usize, tokio::task::JoinHandle<()>>,
}

/// Entry point – sets up terminal & launches event loop
#[tokio::main]
async fn main() -> Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let res = run_app(&mut terminal).await;

    // always restore terminal
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    res
}

/// Runs the interactive loop.  Spawn download tasks as the user adds URLs.
async fn run_app(terminal: &mut Terminal<CrosstermBackend<io::Stdout>>) -> Result<()> {
    let (tx, mut rx): (Sender<ProgressMsg>, Receiver<ProgressMsg>) = mpsc::channel(200);

    let mut app = App {
        rows: Vec::new(),
        selected: 0,
        mode: UiMode::Normal,
        tasks: std::collections::HashMap::new(),
    };
    let mut next_id: usize = 1;

    loop {
        // --- handle inbound progress messages ---
        while let Ok(msg) = rx.try_recv() {
            match msg {
                ProgressMsg::Started { id, total } => {
                    if let Some(row) = app.rows.iter_mut().find(|r| r.id == id) {
                        row.total = total;
                        row.status = "Running".into();
                        row.detail = None;
                    }
                }
                ProgressMsg::Progress { id, delta } => {
                    if let Some(row) = app.rows.iter_mut().find(|r| r.id == id) {
                        row.done += delta;
                    }
                }
                ProgressMsg::Finished { id } => {
                    if let Some(row) = app.rows.iter_mut().find(|r| r.id == id) {
                        row.status = "Done".into();
                        row.detail = None;
                    }
                    // task ends itself; remove handle if still present
                    app.tasks.remove(&id);
                }
                ProgressMsg::Failed { id, err } => {
                    if let Some(row) = app.rows.iter_mut().find(|r| r.id == id) {
                        row.status = "Error".into(); // short table label
                        row.detail = Some(err); // full message for Info popup
                    }
                    app.tasks.remove(&id);
                }
                ProgressMsg::Renamed { id, file } => {
                    if let Some(row) = app.rows.iter_mut().find(|r| r.id == id) {
                        row.file = file;
                    }
                }
                ProgressMsg::Resumable { id, resumable } => {
                    if let Some(row) = app.rows.iter_mut().find(|r| r.id == id) {
                        row.resumable = Some(resumable);
                    }
                }
            }
        }

        // --- input polling ---
        if event::poll(Duration::from_millis(100))? {
            if let Event::Key(key) = event::read()? {
                match (&mut app.mode, key.code) {
                    // Close modals with q or Esc (does not quit the app)
                    (UiMode::Input(_), KeyCode::Char('q'))
                    | (UiMode::Input(_), KeyCode::Esc)
                    | (UiMode::Info(_), KeyCode::Char('q'))
                    | (UiMode::Info(_), KeyCode::Esc)
                    | (UiMode::Renaming(_), KeyCode::Char('q'))
                    | (UiMode::Renaming(_), KeyCode::Esc) => {
                        app.mode = UiMode::Normal;
                    }

                    // Quit only in Normal mode
                    (UiMode::Normal, KeyCode::Char('q')) => break,

                    // ----- NORMAL MODE -----
                    (UiMode::Normal, KeyCode::Up) => {
                        if !app.rows.is_empty() {
                            app.selected = (app.selected + app.rows.len() - 1) % app.rows.len();
                        }
                    }
                    (UiMode::Normal, KeyCode::Down) => {
                        if !app.rows.is_empty() {
                            app.selected = (app.selected + 1) % app.rows.len();
                        }
                    }
                    (UiMode::Normal, KeyCode::Char('a')) => {
                        app.mode = UiMode::Input(String::new());
                    }
                    (UiMode::Normal, KeyCode::Char('i')) => {
                        if let Some(r) = app.rows.get(app.selected) {
                            let text = format_info_text(r);
                            app.mode = UiMode::Info(text);
                        }
                    }
                    // Pause
                    (UiMode::Normal, KeyCode::Char('p')) => {
                        if let Some(r) = app.rows.get_mut(app.selected) {
                            if r.status.starts_with("Running") {
                                if let Some(handle) = app.tasks.remove(&r.id) {
                                    handle.abort();
                                }
                                r.status = "Paused".into();
                            }
                        }
                    }
                    // Cancel (if running) or Continue (if paused/canceled/error/queued)
                    (UiMode::Normal, KeyCode::Char('c')) => {
                        if let Some(r) = app.rows.get_mut(app.selected) {
                            if r.status.starts_with("Running") {
                                if let Some(handle) = app.tasks.remove(&r.id) {
                                    handle.abort();
                                }
                                r.status = "Canceled".into();
                            } else {
                                // Continue / (Re)start with resume support
                                let existing = tokio::fs::metadata(&r.file)
                                    .await
                                    .map(|m| m.len())
                                    .unwrap_or(0);
                                r.done = existing;
                                r.status = "Queued".into();
                                let handle = spawn_download_task(
                                    r.id,
                                    r.url.clone(),
                                    r.file.clone(),
                                    tx.clone(),
                                );
                                app.tasks.insert(r.id, handle);
                            }
                        }
                    }
                    // Rename inline (only when not running)
                    (UiMode::Normal, KeyCode::Char('r')) => {
                        if let Some(r) = app.rows.get(app.selected) {
                            if !r.status.starts_with("Running") {
                                app.mode = UiMode::Renaming(r.file.clone());
                            }
                        }
                    }
                    // Delete record only (no confirmation)
                    (UiMode::Normal, KeyCode::Char('d')) => {
                        if app.rows.is_empty() {
                            // nothing
                        } else {
                            let idx = app.selected;
                            let row_id = app.rows[idx].id;
                            // Abort any running task
                            if let Some(handle) = app.tasks.remove(&row_id) {
                                handle.abort();
                            }
                            // Remove row only; keep file on disk
                            app.rows.remove(idx);
                            if !app.rows.is_empty() {
                                app.selected = app.selected.min(app.rows.len() - 1);
                            } else {
                                app.selected = 0;
                            }
                        }
                    }
                    // Delete file + record (confirmation)
                    (UiMode::Normal, KeyCode::Char('D')) => {
                        if !app.rows.is_empty() {
                            app.mode = UiMode::ConfirmDelete { delete_file: true };
                        }
                    }

                    // ----- INPUT MODE -----
                    (UiMode::Input(buf), KeyCode::Backspace) => {
                        buf.pop();
                    }
                    (UiMode::Input(buf), KeyCode::Char(c)) => {
                        buf.push(c);
                    }
                    (UiMode::Input(buf), KeyCode::Enter) => {
                        let url = buf.trim().to_string();
                        if !url.is_empty() {
                            let id = next_id;
                            next_id += 1;
                            let file = filename_from_url(&url);
                            // Existing partial?
                            let existing = tokio::fs::metadata(&file)
                                .await
                                .map(|m| m.len())
                                .unwrap_or(0);

                            // Create row
                            app.rows.push(RowData {
                                id,
                                url: url.clone(),
                                file: file.clone(),
                                status: "Queued".into(),
                                done: existing,
                                total: None,
                                detail: None,
                                resumable: None,
                            });

                            // Spawn download (will resume if possible)
                            let handle =
                                spawn_download_task(id, url.clone(), file.clone(), tx.clone());
                            app.tasks.insert(id, handle);
                        }
                        app.mode = UiMode::Normal;
                    }

                    // ----- RENAMING MODE -----
                    (UiMode::Renaming(buf), KeyCode::Backspace) => {
                        buf.pop();
                    }
                    (UiMode::Renaming(buf), KeyCode::Char(c)) => {
                        buf.push(c);
                    }
                    (UiMode::Renaming(buf), KeyCode::Enter) => {
                        if let Some(r) = app.rows.get_mut(app.selected) {
                            let new_name = buf.trim();
                            if !new_name.is_empty() && new_name != r.file {
                                // Only allow when not running
                                if !r.status.starts_with("Running") {
                                    match tokio::fs::rename(&r.file, new_name).await {
                                        Ok(()) => {
                                            r.file = new_name.to_string();
                                            r.detail = None;
                                        }
                                        Err(e) => {
                                            r.status = "Error".into();
                                            r.detail = Some(format!("Rename failed: {}", e));
                                        }
                                    }
                                }
                            }
                        }
                        app.mode = UiMode::Normal;
                    }

                    // ----- CONFIRM DELETE MODE -----
                    (UiMode::ConfirmDelete { delete_file }, KeyCode::Char('y')) => {
                        if app.rows.is_empty() {
                            app.mode = UiMode::Normal;
                        } else {
                            let idx = app.selected;
                            let row_id = app.rows[idx].id;
                            // abort any running task
                            if let Some(handle) = app.tasks.remove(&row_id) {
                                handle.abort();
                            }
                            if *delete_file {
                                let file_to_delete = app.rows[idx].file.clone();
                                let _ = tokio::fs::remove_file(&file_to_delete).await;
                            }
                            app.rows.remove(idx);
                            if !app.rows.is_empty() {
                                app.selected = app.selected.min(app.rows.len() - 1);
                            } else {
                                app.selected = 0;
                            }
                            app.mode = UiMode::Normal;
                        }
                    }
                    (UiMode::ConfirmDelete { .. }, KeyCode::Char('n')) => {
                        app.mode = UiMode::Normal;
                    }
                    (UiMode::ConfirmDelete { .. }, KeyCode::Esc) => {
                        app.mode = UiMode::Normal;
                    }
                    (UiMode::ConfirmDelete { .. }, KeyCode::Char('q')) => {
                        app.mode = UiMode::Normal;
                    }

                    _ => {}
                }
            }
        }

        // --- Draw UI ---
        terminal.draw(|f| draw_ui(f, &app))?;
    }

    Ok(())
}

/// Spawns an async task that performs the download and reports progress
fn spawn_download_task(
    id: usize,
    url: String,
    file: String,
    tx: Sender<ProgressMsg>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let client = Client::builder()
            .user_agent("grab-cli")
            .build()
            .expect("http client");
        if let Err(e) = download_with_progress(&client, &url, &file, id, tx.clone()).await {
            let _ = tx
                .send(ProgressMsg::Failed {
                    id,
                    err: e.to_string(),
                })
                .await;
        }
    })
}

/// Async download with resume support that reports progress through a channel
async fn download_with_progress(
    client: &Client,
    url: &str,
    _file: &str,
    id: usize,
    tx: Sender<ProgressMsg>,
) -> Result<()> {
    // Delegate to unified downloader that emits progress and supports resume/rename
    async_impl::download_with_progress(client, url, id, tx).await
}

/// Draws the interface without a top header or downloads block.
/// Adds a bottom "Hints" bar. Columns: File | Status | Progress.
fn draw_ui(f: &mut Frame<'_>, app: &App) {
    let size = f.size();

    // Layout split: table + hints (no header)
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .margin(1)
        .constraints([Constraint::Min(0), Constraint::Length(4)])
        .split(size);

    // Build table rows
    let table_rows: Vec<Row> = app
        .rows
        .iter()
        .enumerate()
        .map(|(idx, r)| {
            let mut file_cell = r.file.clone();

            // If we're renaming the selected row, show inline editor buffer
            if let UiMode::Renaming(ref buf) = app.mode {
                if idx == app.selected {
                    file_cell = format!("[ {} ]", buf);
                }
            }

            let prog_text = match r.total {
                Some(t) if t > 0 => {
                    let pct = r.done.saturating_mul(100) / t;
                    format!("{pct}%")
                }
                _ => human_bytes(r.done),
            };

            let base = Row::new(vec![
                Cell::from(file_cell),
                Cell::from(r.status.as_str()),
                Cell::from(prog_text),
            ]);
            if idx == app.selected {
                base.style(Style::default().bg(Color::Blue))
            } else {
                base
            }
        })
        .collect();

    // Minimalist table: only header row, no borders/block
    let widths = [
        Constraint::Percentage(70),
        Constraint::Length(16),
        Constraint::Length(12),
    ];
    let table = Table::new(table_rows, widths).header(
        Row::new(vec!["File", "Status", "Progress"]).style(Style::default().fg(Color::Yellow)),
    );
    f.render_widget(table, chunks[0]);

    // Bottom hints bar (wrapped to fit narrow terminals)
    let hints_text =
        "a: Add URL   i: Info   q: Quit   p: Pause   c: Cancel/Continue   r: Rename\nd: Delete record   D: Delete file   ↑/↓: Select   Enter/Esc: Modals";
    let hints = Paragraph::new(hints_text)
        .wrap(Wrap { trim: true })
        .block(Block::default().title("Hints").borders(Borders::ALL));
    f.render_widget(hints, chunks[1]);

    // Info popup overlay
    if let UiMode::Info(ref text) = app.mode {
        let area = centered_rect(70, 10, size);
        let info = Paragraph::new(text.as_str())
            .wrap(Wrap { trim: true })
            .block(
                Block::default()
                    .title("Info (q/Esc to close)")
                    .borders(Borders::ALL),
            );
        f.render_widget(Clear, area);
        f.render_widget(info, area);
    }

    // Confirm delete overlay
    if let UiMode::ConfirmDelete { delete_file } = &app.mode {
        let area = centered_rect(70, 7, size);
        let (title, body) = if *delete_file {
            (
                "Confirm Delete",
                "Delete file from disk and remove row?\n(y) Yes   (n) No",
            )
        } else {
            (
                "Confirm Delete",
                "Delete record only (keep file)?\n(y) Yes   (n) No",
            )
        };
        let confirm = Paragraph::new(body)
            .wrap(Wrap { trim: true })
            .block(Block::default().title(title).borders(Borders::ALL));
        f.render_widget(Clear, area);
        f.render_widget(confirm, area);
    }

    // Add URL overlay
    if let UiMode::Input(ref buf) = app.mode {
        let area = centered_rect(60, 3, size);
        let prompt = Paragraph::new(buf.as_str())
            .block(Block::default().title("Add URL").borders(Borders::ALL));
        f.render_widget(Clear, area);
        f.render_widget(prompt, area);
    }
}

/// Helper to center a rect
fn centered_rect(percent_x: u16, height: u16, r: Rect) -> Rect {
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage(50 - height / 2),
            Constraint::Length(height),
            Constraint::Percentage(50 - height / 2),
        ])
        .split(r);

    let horizontal = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage(50 - percent_x / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage(50 - percent_x / 2),
        ])
        .split(vertical[1]);
    horizontal[1]
}

/// Safer filename helper for display and saving
fn filename_from_url(url: &str) -> String {
    // Strip fragment and query, trim trailing slash, take last path segment
    let base = url.split('#').next().unwrap_or(url);
    let base = base.split('?').next().unwrap_or(base);
    let base = base.trim_end_matches('/');
    let seg = base.rsplit('/').next().unwrap_or("");
    if seg.is_empty() {
        "download.bin".to_string()
    } else {
        seg.to_string()
    }
}

fn human_bytes(n: u64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = 1024.0 * 1024.0;
    const GB: f64 = 1024.0 * 1024.0 * 1024.0;
    let x = n as f64;
    if x >= GB {
        format!("{:.1} GiB", x / GB)
    } else if x >= MB {
        format!("{:.1} MiB", x / MB)
    } else if x >= KB {
        format!("{:.1} KiB", x / KB)
    } else {
        format!("{} B", n)
    }
}

/// Formats the text for the Info popup
fn format_info_text(r: &RowData) -> String {
    let prog_line = match (r.done, r.total) {
        (done, Some(t)) if t > 0 => {
            let pct = done.saturating_mul(100) / t;
            format!("Progress: {} / {} bytes ({}%)", done, t, pct)
        }
        (done, _) => format!("Progress: {} bytes (total unknown)", done),
    };

    let resumable_str = match r.resumable {
        Some(true) => "True",
        Some(false) => "False",
        None => "Unknown",
    };

    if let Some(detail) = &r.detail {
        format!(
            "File: {}\nStatus: {}\nResumable: {}\n\nFull error:\n{}",
            r.file, r.status, resumable_str, detail
        )
    } else {
        format!(
            "File: {}\nStatus: {}\nResumable: {}\n{}",
            r.file, r.status, resumable_str, prog_line
        )
    }
}

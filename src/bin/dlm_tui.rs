use anyhow::Result;
use crossterm::{
    event::{self, Event, KeyCode},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use futures_util::StreamExt;
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    prelude::*,
    widgets::{Block, Borders, Cell, Clear, Paragraph, Row, Table, Wrap},
};
use reqwest::Client;
use std::{io, time::Duration};
use tokio::{
    fs::File,
    io::AsyncWriteExt,
    sync::mpsc::{self, Receiver, Sender},
};

/// Row information shown in the table
#[derive(Clone)]
struct RowData {
    id: usize,
    url: String,
    status: String, // short label for table (e.g., "Running", "Done", "Error")
    done: u64,
    total: Option<u64>,
    detail: Option<String>, // full error/details for Info modal
}

/// Application modes
enum UiMode {
    Normal,
    Input(String), // buffer while typing a URL
    Info(String),  // info popup text for selected row
}

/// Messages sent from download tasks back to the UI
enum ProgressMsg {
    Started { id: usize, total: Option<u64> },
    Progress { id: usize, delta: u64 },
    Finished { id: usize },
    Failed { id: usize, err: String },
}

/// Main application state
struct App {
    rows: Vec<RowData>,
    selected: usize,
    mode: UiMode,
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
    let (tx, mut rx): (Sender<ProgressMsg>, Receiver<ProgressMsg>) = mpsc::channel(100);

    let mut app = App {
        rows: Vec::new(),
        selected: 0,
        mode: UiMode::Normal,
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
                }
                ProgressMsg::Failed { id, err } => {
                    if let Some(row) = app.rows.iter_mut().find(|r| r.id == id) {
                        row.status = "Error".into(); // short table label
                        row.detail = Some(err); // full message for Info popup
                    }
                }
            }
        }

        // --- input polling ---
        if event::poll(Duration::from_millis(100))? {
            if let Event::Key(key) = event::read()? {
                match (&mut app.mode, key.code) {
                    // Close modals with q or Esc (does not quit the app)
                    (UiMode::Input(_), KeyCode::Char('q')) | (UiMode::Input(_), KeyCode::Esc) => {
                        app.mode = UiMode::Normal;
                    }
                    (UiMode::Info(_), KeyCode::Char('q')) | (UiMode::Info(_), KeyCode::Esc) => {
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
                            spawn_download_task(id, url.clone(), tx.clone());
                            app.rows.push(RowData {
                                id,
                                url,
                                status: "Queued".into(),
                                done: 0,
                                total: None,
                                detail: None,
                            });
                        }
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
fn spawn_download_task(id: usize, url: String, tx: Sender<ProgressMsg>) {
    tokio::spawn(async move {
        let client = Client::builder()
            .user_agent("grab-cli")
            .build()
            .expect("http client");
        if let Err(e) = download_with_progress(&client, &url, id, tx.clone()).await {
            let _ = tx
                .send(ProgressMsg::Failed {
                    id,
                    err: e.to_string(),
                })
                .await;
        }
    });
}

/// Async download function that reports progress through channel
async fn download_with_progress(
    client: &Client,
    url: &str,
    id: usize,
    tx: Sender<ProgressMsg>,
) -> Result<()> {
    // 1) HEAD is best-effort; some servers reject or omit Content-Length
    let head_total = match client.head(url).send().await {
        Ok(resp) => resp
            .headers()
            .get(reqwest::header::CONTENT_LENGTH)
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.parse::<u64>().ok()),
        Err(_) => None,
    };
    // Notify UI with whatever we know so far
    let _ = tx
        .send(ProgressMsg::Started {
            id,
            total: head_total,
        })
        .await;

    // 2) GET the body
    let resp = client.get(url).send().await?.error_for_status()?;

    // 3) If GET reveals a Content-Length differing from HEAD, update UI
    let get_total = resp.content_length();
    if get_total.is_some() && get_total != head_total {
        let _ = tx
            .send(ProgressMsg::Started {
                id,
                total: get_total,
            })
            .await;
    }

    // 4) Stream body to file and emit progress deltas
    let mut file = File::create(filename_from_url(url)).await?;
    let mut stream = resp.bytes_stream();

    while let Some(item) = stream.next().await {
        let chunk = item?;
        file.write_all(&chunk).await?;
        let _ = tx
            .send(ProgressMsg::Progress {
                id,
                delta: chunk.len() as u64,
            })
            .await;
    }

    file.flush().await?;
    let _ = tx.send(ProgressMsg::Finished { id }).await;
    Ok(())
}

/// Draws the interface without a top header or downloads block.
/// Adds a bottom "Hints" bar. Columns: File | Status | Progress.
fn draw_ui(f: &mut Frame<'_>, app: &App) {
    let size = f.size();

    // Layout split: table + hints (no header)
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .margin(1)
        .constraints([Constraint::Min(0), Constraint::Length(3)])
        .split(size);

    // Build table rows
    let table_rows: Vec<Row> = app
        .rows
        .iter()
        .enumerate()
        .map(|(idx, r)| {
            let file_name = filename_from_url(&r.url);
            let prog_text = match r.total {
                Some(t) if t > 0 => {
                    let pct = r.done.saturating_mul(100) / t;
                    format!("{pct}%")
                }
                _ => human_bytes(r.done),
            };

            let base = Row::new(vec![
                Cell::from(file_name),
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
        Constraint::Length(12),
        Constraint::Length(10),
    ];
    let table = Table::new(table_rows, widths).header(
        Row::new(vec!["File", "Status", "Progress"]).style(Style::default().fg(Color::Yellow)),
    );
    f.render_widget(table, chunks[0]);

    // Bottom hints bar
    let hints_text = "a: Add URL   i: Info   q: Quit   ↑/↓: Select row   Enter/Esc: Modals";
    let hints =
        Paragraph::new(hints_text).block(Block::default().title("Hints").borders(Borders::ALL));
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
    let file_name = filename_from_url(&r.url);
    let prog_line = match (r.done, r.total) {
        (done, Some(t)) if t > 0 => {
            let pct = done.saturating_mul(100) / t;
            format!("Progress: {} / {} bytes ({}%)", done, t, pct)
        }
        (done, _) => format!("Progress: {} bytes (total unknown)", done),
    };

    if let Some(detail) = &r.detail {
        format!(
            "File: {}\nStatus: {}\n\nFull error:\n{}",
            file_name, r.status, detail
        )
    } else {
        format!("File: {}\nStatus: {}\n{}", file_name, r.status, prog_line)
    }
}

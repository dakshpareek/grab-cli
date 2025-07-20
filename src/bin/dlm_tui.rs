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
    widgets::{Block, Borders, Cell, Clear, Paragraph, Row, Table},
};
use reqwest::{header::ACCEPT_RANGES, Client};
use std::{io, time::Duration};
use tokio::{
    fs::{File, OpenOptions},
    io::AsyncWriteExt,
    sync::mpsc::{self, Receiver, Sender},
};

/// Row information shown in the table
#[derive(Clone)]
struct RowData {
    id: usize,
    url: String,
    status: String,
    done: u64,
    total: Option<u64>,
}

/// Application modes
enum UiMode {
    Normal,
    Input(String), // buffer while typing a URL
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
                    }
                }
                ProgressMsg::Failed { id, err } => {
                    if let Some(row) = app.rows.iter_mut().find(|r| r.id == id) {
                        row.status = format!("Err: {err}");
                    }
                }
            }
        }

        // --- input polling ---
        if event::poll(Duration::from_millis(100))? {
            if let Event::Key(key) = event::read()? {
                match (&mut app.mode, key.code) {
                    // Global quit
                    (_, KeyCode::Char('q')) => break,

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

                    // ----- INPUT MODE -----
                    (UiMode::Input(buf), KeyCode::Esc) => {
                        app.mode = UiMode::Normal;
                    }
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
        let client = Client::new();
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
    // HEAD to get total length and resumable flag
    let total = client
        .head(url)
        .send()
        .await?
        .headers()
        .get(reqwest::header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse::<u64>().ok());

    let _ = tx.send(ProgressMsg::Started { id, total }).await;

    let mut resp = client.get(url).send().await?.error_for_status()?;
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

/// Draws the entire interface for current state
fn draw_ui(f: &mut Frame<'_>, app: &App) {
    let size = f.size();

    // Layout split: header (3 rows) + table
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .margin(1)
        .constraints([Constraint::Length(3), Constraint::Min(0)])
        .split(size);

    // Header block
    let header = Block::default()
        .title("DLM  (q=quit, a=add url)")
        .borders(Borders::ALL);
    f.render_widget(header, chunks[0]);

    // Table rows
    let table_rows: Vec<Row> = app
        .rows
        .iter()
        .enumerate()
        .map(|(idx, r)| {
            let pct = r.total.map(|t| r.done * 100 / t).unwrap_or(0);
            let base = Row::new(vec![
                Cell::from(r.id.to_string()),
                Cell::from(r.url.as_str()),
                Cell::from(r.status.as_str()),
                Cell::from(format!("{pct}%")),
            ]);
            if idx == app.selected {
                base.style(Style::default().bg(Color::Blue))
            } else {
                base
            }
        })
        .collect();

    let widths = [
        Constraint::Length(4),
        Constraint::Percentage(50),
        Constraint::Length(12),
        Constraint::Length(8),
    ];
    let table = Table::new(table_rows, widths)
        .header(
            Row::new(vec!["ID", "URL", "Status", "Prog"]).style(Style::default().fg(Color::Yellow)),
        )
        .block(Block::default().borders(Borders::ALL).title("Downloads"));
    f.render_widget(table, chunks[1]);

    // Input prompt overlay
    if let UiMode::Input(ref buf) = app.mode {
        let area = centered_rect(60, 3, size);
        let prompt = Paragraph::new(buf.as_str())
            .block(Block::default().title("Add URL").borders(Borders::ALL));
        f.render_widget(Clear, area); // clear beneath
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

/// Simple helper reused from download module
fn filename_from_url(url: &str) -> String {
    url.split('/').last().unwrap_or("download.bin").to_string()
}

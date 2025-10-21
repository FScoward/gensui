mod app;
mod config;
mod log_parser;
mod state;
mod ui;
mod worker;

use std::io::{self, Stdout};
use std::process::Command;
use std::time::{Duration, Instant};

use anyhow::Result;
use crossterm::event::{self, Event, KeyEventKind};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;

use app::App;

fn main() -> Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    crossterm::execute!(stdout, crossterm::terminal::EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    let result = run_app(&mut terminal);
    disable_raw_mode()?;
    crossterm::execute!(
        terminal.backend_mut(),
        crossterm::terminal::LeaveAlternateScreen,
        crossterm::cursor::Show
    )?;
    result
}

fn run_app(terminal: &mut Terminal<CrosstermBackend<Stdout>>) -> Result<()> {
    let mut app = App::new()?;
    let mut last_tick = Instant::now();
    let tick_rate = Duration::from_millis(50);

    loop {
        // Check for interactive mode request
        if let Some(request) = app.pending_interactive_mode.take() {
            // Suspend TUI
            disable_raw_mode()?;
            crossterm::execute!(
                terminal.backend_mut(),
                crossterm::terminal::LeaveAlternateScreen,
                crossterm::cursor::Show
            )?;

            // Display info and wait for user
            println!("\n=== Interactive Claude Code Session ===");
            println!("Worker: {}", request.worker_name);
            println!("Worktree: {}", request.worktree_path.display());
            if request.session_id.is_some() {
                println!("Session: 継続（前回のセッションを引き継ぎます）");
            } else {
                println!("Session: 新規");
            }
            println!("\nPress Enter to start Claude Code CLI...");

            let mut input = String::new();
            io::stdin().read_line(&mut input)?;

            // Launch Claude Code CLI (non-headless mode)
            let mut cmd = Command::new("claude");
            cmd.current_dir(&request.worktree_path);

            // Add --continue flag if session exists
            if request.session_id.is_some() {
                cmd.arg("--continue");
            }

            let status = cmd.status();

            match status {
                Ok(exit_status) => {
                    println!("\nClaude Code exited with: {:?}", exit_status);
                }
                Err(err) => {
                    println!("\nFailed to launch Claude Code: {}", err);
                    println!("Make sure 'claude' command is available in your PATH");
                }
            }

            println!("\nPress Enter to return to TUI...");
            input.clear();
            io::stdin().read_line(&mut input)?;

            // Resume TUI
            enable_raw_mode()?;
            crossterm::execute!(
                terminal.backend_mut(),
                crossterm::terminal::EnterAlternateScreen
            )?;
            terminal.clear()?;

            // Add logs to notify the user
            app.push_log(format!(
                "インタラクティブセッションから復帰しました ({})",
                request.worker_name
            ));

            // Find the worker and add log entries
            if let Some(worker) = app.workers.iter_mut().find(|w| w.snapshot.name == request.worker_name) {
                worker.push_log("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━".to_string());
                worker.push_log("🎯 Interactive Claude Code Session 完了".to_string());
                worker.push_log("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━".to_string());
                worker.push_log("".to_string());
                worker.push_log("インタラクティブセッションでの変更が完了しました。".to_string());
                worker.push_log("".to_string());
                worker.push_log("📋 セッション履歴を確認するには:".to_string());
                worker.push_log("  1. このワーカーを選択".to_string());
                worker.push_log("  2. 's' キーを押す".to_string());
                worker.push_log("  3. セッション詳細を確認".to_string());
                worker.push_log("".to_string());

                // Check for file changes using git
                if let Ok(output) = std::process::Command::new("git")
                    .arg("status")
                    .arg("--short")
                    .current_dir(&request.worktree_path)
                    .output()
                {
                    let changes = String::from_utf8_lossy(&output.stdout);
                    if !changes.trim().is_empty() {
                        worker.push_log("📝 変更されたファイル:".to_string());
                        for line in changes.lines().take(10) {
                            worker.push_log(format!("  {}", line));
                        }
                        if changes.lines().count() > 10 {
                            worker.push_log(format!("  ... あと {} 件", changes.lines().count() - 10));
                        }
                        worker.push_log("".to_string());
                    }
                }
            }

            continue;
        }

        terminal.draw(|frame| app.render(frame))?;

        let timeout = tick_rate
            .checked_sub(last_tick.elapsed())
            .unwrap_or_else(|| Duration::from_secs(0));

        if event::poll(timeout)? {
            if let Event::Key(key_event) = event::read()? {
                if key_event.kind == KeyEventKind::Press {
                    if app.handle_key(key_event) {
                        break;
                    }
                }
            }
        }

        if last_tick.elapsed() >= tick_rate {
            app.on_tick();
            last_tick = Instant::now();
        }
    }

    Ok(())
}

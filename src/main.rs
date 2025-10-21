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
            println!("\nPress Enter to start Claude Code CLI...");

            let mut input = String::new();
            io::stdin().read_line(&mut input)?;

            // Launch Claude Code CLI (non-headless mode)
            let status = Command::new("claude")
                .current_dir(&request.worktree_path)
                .status();

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

            app.push_log(format!(
                "インタラクティブセッションから復帰しました ({})",
                request.worker_name
            ));

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

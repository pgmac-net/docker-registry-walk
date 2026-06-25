mod clipboard;
mod config;
mod ops;
mod registry;
mod tui;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let orig_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        // Restore terminal before printing panic.
        let _ = crossterm::terminal::disable_raw_mode();
        let _ = crossterm::execute!(
            std::io::stdout(),
            crossterm::terminal::LeaveAlternateScreen,
            crossterm::event::DisableMouseCapture,
        );
        orig_hook(info);
    }));

    let config = config::Config::load().unwrap_or_default();
    tui::run(config.registry).await
}

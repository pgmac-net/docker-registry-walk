mod clipboard;
mod config;
mod ops;
mod registry;
mod tui;

use clap::Parser;

use config::{Config, RegistryProfile};

#[derive(Parser)]
#[command(about = "Browse Docker registries from the terminal")]
struct Cli {
    /// Registry name from config to open on startup.
    #[arg(long)]
    registry: Option<String>,

    /// Ad-hoc registry URL (overrides config; creates a temporary "cli" profile).
    #[arg(long)]
    url: Option<String>,

    /// Username for the ad-hoc registry (used with --url).
    #[arg(long)]
    username: Option<String>,

    /// Password for the registry. Written to the OS keyring — never to the config file.
    #[arg(long)]
    password: Option<String>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let orig_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = crossterm::terminal::disable_raw_mode();
        let _ = crossterm::execute!(
            std::io::stdout(),
            crossterm::terminal::LeaveAlternateScreen,
            crossterm::event::DisableMouseCapture,
        );
        orig_hook(info);
    }));

    let cli = Cli::parse();
    let mut config = Config::load().unwrap_or_default();

    // Determine active profile index.
    let initial_idx = if let Some(url) = cli.url {
        // Ad-hoc profile from CLI — prepend so idx 0 is always the active one.
        let profile = RegistryProfile {
            name: "cli".to_owned(),
            url,
            username: cli.username.clone(),
            registry_type: Default::default(),
        };
        config.registry.insert(0, profile);
        0
    } else if let Some(name) = &cli.registry {
        config
            .registry
            .iter()
            .position(|r| &r.name == name)
            .unwrap_or_else(|| config.default_idx())
    } else {
        config.default_idx()
    };

    // Save password to keyring — never to config file.
    if let Some(password) = &cli.password {
        let profile_name = config
            .registry
            .get(initial_idx)
            .map(|p| p.name.as_str())
            .unwrap_or("cli");
        let service = format!("docker-registry-walk/{profile_name}");
        let username = config
            .registry
            .get(initial_idx)
            .and_then(|p| p.username.as_deref())
            .or(cli.username.as_deref())
            .unwrap_or("default");
        keyring::Entry::new(&service, username)?.set_password(password)?;
    }

    tui::run(config.registry, initial_idx).await
}

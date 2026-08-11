mod clipboard;
mod config;
mod ops;
mod registry;
mod tui;

use clap::Parser;

use config::{AuthMode, Config, RegistryProfile, RegistryType};

#[derive(Parser)]
#[command(
    about = "Browse Docker registries from the terminal",
    version,
    disable_version_flag = true
)]
struct Cli {
    /// Print version and exit.
    #[arg(short = 'v', long = "version", action = clap::ArgAction::Version)]
    version: (),

    /// Registry name from config to open on startup.
    #[arg(long)]
    registry: Option<String>,

    /// Ad-hoc registry URL (overrides config; creates a temporary "cli" profile).
    #[arg(long)]
    url: Option<String>,

    /// Username for the ad-hoc registry (used with --url).
    #[arg(long)]
    username: Option<String>,

    /// Registry flavour for the ad-hoc registry (used with --url).
    #[arg(long = "type", value_enum)]
    registry_type: Option<RegistryType>,

    /// How to authenticate the ad-hoc registry (used with --url).
    #[arg(long, value_enum)]
    auth: Option<AuthMode>,

    /// Prompt for the registry password (masked) and store it in the OS
    /// keyring. Takes no value — pass just `--password`, never
    /// `--password=<secret>`, so the secret never lands in shell history.
    #[arg(long)]
    password: bool,

    /// Prompt for a registry access token (masked) and store it in the OS
    /// keyring. Takes no value, for the same reason as `--password`.
    ///
    /// Alternatively, set $JFROG_ACCESS_TOKEN or $ARTIFACTORY_ACCESS_TOKEN,
    /// which take precedence over the keyring.
    #[arg(long)]
    token: bool,
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

    // Report a bad config rather than silently starting with none: the
    // fallback is an invented localhost profile, so an unreadable or invalid
    // config otherwise surfaces only as a confusing connection refused.
    let mut config = match Config::load() {
        Ok(config) => config,
        Err(e) => {
            eprintln!(
                "warning: ignoring {}: {e}",
                Config::default_path().display()
            );
            Config::default()
        }
    };

    // Determine active profile index.
    let initial_idx = if let Some(url) = cli.url {
        // Ad-hoc profile from CLI — prepend so idx 0 is always the active one.
        let profile = RegistryProfile {
            name: "cli".to_owned(),
            url,
            username: cli.username.clone(),
            registry_type: cli.registry_type.unwrap_or_default(),
            auth: cli.auth.unwrap_or_default(),
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

    let profile_name = config
        .registry
        .get(initial_idx)
        .map(|p| p.name.as_str())
        .unwrap_or("cli")
        .to_owned();

    // Prompt for the password (masked) and save it to the keyring — never
    // to the config file, and never passed as a CLI argument.
    if cli.password {
        let username = config
            .registry
            .get(initial_idx)
            .and_then(|p| p.username.as_deref())
            .or(cli.username.as_deref())
            .unwrap_or("default")
            .to_owned();
        let password = registry::prompt_password(&username)?;
        registry::KeyringStore::new(&profile_name).set_password(&username, &password)?;
    }

    // Same for an access token, stored under a fixed account name since a
    // token-authenticated profile has no username to key it by.
    if cli.token {
        let token = registry::sanitize_pasted_token(&registry::prompt_secret("Access token")?);
        if token.is_empty() {
            anyhow::bail!("no access token entered");
        }
        registry::KeyringStore::new(&profile_name).set_password(registry::TOKEN_ACCOUNT, &token)?;
    }

    tui::run(config.registry, initial_idx).await
}

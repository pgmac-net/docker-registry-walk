use crate::registry::{Manifest, RegistryClient};

pub struct InspectResult {
    pub manifest_json: String,
    pub config_json: Option<String>,
}

pub async fn inspect(
    client: &RegistryClient,
    repo: &str,
    tag: &str,
) -> anyhow::Result<InspectResult> {
    let resp = client.get_manifest(repo, tag).await?;

    let manifest_json = pretty_json(&resp.raw);

    let config_json = match &resp.manifest {
        Manifest::Image(img) => match client.get_blob(repo, &img.config.digest).await {
            Ok(bytes) => Some(pretty_json(&bytes)),
            Err(_) => None,
        },
        Manifest::Index(_) => None,
    };

    Ok(InspectResult {
        manifest_json,
        config_json,
    })
}

/// Pretty-print raw JSON bytes; fall back to lossy UTF-8 if parse fails.
pub fn pretty_json(raw: &[u8]) -> String {
    serde_json::from_slice::<serde_json::Value>(raw)
        .ok()
        .and_then(|v| serde_json::to_string_pretty(&v).ok())
        .unwrap_or_else(|| String::from_utf8_lossy(raw).into_owned())
}

/// Build display lines for the inspect overlay.
///
/// Returns lines in the form: manifest JSON, blank separator, config JSON (if present).
pub fn build_lines(result: &InspectResult) -> Vec<String> {
    let mut lines: Vec<String> = result.manifest_json.lines().map(str::to_owned).collect();

    if let Some(cfg) = &result.config_json {
        lines.push(String::new());
        lines.push("── config ──────────────────────────────────────────".to_owned());
        lines.push(String::new());
        lines.extend(cfg.lines().map(str::to_owned));
    }

    lines
}

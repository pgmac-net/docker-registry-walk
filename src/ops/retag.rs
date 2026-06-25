use crate::registry::{RegistryClient, Result};

/// Validate a tag name: alphanumeric, `-`, `_`, `.` only; no `/`.
pub fn validate_tag(tag: &str) -> bool {
    !tag.is_empty()
        && tag
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

/// Re-push the manifest for `src_tag` under `new_tag` in the same repo.
pub async fn retag(
    client: &RegistryClient,
    repo: &str,
    src_tag: &str,
    new_tag: &str,
) -> Result<()> {
    let resp = client.get_manifest(repo, src_tag).await?;
    client
        .put_manifest(repo, new_tag, resp.raw, &resp.content_type)
        .await
}

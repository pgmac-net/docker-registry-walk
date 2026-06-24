use crate::registry::{Manifest, RegistryClient, RegistryError};

/// Copy a tag from `src_repo:src_tag` to `dst_repo:dst_tag` within the same registry.
///
/// Blobs are mounted cross-repo where supported, falling back to GET+PUT.
/// The manifest is re-pushed verbatim so digest is preserved.
/// `on_progress(done, total)` is called after each blob.
pub async fn copy_image(
    client: &RegistryClient,
    src_repo: &str,
    src_tag: &str,
    dst_repo: &str,
    dst_tag: &str,
    on_progress: impl Fn(usize, usize),
) -> anyhow::Result<()> {
    let manifest_resp = client.get_manifest(src_repo, src_tag).await?;

    let blobs: Vec<String> = match &manifest_resp.manifest {
        Manifest::Image(img) => {
            let mut digests: Vec<String> = img.layers.iter().map(|l| l.digest.clone()).collect();
            digests.push(img.config.digest.clone());
            digests
        }
        // Index manifests reference child manifests by digest, not blobs directly.
        // Re-pushing the index manifest is sufficient if blobs are already present
        // via the child manifests (same registry cross-repo).
        Manifest::Index(_) => vec![],
    };

    let total = blobs.len();
    for (i, digest) in blobs.iter().enumerate() {
        on_progress(i, total);
        copy_blob(client, src_repo, dst_repo, digest).await?;
    }
    on_progress(total, total);

    client
        .put_manifest(
            dst_repo,
            dst_tag,
            manifest_resp.raw,
            &manifest_resp.content_type,
        )
        .await?;
    Ok(())
}

/// Parse `"<repo>:<tag>"` or `"<repo>"` (tag defaults to src_tag).
pub fn parse_destination<'a>(input: &'a str, src_tag: &'a str) -> (&'a str, &'a str) {
    match input.rsplit_once(':') {
        Some((repo, tag)) if !tag.is_empty() => (repo.trim(), tag),
        _ => (input.trim(), src_tag),
    }
}

async fn copy_blob(
    client: &RegistryClient,
    src_repo: &str,
    dst_repo: &str,
    digest: &str,
) -> anyhow::Result<()> {
    // Blob already present at destination?
    match client.head_blob(dst_repo, digest).await {
        Ok(_) => return Ok(()),
        Err(RegistryError::NotFound(_)) => {}
        Err(e) => return Err(e.into()),
    }

    // Try cross-repo mount (avoids data transfer when both repos share storage).
    if client.mount_blob(dst_repo, src_repo, digest).await? {
        return Ok(());
    }

    // Fall back to GET source → PUT destination.
    let data = client.get_blob(src_repo, digest).await?;
    let loc = client.start_blob_upload(dst_repo).await?;
    client
        .complete_blob_upload(&loc.location, digest, data)
        .await?;
    Ok(())
}

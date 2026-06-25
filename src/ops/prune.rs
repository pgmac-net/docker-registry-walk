use crate::registry::RegistryClient;

/// Return all tags in `repo` whose name is a bare sha256 digest
/// (`sha256:<64 hex chars>`).  These are manifests that were pushed
/// without a human-readable tag and represent the most common form of
/// "untagged" content in automated CI workflows.
pub async fn find_digest_tags(client: &RegistryClient, repo: &str) -> anyhow::Result<Vec<String>> {
    let tags = client.tags_all(repo).await?;
    Ok(tags.into_iter().filter(|t| is_digest_tag(t)).collect())
}

fn is_digest_tag(tag: &str) -> bool {
    if let Some(hex) = tag.strip_prefix("sha256:") {
        hex.len() == 64 && hex.chars().all(|c| c.is_ascii_hexdigit())
    } else {
        false
    }
}

/// Delete each digest-style tag by fetching its manifest digest and
/// issuing a DELETE.  Returns the number of manifests successfully deleted.
pub async fn prune_digest_tags(
    client: &RegistryClient,
    repo: &str,
    tags: &[String],
) -> anyhow::Result<usize> {
    let mut deleted = 0usize;
    for tag in tags {
        match client.get_manifest(repo, tag).await {
            Ok(resp) => {
                if client.delete_manifest(repo, &resp.digest).await.is_ok() {
                    deleted += 1;
                }
            }
            Err(_) => continue,
        }
    }
    Ok(deleted)
}

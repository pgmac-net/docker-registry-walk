use crate::registry::{Manifest, RegistryClient};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiffStatus {
    Added,
    Removed,
    Unchanged,
}

#[derive(Debug, Clone)]
pub struct DiffLayer {
    pub digest: String,
    pub size: u64,
    pub status: DiffStatus,
}

/// Compare the layer stacks of two tags in the same repository.
///
/// Layers present in `tag_b` but not `tag_a` → Added.
/// Layers present in `tag_a` but not `tag_b` → Removed.
/// Layers present in both                     → Unchanged.
///
/// The result is ordered: Unchanged and Added layers follow `tag_b`'s
/// order; Removed layers are appended at the end.
pub async fn diff_tags(
    client: &RegistryClient,
    repo: &str,
    tag_a: &str,
    tag_b: &str,
) -> anyhow::Result<Vec<DiffLayer>> {
    let resp_a = client.get_manifest(repo, tag_a).await?;
    let resp_b = client.get_manifest(repo, tag_b).await?;

    let layers_a = extract_layers(&resp_a.manifest)?;
    let layers_b = extract_layers(&resp_b.manifest)?;

    let digests_a: std::collections::HashSet<&str> =
        layers_a.iter().map(|(d, _)| d.as_str()).collect();
    let digests_b: std::collections::HashSet<&str> =
        layers_b.iter().map(|(d, _)| d.as_str()).collect();

    let mut result: Vec<DiffLayer> = layers_b
        .iter()
        .map(|(digest, size)| DiffLayer {
            digest: digest.clone(),
            size: *size,
            status: if digests_a.contains(digest.as_str()) {
                DiffStatus::Unchanged
            } else {
                DiffStatus::Added
            },
        })
        .collect();

    // Append layers from A that are not in B.
    for (digest, size) in &layers_a {
        if !digests_b.contains(digest.as_str()) {
            result.push(DiffLayer {
                digest: digest.clone(),
                size: *size,
                status: DiffStatus::Removed,
            });
        }
    }

    Ok(result)
}

fn extract_layers(manifest: &Manifest) -> anyhow::Result<Vec<(String, u64)>> {
    match manifest {
        Manifest::Image(img) => Ok(img
            .layers
            .iter()
            .map(|l| (l.digest.clone(), l.size as u64))
            .collect()),
        Manifest::Index(_) => Err(anyhow::anyhow!(
            "Multi-arch image index — select a specific platform tag to diff"
        )),
    }
}

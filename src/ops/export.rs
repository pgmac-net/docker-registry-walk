use std::io::Write;
use std::path::Path;

use tar::{Builder, Header};

use crate::registry::{Manifest, ManifestResponse, RegistryClient};

/// Export a single-arch image as an OCI image layout tarball.
///
/// The resulting file is compatible with `skopeo copy oci-archive:<path>`.
/// Multi-arch index manifests are not supported.
pub async fn export_image(
    client: &RegistryClient,
    repo: &str,
    tag: &str,
    dest: &Path,
    on_progress: impl Fn(usize, usize),
) -> anyhow::Result<()> {
    let manifest_resp = client.get_manifest(repo, tag).await?;

    let blob_digests = match &manifest_resp.manifest {
        Manifest::Image(img) => {
            let mut digests = vec![img.config.digest.clone()];
            digests.extend(img.layers.iter().map(|l| l.digest.clone()));
            digests
        }
        Manifest::Index(_) => {
            return Err(anyhow::anyhow!(
                "Multi-arch index — select a specific platform tag to export"
            ));
        }
    };

    let total_steps = blob_digests.len() + 1; // blobs + manifest

    let file = std::fs::File::create(dest)?;
    let mut tar = Builder::new(file);

    // oci-layout marker.
    write_entry(&mut tar, "oci-layout", br#"{"imageLayoutVersion":"1.0.0"}"#)?;

    // Blob entries.
    for (i, digest) in blob_digests.iter().enumerate() {
        let bytes = client.get_blob(repo, digest).await?;
        let hex = digest_hex(digest);
        write_entry(&mut tar, &format!("blobs/sha256/{hex}"), &bytes)?;
        on_progress(i + 1, total_steps);
    }

    // Manifest blob.
    let manifest_hex = digest_hex(&manifest_resp.digest);
    write_entry(
        &mut tar,
        &format!("blobs/sha256/{manifest_hex}"),
        &manifest_resp.raw,
    )?;
    on_progress(total_steps, total_steps);

    // index.json.
    let index = build_index_json(&manifest_resp, tag)?;
    write_entry(&mut tar, "index.json", index.as_bytes())?;

    tar.finish()?;
    Ok(())
}

fn write_entry<W: Write>(tar: &mut Builder<W>, path: &str, data: &[u8]) -> anyhow::Result<()> {
    let mut header = Header::new_gnu();
    header.set_size(data.len() as u64);
    header.set_mode(0o644);
    header.set_mtime(0);
    header.set_cksum();
    tar.append_data(&mut header, path, data)?;
    Ok(())
}

fn digest_hex(digest: &str) -> &str {
    digest.strip_prefix("sha256:").unwrap_or(digest)
}

fn build_index_json(resp: &ManifestResponse, tag: &str) -> anyhow::Result<String> {
    let index = serde_json::json!({
        "schemaVersion": 2,
        "mediaType": "application/vnd.oci.image.index.v1+json",
        "manifests": [{
            "mediaType": resp.content_type,
            "digest": resp.digest,
            "size": resp.raw.len(),
            "annotations": {
                "org.opencontainers.image.ref.name": tag,
            }
        }]
    });
    Ok(serde_json::to_string_pretty(&index)?)
}

use crate::registry::{RegistryClient, Result};

pub async fn delete_tag(client: &RegistryClient, repo: &str, tag: &str) -> Result<()> {
    let resp = client.get_manifest(repo, tag).await?;
    client.delete_manifest(repo, &resp.digest).await
}

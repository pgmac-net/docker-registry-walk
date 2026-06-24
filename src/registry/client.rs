#![allow(dead_code)]

use std::sync::Arc;

use bytes::Bytes;
use reqwest::{Client, Response, StatusCode};
use url::Url;

use crate::registry::{
    error::{RegistryError, Result},
    pagination::parse_next_link,
    types::{
        BlobInfo, Catalog, ImageManifest, Manifest, ManifestIndex, ManifestResponse, TagList,
        UploadLocation, MANIFEST_ACCEPT,
    },
};

/// Provides an `Authorization` header value.  Implement this trait in the auth
/// module (PGM-268); use `NoCredentials` until then.
pub trait Credentials: Send + Sync {
    fn get_authorization(&self) -> Option<String>;
}

/// No-op credentials — suitable for public registries or until PGM-268 lands.
pub struct NoCredentials;

impl Credentials for NoCredentials {
    fn get_authorization(&self) -> Option<String> {
        None
    }
}

/// Async, cheaply-cloneable Docker Registry v2 client.
#[derive(Clone)]
pub struct RegistryClient {
    http: Client,
    base_url: Url,
    creds: Arc<dyn Credentials>,
}

impl RegistryClient {
    pub fn new(base_url: Url) -> Self {
        Self {
            http: Client::new(),
            base_url,
            creds: Arc::new(NoCredentials),
        }
    }

    pub fn with_credentials(mut self, creds: Arc<dyn Credentials>) -> Self {
        self.creds = creds;
        self
    }

    // ------------------------------------------------------------------
    // Internal helpers
    // ------------------------------------------------------------------

    fn url(&self, path: &str) -> Result<Url> {
        self.base_url.join(path).map_err(RegistryError::InvalidUrl)
    }

    async fn send(&self, builder: reqwest::RequestBuilder) -> Result<Response> {
        let builder = match self.creds.get_authorization() {
            Some(auth) => builder.header(reqwest::header::AUTHORIZATION, auth),
            None => builder,
        };
        Ok(builder.send().await?)
    }

    // ------------------------------------------------------------------
    // Registry API
    // ------------------------------------------------------------------

    /// `GET /v2/` — connectivity and auth probe.
    pub async fn ping(&self) -> Result<()> {
        let url = self.url("/v2/")?;
        let resp = self.send(self.http.get(url.clone())).await?;
        match resp.status() {
            StatusCode::OK | StatusCode::UNAUTHORIZED => Ok(()),
            s => Err(RegistryError::UnexpectedStatus {
                status: s.as_u16(),
                url: url.to_string(),
            }),
        }
    }

    /// `GET /v2/_catalog` — one page of the repository list.
    ///
    /// Returns `(repositories, has_more)`.  Pass the last repo name as `last`
    /// for subsequent pages.
    pub async fn catalog_page(
        &self,
        page_size: u32,
        last: Option<&str>,
    ) -> Result<(Catalog, bool)> {
        let mut url = self.url("/v2/_catalog")?;
        {
            let mut q = url.query_pairs_mut();
            q.append_pair("n", &page_size.to_string());
            if let Some(last) = last {
                q.append_pair("last", last);
            }
        }

        let resp = self.send(self.http.get(url.clone())).await?;
        require_success(&resp, &url)?;

        let has_more = resp
            .headers()
            .get("link")
            .and_then(|v| v.to_str().ok())
            .and_then(parse_next_link)
            .is_some();

        let catalog = resp.json::<Catalog>().await?;
        Ok((catalog, has_more))
    }

    /// Fetch the complete repository list, following pagination automatically.
    pub async fn catalog_all(&self) -> Result<Vec<String>> {
        const PAGE: u32 = 100;
        let mut all = Vec::new();
        let mut last: Option<String> = None;

        loop {
            let (page, has_more) = self.catalog_page(PAGE, last.as_deref()).await?;
            last = page.repositories.last().cloned();
            all.extend(page.repositories);
            if !has_more {
                break;
            }
        }
        Ok(all)
    }

    /// `GET /v2/<name>/tags/list` — one page of tags for a repository.
    pub async fn tags_page(
        &self,
        repo: &str,
        page_size: u32,
        last: Option<&str>,
    ) -> Result<(TagList, bool)> {
        let path = format!("/v2/{repo}/tags/list");
        let mut url = self.url(&path)?;
        {
            let mut q = url.query_pairs_mut();
            q.append_pair("n", &page_size.to_string());
            if let Some(last) = last {
                q.append_pair("last", last);
            }
        }

        let resp = self.send(self.http.get(url.clone())).await?;
        require_success(&resp, &url)?;

        let has_more = resp
            .headers()
            .get("link")
            .and_then(|v| v.to_str().ok())
            .and_then(parse_next_link)
            .is_some();

        let tag_list = resp.json::<TagList>().await?;
        Ok((tag_list, has_more))
    }

    /// Fetch all tags for a repository, following pagination automatically.
    pub async fn tags_all(&self, repo: &str) -> Result<Vec<String>> {
        const PAGE: u32 = 100;
        let mut all = Vec::new();
        let mut last: Option<String> = None;

        loop {
            let (page, has_more) = self.tags_page(repo, PAGE, last.as_deref()).await?;
            last = page.tags.last().cloned();
            all.extend(page.tags);
            if !has_more {
                break;
            }
        }
        Ok(all)
    }

    /// `GET /v2/<name>/manifests/<reference>` — fetch a manifest by tag or digest.
    pub async fn get_manifest(&self, repo: &str, reference: &str) -> Result<ManifestResponse> {
        let path = format!("/v2/{repo}/manifests/{reference}");
        let url = self.url(&path)?;

        let resp = self
            .send(
                self.http
                    .get(url.clone())
                    .header(reqwest::header::ACCEPT, MANIFEST_ACCEPT),
            )
            .await?;
        require_success(&resp, &url)?;

        let content_type = resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_owned();

        let digest = resp
            .headers()
            .get("docker-content-digest")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_owned();

        let raw = resp.bytes().await?.to_vec();

        let manifest = parse_manifest(&content_type, &raw)?;

        Ok(ManifestResponse {
            manifest,
            digest,
            content_type,
            raw,
        })
    }

    /// `PUT /v2/<name>/manifests/<reference>` — push a manifest.
    pub async fn put_manifest(
        &self,
        repo: &str,
        reference: &str,
        body: Vec<u8>,
        content_type: &str,
    ) -> Result<()> {
        let path = format!("/v2/{repo}/manifests/{reference}");
        let url = self.url(&path)?;

        let resp = self
            .send(
                self.http
                    .put(url.clone())
                    .header(reqwest::header::CONTENT_TYPE, content_type)
                    .body(body),
            )
            .await?;
        require_success(&resp, &url)
    }

    /// `DELETE /v2/<name>/manifests/<digest>` — delete a manifest by digest.
    ///
    /// The registry must have delete enabled (`REGISTRY_STORAGE_DELETE_ENABLED=true`).
    pub async fn delete_manifest(&self, repo: &str, digest: &str) -> Result<()> {
        let path = format!("/v2/{repo}/manifests/{digest}");
        let url = self.url(&path)?;
        let resp = self.send(self.http.delete(url.clone())).await?;
        require_success(&resp, &url)
    }

    /// `HEAD /v2/<name>/blobs/<digest>` — check blob existence and get size.
    pub async fn head_blob(&self, repo: &str, digest: &str) -> Result<BlobInfo> {
        let path = format!("/v2/{repo}/blobs/{digest}");
        let url = self.url(&path)?;

        let resp = self.send(self.http.head(url.clone())).await?;
        require_success(&resp, &url)?;

        let size = resp
            .headers()
            .get(reqwest::header::CONTENT_LENGTH)
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(0);

        let actual_digest = resp
            .headers()
            .get("docker-content-digest")
            .and_then(|v| v.to_str().ok())
            .unwrap_or(digest)
            .to_owned();

        Ok(BlobInfo {
            digest: actual_digest,
            size,
        })
    }

    /// `GET /v2/<name>/blobs/<digest>` — fetch a blob as raw bytes.
    ///
    /// For large blobs, prefer `get_blob_stream` (not yet implemented).
    pub async fn get_blob(&self, repo: &str, digest: &str) -> Result<Bytes> {
        let path = format!("/v2/{repo}/blobs/{digest}");
        let url = self.url(&path)?;

        let resp = self.send(self.http.get(url.clone())).await?;
        require_success(&resp, &url)?;

        Ok(resp.bytes().await?)
    }

    /// `POST /v2/<name>/blobs/uploads/` — initiate a blob upload.
    ///
    /// Returns the upload `Location` URL to use for the subsequent PUT.
    pub async fn start_blob_upload(&self, repo: &str) -> Result<UploadLocation> {
        let path = format!("/v2/{repo}/blobs/uploads/");
        let url = self.url(&path)?;

        let resp = self.send(self.http.post(url.clone())).await?;

        if resp.status() != StatusCode::ACCEPTED {
            return Err(RegistryError::UnexpectedStatus {
                status: resp.status().as_u16(),
                url: url.to_string(),
            });
        }

        let location = resp
            .headers()
            .get(reqwest::header::LOCATION)
            .and_then(|v| v.to_str().ok())
            .ok_or_else(|| RegistryError::InvalidResponse("missing Location header".into()))?;

        let location = if location.starts_with("http") {
            Url::parse(location)?
        } else {
            self.base_url.join(location)?
        };

        Ok(UploadLocation { location })
    }

    /// Complete a blob upload with a monolithic PUT.
    ///
    /// `upload_url` is the `Location` from `start_blob_upload`.
    pub async fn complete_blob_upload(
        &self,
        upload_url: &Url,
        digest: &str,
        data: Bytes,
    ) -> Result<()> {
        let mut url = upload_url.clone();
        url.query_pairs_mut().append_pair("digest", digest);

        let resp = self
            .send(
                self.http
                    .put(url.clone())
                    .header(
                        reqwest::header::CONTENT_TYPE,
                        "application/octet-stream",
                    )
                    .body(data),
            )
            .await?;

        if resp.status() != StatusCode::CREATED {
            return Err(RegistryError::UnexpectedStatus {
                status: resp.status().as_u16(),
                url: url.to_string(),
            });
        }
        Ok(())
    }
}

// ------------------------------------------------------------------
// Internal helpers
// ------------------------------------------------------------------

fn require_success(resp: &Response, url: &Url) -> Result<()> {
    match resp.status() {
        s if s.is_success() => Ok(()),
        StatusCode::UNAUTHORIZED => Err(RegistryError::Unauthorized),
        StatusCode::NOT_FOUND => Err(RegistryError::NotFound(url.to_string())),
        s => Err(RegistryError::UnexpectedStatus {
            status: s.as_u16(),
            url: url.to_string(),
        }),
    }
}

fn parse_manifest(content_type: &str, raw: &[u8]) -> Result<Manifest> {
    let ct = content_type.split(';').next().unwrap_or("").trim();
    match ct {
        crate::registry::types::media_types::OCI_INDEX
        | crate::registry::types::media_types::DOCKER_MANIFEST_LIST => {
            let index = serde_json::from_slice::<ManifestIndex>(raw).map_err(|e| {
                RegistryError::InvalidResponse(format!("manifest index parse error: {e}"))
            })?;
            Ok(Manifest::Index(index))
        }
        _ => {
            let manifest = serde_json::from_slice::<ImageManifest>(raw).map_err(|e| {
                RegistryError::InvalidResponse(format!("manifest parse error: {e}"))
            })?;
            Ok(Manifest::Image(manifest))
        }
    }
}

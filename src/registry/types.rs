#![allow(dead_code)]

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

pub mod media_types {
    pub const OCI_MANIFEST: &str = "application/vnd.oci.image.manifest.v1+json";
    pub const OCI_INDEX: &str = "application/vnd.oci.image.index.v1+json";
    pub const DOCKER_MANIFEST_V2: &str = "application/vnd.docker.distribution.manifest.v2+json";
    pub const DOCKER_MANIFEST_LIST: &str =
        "application/vnd.docker.distribution.manifest.list.v2+json";
}

/// Ordered list of Accept values to send when fetching manifests.
pub const MANIFEST_ACCEPT: &str = concat!(
    "application/vnd.oci.image.manifest.v1+json,",
    "application/vnd.oci.image.index.v1+json,",
    "application/vnd.docker.distribution.manifest.v2+json,",
    "application/vnd.docker.distribution.manifest.list.v2+json",
);

#[derive(Debug, Clone, Deserialize)]
pub struct Catalog {
    pub repositories: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TagList {
    pub name: String,
    /// Some registries return `null` for repos with no tags.
    #[serde(default)]
    pub tags: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Platform {
    pub os: String,
    pub architecture: String,
    #[serde(default)]
    pub variant: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ManifestDescriptor {
    pub media_type: String,
    pub size: i64,
    pub digest: String,
    #[serde(default)]
    pub platform: Option<Platform>,
}

/// Parsed image config blob (Docker v2 / OCI).
#[derive(Debug, Clone, Deserialize, Default)]
pub struct ImageConfigBlob {
    #[serde(default)]
    pub architecture: Option<String>,
    #[serde(default)]
    pub os: Option<String>,
    #[serde(default)]
    pub created: Option<String>,
    #[serde(default)]
    pub config: ImageConfigSection,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct ImageConfigSection {
    #[serde(rename = "Labels", default)]
    pub labels: Option<HashMap<String, String>>,
}

/// Single-arch image manifest (OCI v1 or Docker v2 schema 2).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImageManifest {
    pub schema_version: u8,
    #[serde(default)]
    pub media_type: String,
    pub config: ManifestDescriptor,
    pub layers: Vec<ManifestDescriptor>,
}

/// Multi-arch manifest index (OCI) / manifest list (Docker).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ManifestIndex {
    pub schema_version: u8,
    #[serde(default)]
    pub media_type: String,
    pub manifests: Vec<ManifestDescriptor>,
}

/// Discriminated manifest payload returned by the registry.
#[derive(Debug, Clone)]
pub enum Manifest {
    Image(ImageManifest),
    Index(ManifestIndex),
}

/// Full response from a manifest fetch, including raw bytes for re-push.
#[derive(Debug, Clone)]
pub struct ManifestResponse {
    pub manifest: Manifest,
    pub digest: String,
    pub content_type: String,
    pub raw: Vec<u8>,
}

/// Metadata returned by a blob HEAD request.
#[derive(Debug, Clone)]
pub struct BlobInfo {
    pub digest: String,
    pub size: u64,
}

/// Location returned after initiating a blob upload (POST).
#[derive(Debug, Clone)]
pub struct UploadLocation {
    pub location: url::Url,
}

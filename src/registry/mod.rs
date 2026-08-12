// Docker Registry HTTP API v2 client
#![allow(unused_imports)]

mod auth;
mod client;
mod error;
mod pagination;
mod search;
mod types;

pub use auth::{
    AccessTokenCredentials, BasicCredentials, BearerCredentials, KeyringStore, Secret,
    TOKEN_ACCOUNT, prompt_password, prompt_secret, resolve_access_token, resolve_secret,
    sanitize_pasted_token,
};
pub use client::{Credentials, NoCredentials, RegistryClient};
pub use error::{RegistryError, Result};
pub use search::search_dockerhub;
pub use types::{
    ArtifactoryRepo, BlobInfo, Catalog, ImageConfigBlob, ImageManifest, Manifest,
    ManifestDescriptor, ManifestIndex, ManifestResponse, Platform, TagList, UploadLocation,
    media_types,
};

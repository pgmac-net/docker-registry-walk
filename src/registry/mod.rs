// Docker Registry HTTP API v2 client
#![allow(unused_imports)]

mod auth;
mod client;
mod error;
mod pagination;
mod search;
mod types;

pub use auth::{BasicCredentials, BearerCredentials, KeyringStore, resolve_password};
pub use search::{is_dockerhub_url, search_dockerhub};
pub use client::{Credentials, NoCredentials, RegistryClient};
pub use error::{RegistryError, Result};
pub use types::{
    BlobInfo, Catalog, ImageConfigBlob, ImageManifest, Manifest, ManifestDescriptor, ManifestIndex,
    ManifestResponse, Platform, TagList, UploadLocation, media_types,
};

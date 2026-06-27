// Docker Registry HTTP API v2 client
#![allow(unused_imports)]

mod auth;
mod client;
mod error;
mod pagination;
mod search;
mod types;

pub use auth::{BasicCredentials, BearerCredentials, KeyringStore, resolve_password};
pub use client::{Credentials, NoCredentials, RegistryClient};
pub use error::{RegistryError, Result};
pub use search::search_dockerhub;
pub use types::{
    BlobInfo, Catalog, ImageConfigBlob, ImageManifest, Manifest, ManifestDescriptor, ManifestIndex,
    ManifestResponse, Platform, TagList, UploadLocation, media_types,
};

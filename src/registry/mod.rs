// Docker Registry HTTP API v2 client
#![allow(unused_imports)]

mod client;
mod error;
mod pagination;
mod types;

pub use client::{Credentials, NoCredentials, RegistryClient};
pub use error::{RegistryError, Result};
pub use types::{
    BlobInfo, Catalog, ImageManifest, Manifest, ManifestDescriptor, ManifestIndex,
    ManifestResponse, TagList, UploadLocation, media_types,
};

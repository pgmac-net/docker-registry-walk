#![allow(dead_code)]

use thiserror::Error;

#[derive(Debug, Error)]
pub enum RegistryError {
    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),

    #[error("registry returned {status} for {url}")]
    UnexpectedStatus { status: u16, url: String },

    #[error("unauthorized — authentication required")]
    Unauthorized,

    #[error("not found: {0}")]
    NotFound(String),

    #[error("invalid response: {0}")]
    InvalidResponse(String),

    #[error("invalid URL: {0}")]
    InvalidUrl(#[from] url::ParseError),
}

pub type Result<T> = std::result::Result<T, RegistryError>;

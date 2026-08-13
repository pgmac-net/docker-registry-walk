//! GHCR repository discovery via the GitHub packages API.
//!
//! GHCR implements no `/v2/_catalog`, so the only way to enumerate
//! repositories is GitHub's own API — which lives on a **different host**
//! (`api.github.com`) from the registry (`ghcr.io`).
//!
//! That is why this is a free function over its own `reqwest` call rather than
//! a `RegistryClient` method, mirroring `search.rs`. `RegistryClient::send`
//! deliberately strips `Authorization` from any request that is not same-origin
//! with the client's `base_url`, to keep the credential off server-supplied
//! URLs (see the blob-upload `Location` case). Reaching into the client's
//! credentials to talk to a second host would carve an exception into exactly
//! that guard. Sending the PAT to `api.github.com` is intended, so it is done
//! openly, here, with the token passed in.

use serde::Deserialize;

use crate::registry::pagination::parse_next_link;

/// Sent on every GitHub API request — GitHub rejects requests without one.
const USER_AGENT: &str = concat!("docker-registry-walk/", env!("CARGO_PKG_VERSION"));

/// Pinned so a future default-version bump cannot silently reshape the
/// response this module parses.
const API_VERSION: &str = "2022-11-28";

/// Upper bound on pages followed, so a very large namespace cannot hang the
/// picker indefinitely. At 100 per page this is 5,000 packages; Homebrew, the
/// largest namespace encountered while testing, has roughly twice that. The
/// caller is told when the cap was hit rather than being handed a silently
/// short list.
const MAX_PAGES: usize = 50;

const PER_PAGE: u32 = 100;

#[derive(Deserialize)]
struct Package {
    name: String,
    owner: PackageOwner,
}

#[derive(Deserialize)]
struct PackageOwner {
    login: String,
}

/// The outcome of a package listing.
pub struct PackageList {
    /// Repository names, in `owner/name` form, ready to use as a `/v2/` path.
    pub repos: Vec<String>,
    /// Whether [`MAX_PAGES`] cut the listing short.
    pub truncated: bool,
}

/// First page of the packages listing for `owner`, or for the token holder
/// when `owner` is `None`.
///
/// Kept separate from the request so it can be asserted on without a network.
fn first_page_url(owner: Option<&str>) -> String {
    let base = match owner {
        Some(owner) => format!("https://api.github.com/users/{owner}/packages"),
        // The token holder's own packages. GitHub has no "whoami" form of the
        // /users/ path, so this is a genuinely different endpoint.
        None => "https://api.github.com/user/packages".to_owned(),
    };
    format!("{base}?package_type=container&per_page={PER_PAGE}")
}

/// The GHCR repository path for a package.
///
/// GHCR paths are lowercase, but the API echoes the owner's login with its
/// original casing (`Homebrew`), and package names are themselves already
/// nested (`core/sqldiff`) — so this is `homebrew/core/sqldiff`, not a flat
/// join of two simple identifiers.
///
/// The login is taken from the payload rather than from the configured
/// `owner`, so `/user/packages` works without the caller knowing whose token
/// it holds.
fn repo_name(pkg: &Package) -> String {
    format!("{}/{}", pkg.owner.login.to_lowercase(), pkg.name)
}

/// List container packages visible to `token`, following `Link` pagination.
///
/// Requires a PAT with `read:packages`; GitHub exposes no anonymous package
/// listing, even for public packages.
pub async fn list_packages(owner: Option<&str>, token: &str) -> anyhow::Result<PackageList> {
    let http = reqwest::Client::new();
    let mut next = Some(first_page_url(owner));
    let mut repos = Vec::new();
    let mut pages = 0usize;

    while let Some(url) = next.take() {
        if pages >= MAX_PAGES {
            return Ok(PackageList {
                repos,
                truncated: true,
            });
        }
        pages += 1;

        let resp = http
            .get(&url)
            .header(reqwest::header::USER_AGENT, USER_AGENT)
            .header(reqwest::header::ACCEPT, "application/vnd.github+json")
            .header("X-GitHub-Api-Version", API_VERSION)
            .bearer_auth(token)
            .send()
            .await?
            .error_for_status()?;

        // Read before consuming the body.
        next = resp
            .headers()
            .get(reqwest::header::LINK)
            .and_then(|v| v.to_str().ok())
            .and_then(parse_next_link);

        let page: Vec<Package> = resp.json().await?;
        repos.extend(page.iter().map(repo_name));
    }

    Ok(PackageList {
        repos,
        truncated: false,
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn pkg(login: &str, name: &str) -> Package {
        Package {
            name: name.to_owned(),
            owner: PackageOwner {
                login: login.to_owned(),
            },
        }
    }

    #[test]
    fn first_page_url_uses_users_endpoint_for_named_owner() {
        assert_eq!(
            first_page_url(Some("pgmac-net")),
            "https://api.github.com/users/pgmac-net/packages?package_type=container&per_page=100"
        );
    }

    #[test]
    fn first_page_url_uses_self_endpoint_without_owner() {
        assert_eq!(
            first_page_url(None),
            "https://api.github.com/user/packages?package_type=container&per_page=100"
        );
    }

    /// Both quirks come from the real Homebrew payload: the owner login is
    /// capitalised while GHCR paths are lowercase, and package names are
    /// already nested, so `owner/name` is a three-segment path.
    #[test]
    fn repo_name_lowercases_owner_and_keeps_nested_package_name() {
        assert_eq!(
            repo_name(&pkg("Homebrew", "core/sqldiff")),
            "homebrew/core/sqldiff"
        );
        assert_eq!(repo_name(&pkg("Homebrew", "brew")), "homebrew/brew");
    }

    #[test]
    fn repo_name_leaves_already_lowercase_owner_alone() {
        assert_eq!(repo_name(&pkg("pgmac-net", "walker")), "pgmac-net/walker");
    }

    /// The listing loop stops when GitHub omits a `rel="next"` link, which is
    /// what terminates pagination — a page whose `Link` carries only `prev`
    /// and `last` must not be read as "keep going".
    #[test]
    fn pagination_terminates_without_a_next_link() {
        let last_page = r#"<https://api.github.com/user/1/packages?page=1>; rel="prev", <https://api.github.com/user/1/packages?page=9>; rel="last""#;
        assert_eq!(parse_next_link(last_page), None);

        let more = r#"<https://api.github.com/user/1/packages?page=2>; rel="next", <https://api.github.com/user/1/packages?page=9>; rel="last""#;
        assert_eq!(
            parse_next_link(more),
            Some("https://api.github.com/user/1/packages?page=2".to_owned())
        );
    }
}

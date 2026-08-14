//! AWS ECR discovery and authorization.
//!
//! ECR has no `/v2/_catalog`, so the repository list comes from
//! `ecr:DescribeRepositories` — an AWS API served from `api.ecr.<region>.amazonaws.com`,
//! a *different origin* from the `<account>.dkr.ecr.<region>.amazonaws.com`
//! registry itself. `RegistryClient::send` deliberately strips `Authorization`
//! from any request that is not same-origin with its base URL, which is exactly
//! the guard that keeps credentials off server-supplied blob URLs; routing an
//! AWS call through the client would mean carving an exception into it.
//!
//! So, like `registry::ghcr`, this module is free functions with their own
//! transport — here the AWS SDK, which brings its own signing, retry and
//! credential chain (SSO, assume-role, static keys, IMDS). Nothing in here
//! reads the keyring: ECR's registry password is minted, not stored.
//!
//! The split is the usual one for this codebase: every decision that can be
//! made without the network lives in a small pure function with unit tests,
//! and the SDK glue below stays thin enough to review by eye.

use std::time::{Duration, SystemTime};

use anyhow::{Context, Result};
use aws_config::BehaviorVersion;
use aws_sdk_ecr::config::Region;

/// Region the `ecr-public` API answers in, regardless of where its images are
/// served from. ECR Public is one global registry, not one per region.
pub const ECR_PUBLIC_API_REGION: &str = "us-east-1";

/// Fixed registry endpoint for ECR Public.
pub const ECR_PUBLIC_REGISTRY_URL: &str = "https://public.ecr.aws";

/// Which AWS identity and region to talk to.
///
/// Both fields are optional and mean "defer to the AWS chain" when unset —
/// `$AWS_PROFILE` / `$AWS_REGION`, then the named profile's own configuration.
/// They are carried explicitly rather than read from the environment once at
/// startup because the TUI lets the user switch either one at runtime.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EcrTarget {
    pub aws_profile: Option<String>,
    pub region: Option<String>,
}

impl EcrTarget {
    pub fn new(aws_profile: Option<String>, region: Option<String>) -> Self {
        Self {
            aws_profile,
            region,
        }
    }
}

/// A registry password minted by AWS, and when it stops working.
#[derive(Clone, Debug)]
pub struct EcrAuthorization {
    /// The registry this token is for, from `proxyEndpoint`.
    pub registry_url: String,
    /// Base64 of `AWS:<password>` — already exactly the value an
    /// `Authorization: Basic` header takes, so it is never decoded here.
    pub authorization_token: String,
    /// Absolute expiry. `None` when AWS did not say, which is treated as
    /// "refresh on every use" rather than "never expires".
    pub expires_at: Option<SystemTime>,
}

/// Everything one connect needs: where the registry is, how to authenticate to
/// it, and what it contains.
///
/// Bundled because both halves come from the same SDK client and the TUI wants
/// them in the same frame — resolving the endpoint without the repository list
/// would leave an empty pane behind a connected header.
#[derive(Clone, Debug)]
pub struct EcrConnection {
    pub authorization: EcrAuthorization,
    pub repos: Vec<String>,
}

/// Margin applied to `expires_at` when deciding whether a cached token is still
/// good.
///
/// ECR tokens last ~12 hours, so this is not about cutting it fine; it is about
/// never handing a request a token that expires while it is in flight.
const EXPIRY_SKEW: Duration = Duration::from_secs(300);

impl EcrAuthorization {
    /// Whether this token can still be used, `now` being passed in so the rule
    /// is testable without waiting 12 hours.
    pub fn is_valid_at(&self, now: SystemTime) -> bool {
        match self.expires_at {
            Some(expiry) => expiry
                .checked_sub(EXPIRY_SKEW)
                .is_some_and(|deadline| now < deadline),
            // No stated expiry: assume the worst and mint a fresh one.
            None => false,
        }
    }

    /// The value for an `Authorization` header.
    pub fn header_value(&self) -> String {
        format!("Basic {}", self.authorization_token)
    }
}

// ---------------------------------------------------------------------------
// Pure helpers
// ---------------------------------------------------------------------------

/// AWS region codes offered as suggestions in the region picker.
///
/// A static list rather than an API call: enumerating regions needs
/// `ec2:DescribeRegions`, a permission unrelated to reading a registry, and the
/// picker always accepts a typed value anyway — so a region missing from this
/// list costs nothing but a few keystrokes.
pub const AWS_REGIONS: &[&str] = &[
    "af-south-1",
    "ap-east-1",
    "ap-northeast-1",
    "ap-northeast-2",
    "ap-northeast-3",
    "ap-south-1",
    "ap-south-2",
    "ap-southeast-1",
    "ap-southeast-2",
    "ap-southeast-3",
    "ap-southeast-4",
    "ca-central-1",
    "ca-west-1",
    "eu-central-1",
    "eu-central-2",
    "eu-north-1",
    "eu-south-1",
    "eu-south-2",
    "eu-west-1",
    "eu-west-2",
    "eu-west-3",
    "il-central-1",
    "me-central-1",
    "me-south-1",
    "sa-east-1",
    "us-east-1",
    "us-east-2",
    "us-west-1",
    "us-west-2",
];

/// An AWS named profile as found in the user's AWS config files.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AwsProfile {
    pub name: String,
    /// The profile's own `region`, used to preselect the region picker.
    pub region: Option<String>,
}

/// Parse the profile names (and their regions) out of the AWS config files.
///
/// Hand-rolled rather than reaching into `aws_config`'s profile loader: that
/// API is `pub` but shaped for the SDK's own use, and all this needs is a list
/// of names to *suggest*. Being wrong here is not a correctness problem — the
/// picker always accepts a typed profile name — so a small readable parser is
/// the better trade than a coupling to SDK internals.
///
/// The two files spell sections differently: `~/.aws/config` uses
/// `[profile foo]` (with a bare `[default]`), `~/.aws/credentials` uses `[foo]`.
/// Names are returned in first-seen order with `default` hoisted to the front,
/// de-duplicated case-sensitively, since AWS profile names are case-sensitive.
pub fn parse_aws_profiles(config_text: &str, credentials_text: &str) -> Vec<AwsProfile> {
    let mut found: Vec<AwsProfile> = Vec::new();

    // `config` first so a region defined there wins: `credentials` conventionally
    // holds only keys.
    for (text, strip_prefix) in [(config_text, true), (credentials_text, false)] {
        let mut current: Option<usize> = None;

        for line in text.lines() {
            let line = line.trim();
            if line.starts_with('#') || line.starts_with(';') {
                continue;
            }

            if let Some(section) = line
                .strip_prefix('[')
                .and_then(|rest| rest.strip_suffix(']'))
            {
                let name = section.trim();
                // `[profile foo]` in config; `[default]` stays bare in both.
                let name = if strip_prefix {
                    name.strip_prefix("profile ").unwrap_or(name).trim()
                } else {
                    name
                };

                if name.is_empty() {
                    current = None;
                    continue;
                }

                current = Some(match found.iter().position(|p| p.name == name) {
                    Some(existing) => existing,
                    None => {
                        found.push(AwsProfile {
                            name: name.to_owned(),
                            region: None,
                        });
                        found.len() - 1
                    }
                });
                continue;
            }

            let Some(idx) = current else { continue };
            if let Some(value) = line.strip_prefix("region") {
                let value = value.trim_start();
                if let Some(region) = value.strip_prefix('=') {
                    let region = region.trim();
                    if !region.is_empty() && found[idx].region.is_none() {
                        found[idx].region = Some(region.to_owned());
                    }
                }
            }
        }
    }

    // `default` is the profile most sessions want, so it leads regardless of
    // where it appears in the file.
    if let Some(pos) = found.iter().position(|p| p.name == "default")
        && pos != 0
    {
        let default = found.remove(pos);
        found.insert(0, default);
    }

    found
}

/// Turn an AWS SDK error into something worth putting in a status bar.
///
/// Two credential failures dominate on a developer machine, and neither of the
/// SDK's messages says how to fix itself:
///
/// * An **expired SSO session** — the token is there but stale.
/// * An **exhausted credential chain** — nothing resolved at all. The SDK
///   reports this by listing every provider it tried, which is six lines of
///   "the credential provider was not enabled" and no advice. Verbatim, it
///   fills the Repositories pane and buries the one fact that matters, so it is
///   replaced rather than appended to.
///
/// Both get the same remedy line, since `aws sso login` is the fix for the
/// overwhelmingly common case of an SSO profile that was never logged into.
/// Anything else is passed through untouched — an IAM authorization failure is
/// already specific, and appending a login hint to it would be misdirection.
///
/// Pure and string-matching by necessity: the SDK models these as opaque
/// provider errors with no typed variant to match on.
pub fn describe_failure(error: &str, aws_profile: Option<&str>) -> String {
    let lowered = error.to_lowercase();

    let remedy = match aws_profile {
        Some(profile) => format!("try: aws sso login --profile {profile}"),
        None => "try: aws sso login, or set AWS_PROFILE".to_owned(),
    };

    let chain_exhausted = [
        "the credential provider was not enabled",
        "no providers in chain",
        "credentialsnotloaded",
    ]
    .iter()
    .any(|needle| lowered.contains(needle));

    if chain_exhausted {
        return match aws_profile {
            Some(profile) => format!("no AWS credentials for profile \"{profile}\" — {remedy}"),
            None => format!("no AWS credentials found — {remedy}"),
        };
    }

    let expired_sso = ["sso", "expired", "token has expired"]
        .iter()
        .any(|needle| lowered.contains(needle));

    if expired_sso {
        return format!("{error} — {remedy}");
    }

    error.to_owned()
}

// ---------------------------------------------------------------------------
// AWS glue
// ---------------------------------------------------------------------------

/// Load an AWS SDK config for `target`.
async fn sdk_config(target: &EcrTarget, default_region: Option<&str>) -> aws_config::SdkConfig {
    let mut loader = aws_config::defaults(BehaviorVersion::latest());

    if let Some(profile) = &target.aws_profile {
        loader = loader.profile_name(profile);
    }
    if let Some(region) = target.region.as_deref().or(default_region) {
        loader = loader.region(Region::new(region.to_owned()));
    }

    loader.load().await
}

/// Mint a registry password for a private ECR registry.
///
/// Also the only way to learn the registry's hostname: `proxyEndpoint` embeds
/// the account ID, which is why an ECR profile need not configure a URL.
pub async fn authorize(target: &EcrTarget) -> Result<EcrAuthorization> {
    let client = aws_sdk_ecr::Client::new(&sdk_config(target, None).await);

    let output = client
        .get_authorization_token()
        .send()
        .await
        .context("ecr:GetAuthorizationToken failed")?;

    let data = output
        .authorization_data()
        .first()
        .context("ecr:GetAuthorizationToken returned no authorization data")?;

    let authorization_token = data
        .authorization_token()
        .context("ecr:GetAuthorizationToken returned no token")?
        .to_owned();
    let registry_url = data
        .proxy_endpoint()
        .context("ecr:GetAuthorizationToken returned no registry endpoint")?
        .to_owned();

    Ok(EcrAuthorization {
        registry_url,
        authorization_token,
        expires_at: data
            .expires_at()
            .and_then(|t| SystemTime::try_from(*t).ok()),
    })
}

/// Mint a registry password for ECR Public.
///
/// A separate service and a separate token: `ecr-public` answers only in
/// `us-east-1` and its registry is the fixed `public.ecr.aws`, with no account
/// ID to discover.
pub async fn authorize_public(target: &EcrTarget) -> Result<EcrAuthorization> {
    let config = sdk_config(target, Some(ECR_PUBLIC_API_REGION)).await;
    let client = aws_sdk_ecrpublic::Client::new(&config);

    let output = client
        .get_authorization_token()
        .send()
        .await
        .context("ecr-public:GetAuthorizationToken failed")?;

    let data = output
        .authorization_data()
        .context("ecr-public:GetAuthorizationToken returned no authorization data")?;

    Ok(EcrAuthorization {
        registry_url: ECR_PUBLIC_REGISTRY_URL.to_owned(),
        authorization_token: data
            .authorization_token()
            .context("ecr-public:GetAuthorizationToken returned no token")?
            .to_owned(),
        expires_at: data
            .expires_at()
            .and_then(|t| SystemTime::try_from(*t).ok()),
    })
}

/// Authorize and list repositories with one SDK client.
pub async fn connect(target: &EcrTarget) -> Result<EcrConnection> {
    let config = sdk_config(target, None).await;
    let client = aws_sdk_ecr::Client::new(&config);

    let output = client
        .get_authorization_token()
        .send()
        .await
        .context("ecr:GetAuthorizationToken failed")?;
    let data = output
        .authorization_data()
        .first()
        .context("ecr:GetAuthorizationToken returned no authorization data")?;

    let authorization = EcrAuthorization {
        registry_url: data
            .proxy_endpoint()
            .context("ecr:GetAuthorizationToken returned no registry endpoint")?
            .to_owned(),
        authorization_token: data
            .authorization_token()
            .context("ecr:GetAuthorizationToken returned no token")?
            .to_owned(),
        expires_at: data
            .expires_at()
            .and_then(|t| SystemTime::try_from(*t).ok()),
    };

    let mut repos = Vec::new();
    let mut pages = client.describe_repositories().into_paginator().send();
    while let Some(page) = pages.next().await {
        let page = page.context("ecr:DescribeRepositories failed")?;
        repos.extend(
            page.repositories()
                .iter()
                .filter_map(|r| r.repository_name())
                .map(str::to_owned),
        );
    }
    repos.sort();

    Ok(EcrConnection {
        authorization,
        repos,
    })
}

/// Authorize and list repositories for ECR Public.
///
/// The listing is the set of repositories *this account publishes* — there is
/// no API that enumerates the whole public registry. Anyone else's public image
/// is still reachable by name, anonymously, which is why an empty list here is
/// not an error.
pub async fn connect_public(target: &EcrTarget) -> Result<EcrConnection> {
    let config = sdk_config(target, Some(ECR_PUBLIC_API_REGION)).await;
    let client = aws_sdk_ecrpublic::Client::new(&config);

    let authorization = authorize_public(target).await?;

    let mut repos = Vec::new();
    let mut pages = client.describe_repositories().into_paginator().send();
    while let Some(page) = pages.next().await {
        let page = page.context("ecr-public:DescribeRepositories failed")?;
        repos.extend(
            page.repositories()
                .iter()
                .filter_map(|r| r.repository_name())
                .map(str::to_owned),
        );
    }
    repos.sort();

    Ok(EcrConnection {
        authorization,
        repos,
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_profile_sections_from_both_file_shapes() {
        let config = "\
[default]
region = ap-southeast-2

[profile pgmac]
region = us-east-1
output = json
";
        let credentials = "\
[pg-terraform-state]
aws_access_key_id = AKIAEXAMPLE
";

        let profiles = parse_aws_profiles(config, credentials);

        assert_eq!(
            profiles,
            vec![
                AwsProfile {
                    name: "default".to_owned(),
                    region: Some("ap-southeast-2".to_owned()),
                },
                AwsProfile {
                    name: "pgmac".to_owned(),
                    region: Some("us-east-1".to_owned()),
                },
                AwsProfile {
                    name: "pg-terraform-state".to_owned(),
                    region: None,
                },
            ]
        );
    }

    #[test]
    fn hoists_default_to_the_front() {
        let config = "\
[profile work]
region = eu-west-1

[default]
region = us-east-1
";

        let profiles = parse_aws_profiles(config, "");

        assert_eq!(profiles[0].name, "default");
        assert_eq!(profiles[1].name, "work");
    }

    #[test]
    fn a_profile_in_both_files_is_listed_once() {
        let profiles = parse_aws_profiles("[profile shared]\nregion = us-west-2\n", "[shared]\n");

        assert_eq!(profiles.len(), 1);
        assert_eq!(profiles[0].region.as_deref(), Some("us-west-2"));
    }

    #[test]
    fn profile_names_keep_their_case_and_punctuation() {
        // `~/.aws/config` really does contain names like this.
        let profiles = parse_aws_profiles("[profile Paulymac@paulymac]\n", "");

        assert_eq!(profiles[0].name, "Paulymac@paulymac");
    }

    #[test]
    fn comments_and_blank_sections_are_ignored() {
        let config = "\
# [profile commented-out]
; region = nowhere
[]
[profile real]
region = ap-south-1
";

        let profiles = parse_aws_profiles(config, "");

        assert_eq!(profiles.len(), 1);
        assert_eq!(profiles[0].name, "real");
    }

    #[test]
    fn a_token_is_valid_until_the_skew_window() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(10_000);
        let auth = EcrAuthorization {
            registry_url: "https://1.dkr.ecr.us-east-1.amazonaws.com".to_owned(),
            authorization_token: "QVdTOnB3".to_owned(),
            expires_at: Some(now + EXPIRY_SKEW + Duration::from_secs(1)),
        };

        assert!(auth.is_valid_at(now));
        assert!(!auth.is_valid_at(now + Duration::from_secs(2)));
    }

    #[test]
    fn a_token_without_an_expiry_is_never_reused() {
        let auth = EcrAuthorization {
            registry_url: "https://1.dkr.ecr.us-east-1.amazonaws.com".to_owned(),
            authorization_token: "QVdTOnB3".to_owned(),
            expires_at: None,
        };

        assert!(!auth.is_valid_at(SystemTime::UNIX_EPOCH));
    }

    #[test]
    fn the_authorization_token_is_sent_verbatim_as_basic() {
        let auth = EcrAuthorization {
            registry_url: "https://1.dkr.ecr.us-east-1.amazonaws.com".to_owned(),
            authorization_token: "QVdTOnB3".to_owned(),
            expires_at: None,
        };

        assert_eq!(auth.header_value(), "Basic QVdTOnB3");
    }

    #[test]
    fn an_expired_sso_session_gets_the_login_command() {
        let msg = describe_failure("the SSO session has expired or is invalid", Some("pgmac"));

        assert!(msg.contains("aws sso login --profile pgmac"), "{msg}");
        assert!(msg.contains("SSO session has expired"), "{msg}");
    }

    /// The real message the SDK emits when nothing resolves: six provider lines
    /// and no advice. It is replaced, not appended to — verbatim it fills the
    /// pane and buries the one fact that matters.
    #[test]
    fn an_exhausted_credential_chain_is_replaced_with_the_remedy() {
        let chain = "dispatch failure: \
             Environment: the credential provider was not enabled: \
             Profile: the credential provider was not enabled: \
             WebIdentityToken: the credential provider was not enabled: \
             EcsContainer: the credential provider was not enabled: \
             Ec2InstanceMetadata: the credential provider was not enabled";

        let msg = describe_failure(chain, Some("pgmac"));

        assert_eq!(
            msg,
            "no AWS credentials for profile \"pgmac\" — try: aws sso login --profile pgmac"
        );
        assert!(!msg.contains("provider was not enabled"), "{msg}");
    }

    #[test]
    fn without_a_named_profile_the_remedy_mentions_the_environment() {
        let msg = describe_failure("Profile: the credential provider was not enabled", None);

        assert!(msg.contains("AWS_PROFILE"), "{msg}");
    }

    /// An authorization failure is already specific; a login hint on top of it
    /// would send the user to fix the wrong thing.
    #[test]
    fn an_unrelated_failure_is_passed_through_unchanged() {
        let original = "User is not authorized to perform: ecr:DescribeRepositories";

        assert_eq!(describe_failure(original, Some("pgmac")), original);
    }
}

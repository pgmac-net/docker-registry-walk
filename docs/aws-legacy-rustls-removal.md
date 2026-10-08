# Dropping the legacy rustls stack from the AWS SDK crates (issue #139)

Dependabot alerts #1, #2 (low) and #3 (high, GHSA-82j2-j2ch-gfr8: DoS via
panic on a malformed CRL BIT STRING) all flagged `rustls-webpki 0.101.7`.

## Cause

`Cargo.lock` held two copies of `rustls-webpki`: `0.103.15` (patched) and
`0.101.7`. The 0.101.x line never received a fixed release, so `cargo update`
could not help.

`aws-sdk-ecr` and `aws-sdk-ecrpublic` enable a deprecated default feature
`rustls` -> `aws-smithy-runtime/tls-rustls` ->
`aws-smithy-http-client/legacy-rustls-ring`, which compiles in `rustls 0.21`,
`hyper-rustls 0.24` and `rustls-webpki 0.101.7`. The modern
`default-https-client` (rustls 0.23) was already enabled alongside it and is the
client the SDK uses at runtime: `src/` never selects a connector, and
`aws_config::defaults(BehaviorVersion::latest())` in `src/registry/ecr.rs` takes
the default one.

## Fix

Both SDK crates use `default-features = false, features =
["default-https-client", "rt-tokio"]` (their previous defaults minus the legacy
`rustls`). The lockfile dropped `rustls 0.21`, `hyper-rustls 0.24`,
`tokio-rustls 0.24`, `rustls-webpki 0.101.7` and related crates. `CLAUDE.md`
carries an invariant so a later bump does not restore the defaults.

## Verification

- `cargo tree -i rustls-webpki`: a single `0.103.15`; `cargo tree -d` shows no
  duplicate TLS crates.
- `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`,
  `cargo test` (258 pass), `cargo build --release`.
- Not tested against a live ECR endpoint (no AWS credentials in the build
  environment); do one manual ECR connect after release.

## Process note

Picked up via `pgmac-workflows:pickup-ticket` after reading the alert. Plan
posted to the ticket and approved before any change; rated STANDARD; no
deviations. Alert #4 (`rustls`) was already fixed.

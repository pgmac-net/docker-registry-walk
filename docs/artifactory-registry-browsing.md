# Browsing JFrog Artifactory-hosted registries

A JFrog Artifactory instance doesn't fit the app's usual "one config profile
= one flat Docker Registry v2 root" model: a single instance commonly hosts
many independent Docker repositories ("repo-keys"), each exposed as its own
`/v2/` root under a URL path prefix — the documented "Repository Path
Method": `https://<host>/artifactory/api/docker/<repo-key>/v2/...`.

## Two bugs this uncovered

1. **URL path-prefix joining.** `RegistryClient::url()` built every request
   URL with `base_url.join(path)`, where `path` was always written with a
   leading `/` (`"/v2/_catalog"`, etc.). Per RFC 3986, a leading-`/` path is
   an *absolute-path reference*, which replaces `base_url`'s path entirely
   instead of appending to it — so a `base_url` of
   `https://host/artifactory/api/docker/myrepo` would have its whole prefix
   silently dropped, sending every request to `https://host/v2/...` instead.
   Fixed by normalizing `base_url` to end with `/` and stripping the leading
   `/` off request paths before joining (`join_path` in `src/registry/client.rs`),
   so the join is relative (appends) instead of absolute (replaces). This
   affects any path-prefix-mounted registry, not just Artifactory.

2. **Auth.** `make_client_for_profile` (`src/tui/event.rs`) always wrapped a
   configured username/password in `BearerCredentials` (token-exchange
   flow). `BasicCredentials` existed in `src/registry/auth.rs` but was dead
   code. Artifactory's Docker v2 endpoint and REST API authenticate via
   plain HTTP Basic (username + API key/identity token), not Bearer token
   exchange — and `BearerCredentials::refresh()` degrades gracefully when no
   `Bearer` challenge is present (sends *no* `Authorization` header at all),
   so requests against Artifactory would silently go out unauthenticated.
   Fixed by branching to `BasicCredentials` for `RegistryType::Artifactory`
   profiles.

## Design

- `RegistryType::Artifactory` (`src/config.rs`): `url` is the **Artifactory
  server base** (e.g. `https://artifactory.example.com/artifactory`), not a
  `/v2/` root.
- `RegistryClient::artifactory_repositories()` (`src/registry/client.rs`):
  `GET {base}/api/repositories?packageType=docker` → `Vec<ArtifactoryRepo>`.
- `RegistryClient::for_artifactory_repo(repo_key)`: returns a client scoped
  to `<base>/api/docker/<repo_key>/`, sharing the same HTTP client and
  credentials — from here on it's browsed exactly like any other registry.
- TUI flow: switching to an Artifactory profile (`R`) opens
  `Modal::ArtifactoryPicker` instead of fetching a catalog directly — a
  one-shot fetch of repo-keys, filtered locally as you type (unlike
  `SearchPicker`'s incremental Docker Hub search). Picking a repo-key builds
  the scoped client, stores it in the event loop's client map under a
  composite key (`"{profile_name}#{repo_key}"`), and from that point every
  existing repos/tags/detail/ops code path runs unmodified.

## Scope boundary

Only the read/browse path goes through the path-prefix fix
(`catalog_page`, `tags_page`, `get_manifest`, `head_blob`, `get_blob`,
`start_blob_upload`, `mount_blob`, `ping`, `artifactory_repositories`).
`complete_blob_upload`'s handling of a server-returned `Location` header is
untouched — Artifactory push/upload support under a path prefix is a
separate, not-yet-needed piece of work.

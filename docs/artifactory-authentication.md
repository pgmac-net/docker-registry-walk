# Artifactory access-token authentication

Issue [#90](https://github.com/pgmac-net/docker-registry-walk/issues/90).

## The problem

> I would like to use oauth authentication for artifactory hosts. The same way terraform authenticates to artifactory.

"The same way terraform does it" turned out to be the load-bearing part, and it is not a browser OAuth flow. The `jfrog/artifactory` Terraform provider authenticates by sending a JFrog **access token** as `Authorization: Bearer <jwt>`, read from `JFROG_ACCESS_TOKEN` or `ARTIFACTORY_ACCESS_TOKEN`. It does have an OIDC path, but that one requires `TFC_WORKLOAD_IDENTITY_TOKEN` — a Terraform Cloud workload identity token — which has no meaning in an interactive terminal.

So the ask is: authenticate to Artifactory with a bearer access token, with no username.

That was impossible before this change, in two compounding ways:

1. `make_client_for_profile` gated the whole credential path behind `if let Some(username) = &profile.username`. A token-only profile has no username, so it fell through to `NoCredentials` and every request went out anonymous.
2. For `RegistryType::Artifactory` the only credential it could build was `BasicCredentials`.

## Two authenticated surfaces

The app talks to Artifactory over two different APIs, and they do not authenticate identically. This is the single most important fact in the design.

| Surface | Path | Accepts |
|---|---|---|
| Artifactory REST | `{base}/api/repositories` | the access token directly, as `Authorization: Bearer` |
| Docker Registry v2 | `{base}/api/docker/<repo-key>/v2/…` | HTTP Basic, or a scoped token minted by its own `/v2/token` realm — and, in practice, usually the access token directly too |

The second one may answer with a standard `WWW-Authenticate: Bearer realm="…/v2/token",service=…,scope=…` challenge instead of accepting the raw token.

Conveniently, `RegistryClient::send` already retried a 401 through `Credentials::get_authorization_for_challenge` — the seam that exists for Docker Hub's per-endpoint scoped tokens. So handling Artifactory's challenge needed **no change to `client.rs`**: the new credential just implements that trait method.

## Design

### `AccessTokenCredentials`

`get_authorization` returns `Bearer <raw token>` unconditionally, with no network call and no cache.

That is not laziness, it is a constraint. `RegistryClient::for_artifactory_repo` clones the client and shares **one** `Arc<dyn Credentials>` between the REST client and the repo-key-scoped client. Those two clients hit the two surfaces in the table above, which have different auth verifiers, and the raw access token is the only value valid at both.

The consequence: **the challenge path must never write state that the fast path reads.** If a repo-scoped token minted by the Docker realm were cached in a global slot — the way `BearerCredentials::refresh` legitimately does — the next `/api/repositories` call would present a repo-scoped Docker token and 401. That is precisely the Docker Hub scope cascade already recorded in `CLAUDE.md`, reached by a different route.

So the minted scoped token is returned and immediately forgotten. What *is* remembered is only which *presentation* worked, which is neither a secret nor scoped to an endpoint.

### The realm presentation ladder

On a challenge, the token is offered to the realm in this order:

1. **Pass-through** — `Authorization: Bearer <token>`. Tried first: the realm is the same Artifactory front door that accepts the token, and it needs no username, which matters because a token-only profile has none.
2. **Basic with the configured username** — only when the profile has one.
3. **Basic with the username decoded from the token** — JFrog access tokens are JWTs whose `sub` looks like `jfrt@<instance-id>/users/<name>`. `jwt_subject_username` reads that segment. The signature is deliberately *not* verified: this only fills in a username field, it is never a trust decision. Returns `None` for JFrog reference tokens (short opaque strings, not JWTs).
4. **Basic with the token as both username and password** — Docker's identity-token convention, as a last resort.

Whichever rung works is cached in a sticky `Mutex<Option<RealmMode>>` and is the only one tried thereafter. Besides saving up to three wasted round trips per 401, this stops repeated failing Basic attempts carrying a *real* username from counting against Artifactory's "Max Failed Login Attempts" policy for that user — which is the reason it is not merely an optimisation.

> **Note:** the ladder was implemented in full because there was no Artifactory instance available to test against while building this, and it still isn't verified live (see [#96](https://github.com/pgmac-net/docker-registry-walk/issues/96)). An attempt to measure it by standing up `jfrog/artifactory-oss` locally (with a real PostgreSQL backend, after the image's embedded-DB path turned out to be gone) hit a harder wall: **Docker repository support itself is a Pro-only feature, absent entirely from OSS** — `PUT /api/repositories/*` and even reading an existing repo's config both return `This REST API is available only in Artifactory Pro`, and the fixed `/api/docker/<repo-key>/v2/...` route 401s unconditionally regardless of credentials once a Docker repo can't exist to serve it. No amount of local wiring gets past a licensed-feature wall, so the question stays open pending either a Pro trial license or a real instance elsewhere. Three read-only curls settle it, if one becomes available:
>
> ```sh
> curl -s -o /dev/null -w '%{http_code}\n' -H "Authorization: Bearer $JFROG_ACCESS_TOKEN" \
>   "$BASE/api/repositories?packageType=docker"
> curl -s -o /dev/null -w '%{http_code}\n' -H "Authorization: Bearer $JFROG_ACCESS_TOKEN" \
>   "$BASE/api/docker/<repo-key>/v2/_catalog"
> curl -si "$BASE/api/docker/<repo-key>/v2/_catalog" | grep -i www-authenticate
> ```
>
> In the meantime, #96 (the scope-keyed token cache this ladder's cost would motivate) is closed on documented evidence rather than a live measurement: JFrog's own docs state an access token "can be used as a bearer token in authorization headers" against Artifactory endpoints, which is the same conclusion Arm A of #96's plan expected from a 200. Whether the *specific* Docker v2 challenge path ever fires — and so whether ladder rungs 2–4 are reachable at all — remains genuinely unknown; no cleanup ticket for them has been filed on the strength of docs alone.

### A stricter realm guard

`is_trusted_realm` accepts any realm host sharing the registry's last two DNS labels. That relaxation exists for one reason: Docker Hub splits `auth.docker.io` from `registry-1.docker.io`.

Applied to access tokens it is dangerous. For a registry at `artifactory.corp.example.com` it would also trust every other host under `example.com` — a marketing site, a shared-hosting subdomain, a dangling CNAME open to takeover. And what would be disclosed is not one repository's pull token but a JFrog **platform** access token, valid across the whole REST API.

Artifactory's token realm is always same-origin, so the relaxation buys nothing here. `is_same_origin_realm` therefore requires an exact host *and* an exact port, and fails closed: an unusual proxy that relocates the realm gets a visible auth error rather than silently handing over the token. `is_trusted_realm` is left untouched so Docker Hub keeps working.

`same_origin_realm_rejects_sibling_subdomain` asserts both halves of this — that the old guard allows the case and the new one rejects it.

### Configuration

A new per-profile `auth` key: `auto` (default), `basic`, `bearer`, `token`.

The decision of which credential to build lives in `RegistryProfile::auth_kind(has_token, has_password)` — pure, and so unit-tested across the entire matrix with no keyring, environment or network. `make_client_for_profile` does only the impure lookups and the construction.

`auto` is behaviour-identical to the code that predates the field:

| type | `auth` | username | token | password | credential |
|---|---|---|---|---|---|
| Artifactory | auto | yes | – | yes | Basic *(unchanged)* |
| Artifactory | auto | yes | yes | no | **AccessToken** *(was anonymous)* |
| Artifactory | auto | no | yes | – | **AccessToken** *(was anonymous)* |
| Standard/Hub | auto | yes | – | yes | Bearer *(unchanged)* |
| Standard/Hub | auto | – | yes | no | None — `JFROG_*` not consulted |
| any | token | any | yes | – | AccessToken |
| any | basic \| bearer | no | – | – | `validate()` error |

Two deliberate choices in there:

- Under `auto`, a token is used only when Basic is *not* possible. A profile that authenticates with a username and password today keeps doing so even if a token also happens to be present, so upgrading changes nothing. Set `auth = "token"` to prefer the token when both exist.
- Under `auto`, only Artifactory consults `JFROG_ACCESS_TOKEN` / `ARTIFACTORY_ACCESS_TOKEN`. A stray JFrog variable exported in someone's shell must not change how they authenticate to `docker.io`.

### Token resolution

`$JFROG_ACCESS_TOKEN` → `$ARTIFACTORY_ACCESS_TOKEN` → keychain → masked prompt.

The environment wins so a token can be overridden for one invocation without disturbing stored state. A token that came from the environment is deliberately **not** persisted to the keychain — an ambient override should not silently become permanent.

Keychain storage uses a fixed account name, `__token__`, rather than a username: a token-authenticated profile has none. It is used even when a username *is* configured, so a stored password and a stored token coexist instead of overwriting each other.

## Three pre-existing defects fixed alongside

These were found while mapping the code and all three sit directly on this feature's path, so they ship together rather than as follow-ups.

### 1. The input modal echoed secrets in cleartext

`draw_input_modal` rendered `input.buffer` verbatim, and the modal dispatch routes *every* `Modal::Input` there — including `InputAction::EnterPassword`. Passwords were displayed on screen.

Fixed by deriving masking from the action (`InputAction::is_secret()`) rather than adding a flag to `Modal::Input`, so none of the seven construction sites can forget it and a future credential action only has to be listed in one place. The mask is applied *after* the visible slice is computed, so the scroll window and cursor column still come from the real buffer and needed no adjustment. The plaintext echo is replaced by a `(N chars)` counter — a masked 600-character JWT is unreadable, but a length confirms a paste landed.

This mattered enough to block the feature: the token prompt reuses this modal, and a JFrog access token is a wider credential than a registry password.

### 2. Re-auth did nothing while browsing a repo-key

Two bugs stacked:

- Entering a credential rebuilt the client and inserted it under the **bare** profile name, but Artifactory repo-key clients are keyed `<profile>#<repo-key>`. So `active_name == profile_name` was false whenever the user was inside a repo-key, and no refetch was ever spawned.
- Even once the root was replaced, the scoped clients still held the `Arc<dyn Credentials>` cloned when they were derived — so they would have kept using the credential that just failed.

`rebuild_clients_for_profile` re-derives every `<profile>#<repo-key>` entry from a freshly built root, and `is_client_key_for` decides whether the active client belongs to the profile that changed. `Config::validate` now rejects `#` in profile names to keep that prefix match unambiguous.

The retry also used to call `start_registry_switch`, which goes through `reset_for_new_registry` and clears `current_artifactory_repo_key` — visually ejecting the user back to the repo-key picker on a *successful* re-auth, while the client stayed scoped to the repo-key. `App::restart_catalog_load` is the narrower replacement: it reloads the catalog and leaves `registry_name`, `registry_url` and `current_artifactory_repo_key` alone.

### 3. 403 was not treated as an auth failure

`require_success` maps 403 to `UnexpectedStatus`, so `auth_failed` was false. Artifactory answers a valid-but-under-privileged token with 403, which meant the user saw "catalog unavailable" and was never offered the chance to supply a better credential. `is_auth_failure` now covers 401 and 403.

## Verifying it

Unit-level (129 tests, all green; `cargo clippy -- -D warnings` clean):

```sh
cargo test config::tests          # the auth_kind matrix
cargo test registry::auth::tests  # realm guards, token helpers, JWT subject
cargo test tui::event::tests      # client-cache keys, prompt selection
```

End-to-end, token mode with **no username**:

```toml
[[registry]]
name = "artifactory"
url  = "https://artifactory.example.com/artifactory"
type = "artifactory"
auth = "token"
```

1. `JFROG_ACCESS_TOKEN=<token> docker-registry-walk --registry artifactory` → the repo-key picker populates, proving Bearer works on `/api/repositories`.
2. Select a repo-key → the catalog lists, proving auth on `/api/docker/<key>/v2/`.
3. Drill into a tag → manifest and config blob load.
4. Unset the variable, run `--registry artifactory --token`, paste at the prompt → only mask characters and a `(N chars)` counter appear, and it persists for the next run.
5. Re-auth regression: while browsing a repo-key, invalidate the token (`secret-tool clear service docker-registry-walk/artifactory username __token__`, and unset the env var), trigger a refetch, then enter a good token → the catalog reloads **and the repo-key context is preserved**.
6. Backward compatibility: an existing username + password Artifactory profile with no `auth` key still uses Basic, unchanged. Docker Hub and standard registries are untouched.

## Follow-ups

Deliberately out of scope, each worth its own issue:

- **`Authorization` sent to arbitrary hosts on blob upload.** `start_blob_upload` accepts an absolute `Location` on any host and `send` attaches the header unconditionally. reqwest strips `Authorization` on cross-origin *redirects*, but this is a fresh request, so nothing strips it. Pre-existing for Basic and worse for an access token — but it is on the upload/copy path, not the browse path this issue covers.
- **Scope-keyed token cache**, if the challenge path turns out to be hot. It must be keyed by the exact scope string and never read by `get_authorization`.
- **`spawn_blocking` for keyring reads.** The `keyring` crate falls back to forking `secret-tool` synchronously on headless Linux, on the event-loop thread. Adding a token lookup doubles the stalls.
- **Cached clients are never rebuilt on registry switch** (`clients.entry(..).or_insert_with(..)`), so a credential changed out of band is not picked up.
- **Bracketed paste is not enabled**, so a pasted token arrives as N discrete key events, each triggering a full redraw.

## Process note

The ticket said "oauth", which could have meant a browser authorization-code flow with PKCE, a localhost callback listener and refresh-token handling — a far larger change requiring new dependencies. Checking what the Terraform provider actually does first turned that into a static bearer token and no new dependencies at all. Reading the referenced implementation was worth more than reading the word.

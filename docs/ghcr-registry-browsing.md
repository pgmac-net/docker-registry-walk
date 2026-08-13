# GHCR registry browsing

How `docker-registry-walk` browses GitHub Container Registry (`ghcr.io`), and
why it does it the way it does. Issue
[#91](https://github.com/pgmac-net/docker-registry-walk/issues/91).

## Configuration

```toml
[[registry]]
name = "ghcr"
url = "https://ghcr.io"
type = "ghcr"
# owner = "pgmac-net"   # optional; omit to list your own packages
```

`type` may be omitted when the URL host is `ghcr.io` — it is a hosted service
at one fixed hostname, so the host is conclusive, and detection mirrors Docker
Hub's. (Artifactory deliberately has no such sniffing: it is self-hosted and
could be at any hostname.)

`owner` chooses whose packages are listed. Omit it for the token holder's own.
It accepts a **user or an organisation** — GitHub's `/users/{owner}/packages`
endpoint serves both, so there is no separate org setting.

Ad hoc, without touching the config:

```sh
docker-registry-walk --url https://ghcr.io --type ghcr --owner Homebrew
```

## Authentication

A GitHub personal access token with the **`read:packages`** scope. It is
resolved from `$CR_PAT`, `$GITHUB_TOKEN`, `$GH_TOKEN`, then the OS keychain,
then a masked prompt — see the README's "Access tokens" section. `CR_PAT` is
first because it is GitHub's own name for a GHCR token; the other two are
general-purpose variables that surrounding tooling exports for other reasons.

These variables are read **only** for `type = "ghcr"` profiles, and an
Artifactory profile's `JFROG_*` / `ARTIFACTORY_*` variables are never read for
GHCR. The partition matters in both directions: `GITHUB_TOKEN` is set on most
developer machines, so without it a stray export would silently become an
Artifactory credential.

### The token is exchanged, not sent

**GHCR rejects a raw PAT on its `/v2/` API with `403 Forbidden` — not a `401`
with a `WWW-Authenticate` challenge.** This is the single most important fact
about the implementation, because `RegistryClient::send` only re-challenges on
401. A 403 is terminal: there is nothing to recover from. Presenting the PAT
eagerly therefore breaks *every* request, including ones that would have
succeeded anonymously.

So `GhcrCredentials::get_authorization` returns `None` — deliberately, and it
must stay that way. The unauthenticated request draws GHCR's 401, which carries
that endpoint's **real** scope:

```
www-authenticate: Bearer realm="https://ghcr.io/token",service="ghcr.io",
                  scope="repository:homebrew/core/git:pull"
```

`get_authorization_for_challenge` then exchanges the PAT at
`https://ghcr.io/token` for a token valid for exactly that scope. The minted
token is **not** cached: it is scoped to one repository, so reusing it
elsewhere would reproduce the Docker Hub scope cascade that
`BearerCredentials` documents.

Note the contrast with `AccessTokenCredentials`, which sends its token
unconditionally and *must* — Artifactory's REST API and Docker endpoint both
accept the raw token, and they share one credentials `Arc`. The two types look
like duplicates and are not; see `CLAUDE.md`.

The bare `/v2/` probe is no use for discovering scope, incidentally: it returns
a canned `scope="repository:user/image:pull"` that names a repository nobody
owns, and exchanging it yields `403 DENIED`.

### Anonymous access

`token` is optional. The same exchange works with no credentials at all, which
is how an unauthenticated `docker pull` of a public image works — so a GHCR
profile with no PAT can still browse public packages by name.

What it *cannot* do is list them: GitHub exposes no anonymous package listing,
even for public packages. A profile with no token therefore lands on the
existing "Catalog unavailable. Enter repo name to browse:" prompt, and browsing
proceeds normally from there.

## Repository discovery

GHCR implements no `/v2/_catalog` — it answers 401 unconditionally. The
repository list comes from the GitHub packages API instead:

| `owner` | Endpoint |
|---------|----------|
| set     | `GET /users/{owner}/packages?package_type=container` |
| unset   | `GET /user/packages?package_type=container` |

This lives in `src/registry/ghcr.rs` as a free function over its own `reqwest`
call, **not** as a `RegistryClient` method. `api.github.com` is a different
origin from `ghcr.io`, and `RegistryClient::send` strips `Authorization` from
off-origin requests on purpose — that guard is what keeps the credential off
server-supplied URLs such as a blob-upload `Location` pointing at S3. Sending
the PAT to `api.github.com` is intended, so it is done openly, with the token
passed in, rather than by carving an exception into the guard.

Consequences worth knowing:

- **Repository names are `lowercase(owner)/<package name>`.** The API echoes
  the owner's login with its original casing (`Homebrew`) while GHCR paths are
  lowercase, and package names are already nested — so `core/sqldiff` under
  `Homebrew` is the three-segment path `homebrew/core/sqldiff`.
- **Pagination follows `Link; rel="next"`.** The `next` URL rewrites to a
  numeric-id form (`/user/1503512/packages?...`), so links must be followed,
  not reconstructed from page numbers.
- **Listings are capped at 50 pages** (5,000 packages). Homebrew has roughly
  twice that. On hitting the cap the list is returned with a truncation notice
  rather than silently short, and the picker's filter is the way to reach the
  rest.

## The picker

Switching to a GHCR profile opens a filterable package picker, the same shape
as the Artifactory repo-key picker (they share a renderer,
`draw_filter_picker_modal`). `Backspace` / `u` reopens it mid-browse, served
from cache.

One fetch fills **two** surfaces: the picker *and* the Repos pane. This differs
from Artifactory on purpose. An Artifactory repo-key is a whole sub-registry,
so picking one is followed by fetching *its* catalog into the Repos pane. A
GHCR package is just a repository — there is no second level — so populating
only the picker would leave the pane empty behind it, with nothing to return to
on `Esc`.

Confirming a package therefore only moves the Repos pane's selection; the event
loop reloads tags whenever the selected repo changes. Issuing a `BrowseRepo`
as well would fetch the same tags twice, and `on_tags_page` appends — which
showed up in testing as every tag appearing exactly twice.

## What works, and what doesn't

Browsing is ordinary Docker v2 once a repository is known: tags, manifests,
the JSON inspector and layer details all work, and tag pagination uses the
same `Link` parser as any other registry.

**Deletion, copy and retag are not supported for GHCR.** It offers no
registry-level manifest `DELETE`; removal goes through
`DELETE /user/packages/container/{name}/versions/{id}`, which needs a
version-id lookup with no registry-API equivalent. That is deliberately out of
scope for #91.

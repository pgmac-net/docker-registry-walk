# AWS ECR registry browsing

How `docker-registry-walk` browses Amazon Elastic Container Registry — both
private registries and ECR Public — and why it does it the way it does. Issue
[#67](https://github.com/pgmac-net/docker-registry-walk/issues/67).

## Configuration

```toml
[[registry]]
name = "ecr"
type = "ecr"
aws_profile = "default"        # optional; falls back to the AWS chain
region = "ap-southeast-2"      # optional; falls back to the AWS chain

[[registry]]
name = "ecr-public"
type = "ecr-public"
aws_profile = "default"        # optional
```

**There is no `url`.** A private ECR registry's hostname is
`<account-id>.dkr.ecr.<region>.amazonaws.com`, and the account ID is not
something you should have to look up — so the endpoint is derived at runtime
from the `proxyEndpoint` that `ecr:GetAuthorizationToken` returns. `url` is
optional for these two types and required for every other one. Writing it
explicitly still works, and is still validated.

ECR Public needs no derivation at all: it is the fixed `https://public.ecr.aws`,
one global registry rather than one per region, which is why `region` does not
apply to it.

`type` may be omitted when the URL host gives it away — a host matching
`*.dkr.ecr.*.amazonaws.com`, or `public.ecr.aws` — the same URL sniffing GHCR
and Docker Hub get. The `.dkr.ecr.` infix is required: `amazonaws.com` hosts
every AWS service, so the domain alone proves nothing.

Ad hoc, without touching the config:

```sh
docker-registry-walk --type ecr --aws-profile pgmac --region ap-southeast-2
docker-registry-walk --type ecr-public
```

## Authentication

Credentials come from the **ordinary AWS credential chain** — SSO, assume-role,
static keys, container and instance roles — via the AWS SDK. Whatever works for
`aws ecr get-login-password` works here.

Two consequences distinguish ECR from every other registry type:

- **Nothing is stored in the keychain.** The registry password is a token minted
  by `ecr:GetAuthorizationToken` that expires in about twelve hours, so there is
  no durable secret to keep. `--password` and `--token` do not apply.
- **No token environment variables are read.** `RegistryProfile::token_env_vars`
  returns an empty slice for both ECR types. A `GITHUB_TOKEN` or
  `JFROG_ACCESS_TOKEN` exported for something else can never be offered to AWS;
  the SDK reads its own vocabulary (`AWS_PROFILE`, `AWS_REGION`, …) for itself.

An authentication failure therefore never opens a credential prompt — there is
nothing a user could usefully type. It is reported as the AWS problem it is.

### Required IAM permissions

| Action | Needed for |
|---|---|
| `ecr:GetAuthorizationToken` | Everything — it mints the registry password *and* reveals the endpoint |
| `ecr:DescribeRepositories` | Listing repositories in the Repos pane |

They are separate permissions, and the app treats them that way: an identity
that can pull images but not enumerate them still gets the browse-by-name
fallback, so a known repository can be opened by typing its name. ECR Public
uses the `ecr-public:` equivalents.

### The token is sent eagerly — the opposite of GHCR

`EcrCredentials::get_authorization` returns `Basic <authorizationToken>` on
every request. The SDK's `authorizationToken` is already the base64 of
`AWS:<password>`, so it is the header value verbatim and is never decoded.

This is deliberately the **inverse** of `GhcrCredentials`, which must return
`None`. The two look like candidates for unification and are not:

| | GHCR | ECR |
|---|---|---|
| Raw credential on `/v2/` | `403 Forbidden` — terminal | Accepted |
| So the credential is | Withheld, then exchanged on the 401 | Sent up front |
| Minted token cached | No — it is repository-scoped | Yes — it is registry-wide |

Caching is safe here precisely because an ECR authorization token covers the
whole registry, so there is no repository-scoped token to leak across
endpoints — the failure mode that shaped the Docker Hub and GHCR paths. The
cached token is re-minted once it comes within five minutes of expiring.

### Anonymous ECR Public

Pulling a public image needs no AWS account. `EcrCredentials` therefore also
implements the challenge path — anonymously, and only for ECR Public — so
`public.ecr.aws/<namespace>/<image>` can be opened by name with no credentials
at all. Private ECR does not implement it: there, a 401 means the AWS
credential itself was refused, and no realm exchange can rescue that.

This is why an ECR Public client is built immediately rather than waiting on
AWS: its endpoint is a constant, so a credential failure costs you the
repository *listing* but not the ability to browse.

## Discovery

ECR has no `/v2/_catalog`. Repositories come from `ecr:DescribeRepositories`,
served from `api.ecr.<region>.amazonaws.com` — a **different origin** from the
registry itself.

`RegistryClient::send` strips `Authorization` from any request that is not
same-origin with its base URL, which is the guard that keeps credentials off
server-supplied blob URLs. Routing an AWS call through the client would mean
carving an exception into exactly that guard, so `registry/ecr.rs` is free
functions with the SDK as their own transport — the same shape, and the same
reasoning, as `registry/ghcr.rs`.

For ECR Public the listing is the set of repositories **you publish**. There is
no API that enumerates the whole public registry, so an empty list is an
ordinary outcome rather than an error.

## Switching account and region

An ECR registry is one AWS account × one region, so "multi-registry" browsing
means switching both. `u` (or `Backspace`) opens a two-stage picker:

```
AWS profile  →  region  →  repository  →  tag
```

- **Profile stage.** Suggestions are parsed from `~/.aws/config` (`[profile x]`)
  and `~/.aws/credentials` (`[x]`), with `default` listed first. Any value can
  be typed, so a profile defined only in the environment still works, and an
  exact match is hoisted above longer names that merely contain it.
- **Region stage.** The region in use leads, then the chosen profile's own
  configured region, then the static region list. `Backspace` on an empty input
  steps back to the profile stage. Skipped entirely for ECR Public.

Nothing is applied until both stages are confirmed — a region alone would point
at a new region of the *old* account. The choice is in-memory only; the app
never writes `config.toml`.

The repository cache is keyed by `(profile, aws_profile, region)`. Keyed by
profile name alone, switching accounts would serve the previous account's
repositories, which reads as an AWS or permissions fault rather than the cache
bug it is.

## Troubleshooting

| Message | Meaning |
|---|---|
| `no AWS credentials for profile "x" — try: aws sso login --profile x` | The credential chain resolved nothing. The SDK's own message lists all six providers it tried; it is replaced with this, because verbatim it fills the pane and buries the point |
| `… — try: aws sso login --profile x` appended | An SSO session exists but has expired |
| `User is not authorized to perform: ecr:DescribeRepositories` | Passed through unchanged. Authorization failures are already specific, and a login hint would send you to fix the wrong thing |
| `Catalog unavailable. Enter repo name to browse:` | Listing failed but browsing may still work — see the permissions table above |

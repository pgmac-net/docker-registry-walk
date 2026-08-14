# AWS ECR browsing (issue #67)

Work record for [#67](https://github.com/pgmac-net/docker-registry-walk/issues/67),
delivered as [PR #115](https://github.com/pgmac-net/docker-registry-walk/pull/115)
on 2026-08-14. For *how to use* the feature, see
[ecr-registry-browsing.md](ecr-registry-browsing.md); this page is the record
of how it was decided and built.

## The ticket

One sentence, no comments: "I also need to be able to browse AWS ECR
multi-registry." Everything below came out of a grilling session, because
almost every load-bearing decision was underspecified — starting with what
"multi-registry" meant.

## Decisions taken during grilling

| Question | Decision | Why |
|---|---|---|
| What is "multi-registry"? | An ECR registry is one AWS **account × region**. Config profiles for the ones you use, *plus* a runtime picker to switch both. | Switching accounts is the point of the ticket; a config-only feature would have missed it. |
| How to get AWS credentials? | `aws-config` + `aws-sdk-ecr` + `aws-sdk-ecrpublic`. | Chosen over shelling out to the `aws` CLI (which was the recommendation, on dependency-weight grounds) and over hand-rolled SigV4. Native SDK, typed pagination, no subprocess. |
| Is ECR Public in scope? | Yes, as its own `type = "ecr-public"`. | Separate AWS service, fixed hostname, no region axis. A boolean on one ECR type would put an `if public` branch in every code path. |
| Which actions must work? | Whatever plain v2 gives, once auth works. No ECR-specific API code. | Keeps the surface honest; a rejected op is a follow-up, not a guess now. |
| Picker shape? | Two stages: AWS profile → region. | Mirrors GHCR's owner picker. `Backspace` on an empty region input steps back a stage. |
| Config shape? | `url` becomes optional and is derived from `GetAuthorizationToken`'s `proxyEndpoint`. | The hostname embeds the account ID; nobody should hand-type that. |
| ECR Public listing? | Your own published repos. | No API enumerates the whole public registry. Anonymous browse-by-name covers everyone else's. |
| Credential failure UX? | Surface the AWS error, never prompt, never touch the keyring. | The registry password is minted and expires in ~12h; there is nothing a user could usefully type. |
| CLI? | `--aws-profile` and `--region`, with `--type ecr` alone enough to build an ad-hoc profile. | Every other type needs `--url`; ECR has none to give. |
| Region suggestions? | Static list + the profile's own region, always typeable. | Enumerating regions needs `ec2:DescribeRegions` — a permission unrelated to reading a registry. |

## Shape of the implementation

The feature is deliberately modelled on GHCR, which had already solved "a
registry with no `/v2/_catalog` whose discovery API lives on another host":

- `src/registry/ecr.rs` — free functions over the AWS SDK, *not* `RegistryClient`
  methods, because `api.ecr.<region>.amazonaws.com` is a different origin and
  `send` strips `Authorization` off-origin. That guard is what keeps credentials
  off server-supplied blob URLs, so it does not get an exception.
- `EcrCredentials` in `auth.rs` — beside the other credential impls, delegating
  all AWS work to `ecr.rs`.
- Two new `Modal` variants sharing `HelpContext::ChoicePicker` and the
  `picker_choices` row builder with the GHCR owner picker.

### The one genuinely new invariant

ECR's credential behaviour is the **inverse** of GHCR's, and the two sit next to
each other in the same file, so the contrast is documented at both ends:

| | GHCR | ECR |
|---|---|---|
| Raw credential on `/v2/` | `403` — terminal | Accepted |
| So it is | withheld, exchanged on the 401 | sent up front |
| Minted token cached | no (repo-scoped) | yes (registry-wide) |

Caching is safe for ECR precisely because its token is registry-wide — the
property that makes the Docker Hub / GHCR scope cascade impossible here.

## Deviations from the approved plan

1. **One PR, not two stacked ones.** The plan proposed landing private ECR
   first and ECR Public second. In practice the two share the type enum, the
   credential impl, the picker and the discovery module; by the time private
   ECR worked, public was a handful of lines. Splitting would have meant
   unpicking tested code for no reviewer benefit.
2. **`reqwest` was left on native-tls.** The plan flagged the two-TLS-stack
   problem and said to switch `reqwest` to `rustls-tls` "if it hurts". It was
   left alone deliberately: rustls-tls uses bundled Mozilla roots instead of the
   system trust store, which would break self-signed and internal-CA registries
   — the Artifactory case this tool exists for. Two stacks is the cheaper cost.
3. **`describe_failure` was rewritten after live testing.** The planned version
   appended an `aws sso login` hint on SSO-ish keywords. Against a real machine
   the dominant failure was an *exhausted credential chain*, whose SDK message
   is six lines of "the credential provider was not enabled" and matches none of
   those keywords. It now replaces that message rather than appending to it.
4. **An anonymous challenge path was added for ECR Public.** Not in the plan.
   Without it the claim that a public image can be browsed by name was false —
   `EcrCredentials` returned `None` on a failed mint and then had nothing to
   answer the registry's 401 with.
5. **`HelpContext::OwnerPicker` was renamed `ChoicePicker`.** Planned, but worth
   recording: three pickers now share it, and the help copy is no longer
   GHCR-specific.

## Bugs found while verifying

Both were found by driving the real TUI under tmux, not by the test suite.

- **Panic on any keystroke before AWS answered.** The event loop indexed
  `clients[&active_name]`, which had always been safe because every profile had
  a client before the first frame. An ECR profile legitimately does not. Fixed
  with `active_client` and an explicit not-connected stand-in on an RFC 2606
  `.invalid` host, so an escaped request fails with a self-describing DNS error
  rather than reaching something real.
- **ECR Public lost its client on a credential failure**, taking the
  browse-by-name fallback with it — despite `public.ecr.aws` being a constant
  that needs no AWS account to read. Its client is now built eagerly.

A third, pre-existing issue surfaced too: the choice pickers filter by
*substring*, so typing the full profile name `pgmac` and pressing Enter selected
`aerofit-pgmac`. An exact match is now hoisted to the top. This fixes the GHCR
owner picker as well.

## Verification

- `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings` clean,
  237 tests passing.
- Live under tmux: anonymous ECR Public browse-by-name, 100 tags and a manifest
  inspect against `public.ecr.aws/docker/library/nginx`; both pickers, the
  back-step, contextual help and its restore; the credential-failure path.
- **Not verified:** the authenticated private-ECR path. No AWS profile on the
  dev machine has ECR access — `default` is denied `ecr:GetAuthorizationToken`
  and the SSO profiles have no live session. Flagged on the PR for a manual
  check before merge.

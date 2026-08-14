use std::collections::HashMap;
use std::time::{Duration, Instant};

use ratatui::widgets::ListState;

use crate::clipboard;
use crate::config::RegistryProfile;
use crate::ops::diff::DiffLayer;
use crate::registry::ArtifactoryRepo;
use crate::registry::ecr::{AWS_REGIONS, AwsProfile};

use super::detail::ImageDetail;
use super::input::InputState;
use super::jsonview::{self, RowMeta};

const STATUS_TTL: Duration = Duration::from_secs(2);
const LOAD_AHEAD: usize = 20;
pub const SPINNER: [char; 6] = ['⠋', '⠙', '⠸', '⠴', '⠦', '⠇'];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Repos,
    Tags,
    Detail,
}

impl Focus {
    pub fn toggle(self) -> Self {
        match self {
            Focus::Repos => Focus::Tags,
            Focus::Tags => Focus::Detail,
            Focus::Detail => Focus::Repos,
        }
    }

    pub fn prev(self) -> Self {
        match self {
            Focus::Repos => Focus::Detail,
            Focus::Tags => Focus::Repos,
            Focus::Detail => Focus::Tags,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoadState {
    Idle,
    Loading,
    Error(String),
}

/// Which retry, if any, produced the in-flight catalog load. See the field
/// doc on `App::catalog_attempt`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CatalogAttempt {
    /// First load for this registry (or repo-key).
    #[default]
    Initial,
    /// Retry after silently re-reading credentials from the keyring.
    AfterReread,
    /// Retry after the user supplied a credential at the prompt.
    AfterCredential,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortOrder {
    NameAsc,
    NameDesc,
}

impl SortOrder {
    pub fn cycle(self) -> Self {
        match self {
            Self::NameAsc => Self::NameDesc,
            Self::NameDesc => Self::NameAsc,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::NameAsc => "↑ name",
            Self::NameDesc => "↓ name",
        }
    }
}

/// In-modal text search over the inspect JSON.
#[derive(Debug, Default)]
pub struct SearchState {
    /// True while the user is typing a query (keys route to `input`).
    pub active: bool,
    pub input: InputState,
    /// Absolute line indices of the last committed query's matches.
    pub matches: Vec<usize>,
    /// Index into `matches` of the currently-focused hit.
    pub current: usize,
    /// The committed query text (for highlighting), empty when none.
    pub query: String,
}

/// The Inspect modal: a navigable viewer over pretty-printed manifest +
/// config JSON with collapsible nodes, paging, and text search.
///
/// `cursor` and `scroll` index into `visible` (the currently-shown subset
/// of line indices), not into `lines` directly. `viewport_h` is written by
/// the renderer each frame so paging can step by a screenful.
#[derive(Debug)]
pub struct InspectModal {
    pub title: String,
    pub lines: Vec<String>,
    pub rows: Vec<RowMeta>,
    pub collapsed: Vec<bool>,
    pub visible: Vec<usize>,
    pub cursor: usize,
    pub scroll: usize,
    pub viewport_h: usize,
    pub search: SearchState,
}

impl InspectModal {
    pub fn new(title: String, lines: Vec<String>) -> Self {
        let rows = jsonview::build_rows(&lines);
        let collapsed = vec![false; lines.len()];
        let visible = jsonview::visible_lines(&rows, &collapsed);
        Self {
            title,
            lines,
            rows,
            collapsed,
            visible,
            cursor: 0,
            scroll: 0,
            viewport_h: 1,
            search: SearchState::default(),
        }
    }

    /// Absolute line index under the cursor.
    pub fn cursor_line(&self) -> usize {
        self.visible.get(self.cursor).copied().unwrap_or(0)
    }

    /// Recompute the visible set after a fold change, keeping the cursor on
    /// the same absolute line where possible.
    fn rebuild_visible(&mut self) {
        let anchor = self.cursor_line();
        self.visible = jsonview::visible_lines(&self.rows, &self.collapsed);
        self.cursor = self
            .visible
            .iter()
            .position(|&l| l >= anchor)
            .unwrap_or(self.visible.len().saturating_sub(1));
        self.ensure_visible();
    }

    /// Scroll so the cursor row sits within the viewport.
    fn ensure_visible(&mut self) {
        let h = self.viewport_h.max(1);
        if self.cursor < self.scroll {
            self.scroll = self.cursor;
        } else if self.cursor >= self.scroll + h {
            self.scroll = self.cursor + 1 - h;
        }
        let max_scroll = self.visible.len().saturating_sub(h);
        self.scroll = self.scroll.min(max_scroll);
    }

    /// Record the rendered content height and re-clamp scroll to it.
    pub fn set_viewport(&mut self, h: usize) {
        self.viewport_h = h.max(1);
        self.ensure_visible();
    }

    pub fn move_cursor(&mut self, delta: isize) {
        let last = self.visible.len().saturating_sub(1);
        let next = (self.cursor as isize + delta).clamp(0, last as isize);
        self.cursor = next as usize;
        self.ensure_visible();
    }

    pub fn page(&mut self, pages: isize) {
        let step = self.viewport_h.max(1) as isize;
        self.move_cursor(pages * step);
    }

    pub fn jump_top(&mut self) {
        self.cursor = 0;
        self.ensure_visible();
    }

    pub fn jump_bottom(&mut self) {
        self.cursor = self.visible.len().saturating_sub(1);
        self.ensure_visible();
    }

    /// Toggle the fold of the node at (or enclosing) the cursor.
    pub fn toggle_fold(&mut self) {
        if let Some(o) = jsonview::opener_at(&self.rows, self.cursor_line()) {
            self.collapsed[o] = !self.collapsed[o];
            // Keep the cursor on the opener so repeated toggles are stable.
            self.rebuild_visible();
            if let Some(pos) = self.visible.iter().position(|&l| l == o) {
                self.cursor = pos;
                self.ensure_visible();
            }
        }
    }

    /// Collapse (`true`) or expand (`false`) the node at the cursor.
    pub fn set_fold(&mut self, collapse: bool) {
        if let Some(o) = jsonview::opener_at(&self.rows, self.cursor_line())
            && self.collapsed[o] != collapse
        {
            self.collapsed[o] = collapse;
            self.rebuild_visible();
            if let Some(pos) = self.visible.iter().position(|&l| l == o) {
                self.cursor = pos;
                self.ensure_visible();
            }
        }
    }

    pub fn collapse_all(&mut self) {
        for (i, r) in self.rows.iter().enumerate() {
            if r.opener {
                self.collapsed[i] = true;
            }
        }
        self.rebuild_visible();
    }

    pub fn expand_all(&mut self) {
        for c in &mut self.collapsed {
            *c = false;
        }
        self.rebuild_visible();
    }

    pub fn start_search(&mut self) {
        self.search.active = true;
        self.search.input.start("");
    }

    pub fn cancel_search(&mut self) {
        self.search.active = false;
    }

    /// Commit the typed query: compute matches and jump to the first hit.
    pub fn commit_search(&mut self) {
        self.search.active = false;
        let q = self.search.input.buffer.clone();
        self.search.matches = jsonview::find_matches(&self.lines, &q);
        self.search.query = q;
        self.search.current = 0;
        if let Some(&line) = self.search.matches.first() {
            self.jump_to(line);
        }
    }

    pub fn next_match(&mut self) {
        if self.search.matches.is_empty() {
            return;
        }
        self.search.current = (self.search.current + 1) % self.search.matches.len();
        self.jump_to(self.search.matches[self.search.current]);
    }

    pub fn prev_match(&mut self) {
        if self.search.matches.is_empty() {
            return;
        }
        let n = self.search.matches.len();
        self.search.current = (self.search.current + n - 1) % n;
        self.jump_to(self.search.matches[self.search.current]);
    }

    /// Move the cursor to an absolute line, expanding any folds hiding it.
    fn jump_to(&mut self, line: usize) {
        jsonview::expand_ancestors(&self.rows, &mut self.collapsed, line);
        self.visible = jsonview::visible_lines(&self.rows, &self.collapsed);
        if let Some(pos) = self.visible.iter().position(|&l| l == line) {
            self.cursor = pos;
        }
        self.ensure_visible();
    }

    pub fn has_matches(&self) -> bool {
        !self.search.matches.is_empty()
    }
}

#[derive(Debug)]
pub struct LayerDiffModal {
    pub tag_a: String,
    pub tag_b: String,
    pub layers: Vec<DiffLayer>,
    pub scroll: usize,
}

/// Which set of keybindings the Help pane shows.
///
/// Stored on `Modal::Help` at the moment it opens rather than re-derived from
/// `app` at render time — `draw_help_modal` (and the key handler) only see
/// `&app.modal`, since Help itself is one of `Modal`'s variants, so recovering
/// "what was open before Help" would need state this enum already is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HelpContext {
    /// Not inside any modal; which panel has focus decides the sections shown,
    /// matching the footer bar, which already varies with focus.
    Normal(Focus),
    Inspect,
    /// Docker Hub's server-side search: `↑↓` only, since `j`/`k` insert text.
    SearchPicker,
    /// Artifactory repo-keys or GHCR packages: fetched once, filtered locally.
    FilterPicker,
    /// A picker where any typed value is also selectable, not just the
    /// suggestions — the GHCR owner picker and both ECR pickers. Worth its own
    /// context because "type something that isn't listed" is the one binding a
    /// `FilterPicker` does not have.
    ChoicePicker,
    RegistrySelect,
    LayerDiff,
}

/// Which keybinding context applies to `modal`, given the current `focus`.
///
/// Pure, so the mapping is testable without rendering — and is the single
/// source of truth for what `?` opens, since the same modal can only mean one
/// context.
pub fn help_context_for(modal: &Modal, focus: Focus) -> HelpContext {
    match modal {
        Modal::Inspect(_) => HelpContext::Inspect,
        Modal::SearchPicker { .. } => HelpContext::SearchPicker,
        Modal::ArtifactoryPicker { .. } | Modal::GhcrPicker { .. } => HelpContext::FilterPicker,
        Modal::GhcrOwnerPicker { .. }
        | Modal::EcrProfilePicker { .. }
        | Modal::EcrRegionPicker { .. } => HelpContext::ChoicePicker,
        Modal::RegistrySelect { .. } => HelpContext::RegistrySelect,
        Modal::LayerDiff(_) => HelpContext::LayerDiff,
        // Confirm, Input and Help itself have no keybinding context of their
        // own worth switching to: Confirm/Input already show their keys
        // inline, and `?` cannot reach Help from Help. Normal is the sensible
        // fallback for all three, and for `None`.
        _ => HelpContext::Normal(focus),
    }
}

#[derive(Debug)]
pub enum Modal {
    None,
    Confirm {
        message: String,
        on_confirm: ConfirmAction,
    },
    Input {
        prompt: String,
        input: InputState,
        on_confirm: InputAction,
    },
    RegistrySelect {
        selected_idx: usize,
    },
    Inspect(Box<InspectModal>),
    LayerDiff(Box<LayerDiffModal>),
    Help {
        scroll: usize,
        context: HelpContext,
    },
    /// Docker Hub repository search with live results.
    SearchPicker {
        input: InputState,
        results: Vec<String>,
        selected: usize,
        searching: bool,
    },
    /// Pick which Docker repo-key to browse on a JFrog Artifactory
    /// instance. `repos` is fetched once (via `/api/repositories`) and
    /// filtered locally as the user types, unlike `SearchPicker`'s
    /// incremental server-side search.
    ArtifactoryPicker {
        filter: InputState,
        repos: Vec<ArtifactoryRepo>,
        selected: usize,
        loading: bool,
    },
    /// Pick which GHCR package to browse. Fetched once from the GitHub
    /// packages API and filtered locally, like `ArtifactoryPicker` — but the
    /// entries are ordinary repository names, not sub-registries, so the same
    /// list also populates the Repos pane (see `App::on_ghcr_packages`).
    GhcrPicker {
        filter: InputState,
        packages: Vec<String>,
        selected: usize,
        loading: bool,
    },
    /// Pick whose GHCR packages to browse — one level *above* `GhcrPicker`,
    /// since GHCR's hierarchy is owner → package → tag.
    ///
    /// `owners` is only ever a suggestion list; the typed input is always
    /// offered as a choice too, so an owner nobody could enumerate (an org the
    /// token cannot see, or any account at all when browsing anonymously)
    /// stays reachable. See [`picker_choices`].
    GhcrOwnerPicker {
        input: InputState,
        owners: Vec<String>,
        selected: usize,
        loading: bool,
    },
    /// Pick which AWS named profile an ECR registry is browsed as — the first
    /// of ECR's two switchable axes, and the one that decides the account.
    ///
    /// Suggestions are parsed from the user's AWS config files; as with
    /// [`Modal::GhcrOwnerPicker`], the typed value is always selectable too, so
    /// a profile defined somewhere this does not look at (or supplied purely
    /// through the environment) stays reachable.
    EcrProfilePicker {
        input: InputState,
        profiles: Vec<String>,
        selected: usize,
    },
    /// Pick which region's registry to browse — the second axis, opened once a
    /// profile has been chosen.
    ///
    /// `aws_profile` is the pick being carried over from
    /// [`Modal::EcrProfilePicker`]: an ECR registry is an account *and* a
    /// region, so neither is applied until both are known. Not shown at all for
    /// ECR Public, which is a single global registry.
    EcrRegionPicker {
        input: InputState,
        regions: Vec<String>,
        selected: usize,
        aws_profile: Option<String>,
    },
}

/// One row of the GHCR owner picker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PickerChoice {
    /// The text the user typed, offered because it matches no known owner.
    Typed(String),
    /// A suggestion fetched from the GitHub API, or one seen earlier.
    Listed(String),
}

impl PickerChoice {
    /// What the row shows. The typed row is labelled rather than shown bare so
    /// it reads as an action instead of looking like another suggestion.
    pub fn label(&self) -> String {
        match self {
            Self::Typed(owner) => format!("Use \"{owner}\""),
            Self::Listed(owner) => owner.clone(),
        }
    }

    /// The owner this row selects.
    pub fn owner(&self) -> &str {
        match self {
            Self::Typed(owner) | Self::Listed(owner) => owner,
        }
    }
}

/// A choice picker's rows for a given input and suggestion list — the GHCR
/// owner picker and both ECR pickers.
///
/// Pure, and the single source of truth for both rendering and selection —
/// computing the rows twice is how a picker ends up opening the row above the
/// one that was highlighted.
///
/// The typed value leads the list whenever it is non-empty and doesn't already
/// name a suggestion, so `Enter` on a freshly typed value does the obvious
/// thing. Suppressing it on an exact (case-insensitive) match avoids offering
/// `Use "pgmac"` directly above `pgmac` — and in that case the exact match is
/// hoisted to the top instead, because the filter is a *substring* match and
/// would otherwise rank a longer name first: typing the full name `pgmac` and
/// pressing Enter would select `aerofit-pgmac`, which merely contains it.
pub fn picker_choices(input: &str, owners: &[String]) -> Vec<PickerChoice> {
    let typed = input.trim();
    let needle = typed.to_lowercase();

    let exact = owners.iter().find(|o| o.to_lowercase() == needle);

    let mut rows = Vec::new();
    match exact {
        Some(matched) if !typed.is_empty() => rows.push(PickerChoice::Listed(matched.clone())),
        _ if !typed.is_empty() => rows.push(PickerChoice::Typed(typed.to_owned())),
        _ => {}
    }
    rows.extend(
        owners
            .iter()
            .filter(|o| needle.is_empty() || o.to_lowercase().contains(&needle))
            .filter(|o| exact.is_none_or(|matched| *o != matched))
            .cloned()
            .map(PickerChoice::Listed),
    );
    rows
}

/// Region suggestions for the ECR region picker.
///
/// The two regions that are actually likely lead the list — the one already in
/// use, then the chosen AWS profile's configured default — followed by the
/// static [`AWS_REGIONS`] catalogue with those two removed so neither appears
/// twice. Pure, so the ordering is testable without a picker.
pub fn ecr_region_suggestions(profile_region: Option<&str>, current: Option<&str>) -> Vec<String> {
    let mut regions: Vec<String> = Vec::new();

    for preferred in [current, profile_region].into_iter().flatten() {
        if !preferred.is_empty() && !regions.iter().any(|r| r == preferred) {
            regions.push(preferred.to_owned());
        }
    }

    let preferred = regions.clone();
    regions.extend(
        AWS_REGIONS
            .iter()
            .filter(|r| !preferred.iter().any(|seen| seen == *r))
            .map(|r| (*r).to_owned()),
    );

    regions
}

#[derive(Debug, Clone)]
pub enum ConfirmAction {
    DeleteManifest { repo: String, tag: String },
    PruneDigestTags { repo: String, tags: Vec<String> },
}

#[derive(Debug, Clone)]
pub enum InputAction {
    CopyImage {
        src_repo: String,
        src_tag: String,
    },
    Retag {
        repo: String,
        src_tag: String,
    },
    Export {
        repo: String,
        tag: String,
    },
    DiffAgainst {
        repo: String,
        tag_a: String,
    },
    /// User typed a repo name directly (e.g. after catalog failure).
    BrowseRepo,
    /// User entered a password after auth failure.
    EnterPassword {
        profile_name: String,
        username: String,
    },
    /// User entered an access token after auth failure. Carries no username —
    /// token auth does not need one.
    EnterToken {
        profile_name: String,
    },
}

impl InputAction {
    /// Whether this action collects a credential, so the input modal must
    /// mask what it echoes.
    ///
    /// Deriving masking from the action (rather than a flag on `Modal::Input`)
    /// means none of the modal's construction sites can forget to set it, and
    /// a new credential-collecting action only has to be listed here.
    pub fn is_secret(&self) -> bool {
        matches!(self, Self::EnterPassword { .. } | Self::EnterToken { .. })
    }
}

#[derive(Debug)]
struct StatusMessage {
    text: String,
    expires_at: Instant,
}

pub struct App {
    pub focus: Focus,
    pub filter_mode: Option<Focus>,
    // Displayed (filtered/sorted) lists
    pub repos: Vec<String>,
    pub repos_state: ListState,
    pub tags: Vec<String>,
    pub tags_state: ListState,
    // Raw loaded data
    repos_all: Vec<String>,
    tags_all: Vec<String>,
    // Filters
    pub repo_filter: String,
    pub tag_filter: String,
    // Pagination
    pub repos_cursor: Option<String>,
    pub repos_has_more: bool,
    pub tags_cursor: Option<String>,
    pub tags_has_more: bool,
    pub current_repo: Option<String>,
    // Load state
    pub repo_load: LoadState,
    pub tag_load: LoadState,
    pub detail_load: LoadState,
    // Sort
    pub tag_sort: SortOrder,
    // Detail panel
    pub detail: Option<ImageDetail>,
    pub detail_scroll: usize,
    pub current_tag: Option<String>,
    // Display
    pub registry_name: String,
    pub registry_url: String,
    pub modal: Modal,
    /// The modal stashed while the Help overlay is shown over it, so closing
    /// Help returns to it exactly where it was left — including, for a
    /// picker, whatever filter text and selection had already been typed.
    /// `Box` because `Modal` embeds `Self` (e.g. `LayerDiff`), and boxing here
    /// keeps `Modal` from needing to be recursively sized.
    pub help_return: Option<Box<Modal>>,
    pub should_quit: bool,
    pub spinner_tick: usize,
    /// Which retry, if any, produced the catalog load currently in flight.
    ///
    /// A 401 on `Initial` means the cached client's credentials might simply
    /// predate something now in the keyring (another process wrote it, or it
    /// was unlocked after startup) — worth one silent re-read before bothering
    /// the user. A 401 on `AfterReread` or `AfterCredential` means that
    /// re-read (or a prompt) already ran and failed, so it opens BrowseRepo /
    /// the credential prompt instead of retrying again.
    pub catalog_attempt: CatalogAttempt,
    status: Option<StatusMessage>,
    // Registry switcher
    pub profiles: Vec<RegistryProfile>,
    pub active_profile_idx: usize,
    /// Last-fetched repo-key list per Artifactory profile (by profile name),
    /// so re-opening the picker via up-navigation doesn't wait on a fetch.
    artifactory_repo_cache: HashMap<String, Vec<ArtifactoryRepo>>,
    /// Repo-key currently being browsed, when the active registry is an
    /// Artifactory repo-key. Used to preselect it in the picker.
    current_artifactory_repo_key: Option<String>,
    /// Last-fetched package list per GHCR profile **and owner**, so re-opening
    /// the picker doesn't wait on the GitHub API again.
    ///
    /// Keyed by owner as well as profile name because the owner is now
    /// switchable at runtime: keyed by profile alone, re-opening the picker
    /// after an owner change would serve the *previous* owner's packages. A
    /// tuple rather than a `<profile>#<owner>` string, to stay clear of the `#`
    /// that `Config::validate` reserves for client-cache keys.
    ghcr_package_cache: HashMap<(String, Option<String>), Vec<String>>,
    /// Last-fetched owner suggestions per GHCR profile (by profile name).
    ghcr_owner_cache: HashMap<String, Vec<String>>,
    /// Last-fetched repository list per ECR profile **and AWS target**.
    ///
    /// Keyed `(profile, aws_profile, region)` for the same reason
    /// `ghcr_package_cache` is keyed by owner: both axes are switchable at
    /// runtime, so a cache keyed by profile name alone would serve the previous
    /// account's repositories after a switch — which presents as an AWS or
    /// permissions fault rather than as the cache bug it is. A tuple, not a
    /// `#`-joined string, to stay clear of the client-cache separator.
    ecr_repo_cache: HashMap<(String, Option<String>, Option<String>), Vec<String>>,
    /// AWS named profiles parsed from the user's config files, cached for the
    /// life of the session — they are read from disk, not fetched, and do not
    /// change under a running TUI often enough to be worth re-reading.
    aws_profile_cache: Vec<AwsProfile>,
}

impl App {
    pub fn new(profiles: Vec<RegistryProfile>, initial_idx: usize) -> Self {
        let mut repos_state = ListState::default();
        repos_state.select(Some(0));
        let idx = initial_idx.min(profiles.len().saturating_sub(1));
        let registry_name = profiles
            .get(idx)
            .map(|p| p.name.clone())
            .unwrap_or_default();
        let registry_url = profiles
            .get(idx)
            .map(|p| p.display_url())
            .unwrap_or_default();
        Self {
            focus: Focus::Repos,
            filter_mode: None,
            repos: Vec::new(),
            repos_state,
            tags: Vec::new(),
            tags_state: ListState::default(),
            repos_all: Vec::new(),
            tags_all: Vec::new(),
            repo_filter: String::new(),
            tag_filter: String::new(),
            repos_cursor: None,
            repos_has_more: false,
            tags_cursor: None,
            tags_has_more: false,
            current_repo: None,
            repo_load: LoadState::Idle,
            tag_load: LoadState::Idle,
            detail_load: LoadState::Idle,
            tag_sort: SortOrder::NameAsc,
            detail: None,
            detail_scroll: 0,
            current_tag: None,
            registry_name,
            registry_url,
            modal: Modal::None,
            help_return: None,
            should_quit: false,
            spinner_tick: 0,
            catalog_attempt: CatalogAttempt::Initial,
            status: None,
            profiles,
            active_profile_idx: idx,
            artifactory_repo_cache: HashMap::new(),
            current_artifactory_repo_key: None,
            ghcr_package_cache: HashMap::new(),
            ghcr_owner_cache: HashMap::new(),
            ecr_repo_cache: HashMap::new(),
            aws_profile_cache: Vec::new(),
        }
    }

    // ------------------------------------------------------------------
    // Page arrival
    // ------------------------------------------------------------------

    pub fn on_repos_page(&mut self, repos: Vec<String>, has_more: bool) {
        self.repos_has_more = has_more;
        self.repos_all.extend(repos);
        self.repos_cursor = self.repos_all.last().cloned();
        self.repo_load = LoadState::Idle;
        self.apply_repo_filter();
    }

    pub fn on_repos_error(&mut self, msg: String, show_browse: bool) {
        let is_dh = self
            .profiles
            .get(self.active_profile_idx)
            .is_some_and(|p| p.is_dockerhub());
        let display = if is_dh {
            "We can't show you these, please search for an image".to_owned()
        } else {
            msg.clone()
        };
        self.repo_load = LoadState::Error(display);
        self.set_status(format!("Repos error: {msg}"));
        if show_browse && matches!(self.modal, Modal::None) {
            if is_dh {
                self.modal = Modal::SearchPicker {
                    input: InputState::default(),
                    results: Vec::new(),
                    selected: 0,
                    searching: false,
                };
            } else {
                self.modal = Modal::Input {
                    prompt: "Catalog unavailable. Enter repo name to browse:".to_owned(),
                    input: InputState::default(),
                    on_confirm: InputAction::BrowseRepo,
                };
            }
        }
    }

    pub fn on_tags_page(&mut self, repo: String, tags: Vec<String>, has_more: bool) {
        if self.current_repo.as_deref() != Some(&repo) {
            return;
        }
        let was_empty = self.tags_all.is_empty();
        self.tags_has_more = has_more;
        self.tags_all.extend(tags);
        self.tags_cursor = self.tags_all.last().cloned();
        self.tag_load = LoadState::Idle;
        self.apply_tag_filter_sort();
        if was_empty && !self.tags.is_empty() {
            self.tags_state.select(Some(0));
        }
    }

    pub fn on_tags_error(&mut self, msg: String) {
        self.tag_load = LoadState::Error(msg.clone());
        self.set_status(format!("Tags error: {msg}"));
    }

    // ------------------------------------------------------------------
    // Tag loading lifecycle
    // ------------------------------------------------------------------

    pub fn start_detail_load(&mut self, tag: String) {
        self.current_tag = Some(tag);
        self.detail = None;
        self.detail_scroll = 0;
        self.detail_load = LoadState::Loading;
    }

    pub fn on_detail_loaded(&mut self, repo: String, tag: String, detail: ImageDetail) {
        if self.current_repo.as_deref() == Some(&repo) && self.current_tag.as_deref() == Some(&tag)
        {
            self.detail = Some(detail);
            self.detail_load = LoadState::Idle;
        }
    }

    pub fn on_detail_error(&mut self, msg: String) {
        self.detail_load = LoadState::Error(msg.clone());
        self.set_status(format!("Detail error: {msg}"));
    }

    pub fn start_tags_load(&mut self, repo: String) {
        self.current_repo = Some(repo);
        self.tags_all.clear();
        self.tags.clear();
        self.tags_state.select(None);
        self.tags_cursor = None;
        self.tags_has_more = false;
        self.tag_filter.clear();
        self.tag_load = LoadState::Loading;
        self.detail = None;
        self.detail_load = LoadState::Idle;
        self.current_tag = None;
    }

    /// Clear all repo/tag/detail state, keeping neither list nor selection
    /// from the previous registry (or Artifactory repo-key).
    fn reset_for_new_registry(&mut self) {
        self.repos_all.clear();
        self.repos.clear();
        self.repos_state.select(Some(0));
        self.repos_cursor = None;
        self.repos_has_more = false;
        self.repo_filter.clear();

        self.tags_all.clear();
        self.tags.clear();
        self.tags_state.select(None);
        self.tags_cursor = None;
        self.tags_has_more = false;
        self.tag_filter.clear();
        self.tag_load = LoadState::Idle;

        self.current_repo = None;
        self.current_tag = None;
        self.detail = None;
        self.detail_load = LoadState::Idle;
        self.detail_scroll = 0;
        self.focus = Focus::Repos;
        self.filter_mode = None;
        self.catalog_attempt = CatalogAttempt::Initial;
        self.current_artifactory_repo_key = None;
    }

    /// Clear all repo/tag/detail state when switching registries.
    pub fn start_registry_switch(&mut self, idx: usize) {
        self.active_profile_idx = idx;
        let profile = &self.profiles[idx];
        self.registry_name = profile.name.clone();
        self.registry_url = profile.display_url();
        self.reset_for_new_registry();
        self.repo_load = LoadState::Loading;
    }

    /// Reload the catalog in place after a credential change (a silent
    /// keyring re-read or a prompt).
    ///
    /// Deliberately narrower than [`Self::start_registry_switch`], which goes
    /// through `reset_for_new_registry` and so clears
    /// `current_artifactory_repo_key`. Using that here would visually eject the
    /// user back to the repo-key picker on a *successful* re-auth, while the
    /// client stayed scoped to the repo-key they were browsing.
    ///
    /// Does not touch `catalog_attempt` — the caller sets it to whichever
    /// retry stage this reload is for, since `restart_catalog_load` itself
    /// has no way to know which one that is.
    pub fn restart_catalog_load(&mut self) {
        self.repos_all.clear();
        self.repos.clear();
        self.repos_state.select(Some(0));
        self.repos_cursor = None;
        self.repos_has_more = false;
        self.repo_filter.clear();

        self.tags_all.clear();
        self.tags.clear();
        self.tags_state.select(None);
        self.tags_cursor = None;
        self.tags_has_more = false;
        self.tag_filter.clear();
        self.tag_load = LoadState::Idle;

        self.current_repo = None;
        self.current_tag = None;
        self.detail = None;
        self.detail_load = LoadState::Idle;
        self.detail_scroll = 0;
        self.focus = Focus::Repos;
        self.filter_mode = None;

        self.repo_load = LoadState::Loading;
    }

    /// Switch to a JFrog Artifactory profile: clear repo/tag/detail state
    /// and open the repo-key picker instead of fetching a catalog directly
    /// (an Artifactory instance's base URL isn't itself a `/v2/` root).
    pub fn start_artifactory_switch(&mut self, idx: usize) {
        self.active_profile_idx = idx;
        let profile = &self.profiles[idx];
        self.registry_name = profile.name.clone();
        self.registry_url = profile.display_url();
        self.reset_for_new_registry();
        self.repo_load = LoadState::Idle;
        self.modal = Modal::ArtifactoryPicker {
            filter: InputState::default(),
            repos: Vec::new(),
            selected: 0,
            loading: true,
        };
    }

    /// Fill in the repo-key list once `/api/repositories` returns. Also
    /// refreshes the profile's cache, so the next up-navigation open has
    /// this list ready instantly.
    pub fn on_artifactory_repos(&mut self, repos: Vec<ArtifactoryRepo>) {
        let profile_name = self.profiles[self.active_profile_idx].name.clone();
        self.artifactory_repo_cache
            .insert(profile_name, repos.clone());

        let current_key = self.current_artifactory_repo_key.clone();
        if let Modal::ArtifactoryPicker {
            repos: r,
            selected,
            loading,
            ..
        } = &mut self.modal
        {
            *r = repos;
            *loading = false;
            let len = r.len();
            *selected = if len == 0 {
                0
            } else if let Some(idx) = current_key
                .as_ref()
                .and_then(|key| r.iter().position(|repo| &repo.key == key))
            {
                idx
            } else {
                (*selected).min(len - 1)
            };
        }
    }

    /// Re-open the repo-key picker for the active Artifactory profile using
    /// the cached list from the last fetch, so it appears instantly instead
    /// of waiting on a network round-trip. The caller is expected to spawn a
    /// background refetch (see `on_artifactory_repos`) to keep it current.
    ///
    /// Unlike `start_artifactory_switch`, this does not touch repo/tag/detail
    /// state — `Esc` from the picker returns to browsing exactly as it was.
    pub fn open_artifactory_picker_cached(&mut self) {
        let profile_name = &self.profiles[self.active_profile_idx].name;
        let cached = self
            .artifactory_repo_cache
            .get(profile_name)
            .cloned()
            .unwrap_or_default();
        let selected = self
            .current_artifactory_repo_key
            .as_ref()
            .and_then(|key| cached.iter().position(|r| &r.key == key))
            .unwrap_or(0);
        self.modal = Modal::ArtifactoryPicker {
            filter: InputState::default(),
            repos: cached,
            selected,
            loading: true,
        };
    }

    /// Cache key for the active profile's package list: the profile *and* the
    /// owner it is currently pointed at, since the owner is switchable.
    fn ghcr_cache_key(&self) -> (String, Option<String>) {
        let profile = &self.profiles[self.active_profile_idx];
        (profile.name.clone(), profile.owner.clone())
    }

    /// The owner whose packages are currently listed, if one is set.
    pub fn ghcr_owner(&self) -> Option<&str> {
        self.profiles[self.active_profile_idx].owner.as_deref()
    }

    /// Switch to a GHCR profile: clear repo/tag/detail state and open the
    /// package picker, since GHCR serves no `/v2/_catalog` to fetch instead.
    pub fn start_ghcr_switch(&mut self, idx: usize) {
        self.active_profile_idx = idx;
        let profile = &self.profiles[idx];
        self.registry_name = profile.name.clone();
        self.registry_url = profile.display_url();
        self.reset_for_new_registry();
        // Loading, not Idle: the same fetch fills the Repos pane behind the
        // picker, so the pane must not read as an empty result until it lands.
        self.repo_load = LoadState::Loading;
        self.modal = Modal::GhcrPicker {
            filter: InputState::default(),
            packages: Vec::new(),
            selected: 0,
            loading: true,
        };
    }

    /// Fill in the package list once the GitHub packages API returns.
    ///
    /// One fetch serves two surfaces. A GHCR package is an ordinary
    /// repository — unlike an Artifactory repo-key, which is a whole
    /// sub-registry whose catalog is fetched afterwards — so the list is both
    /// the picker's contents *and* the Repos pane's contents. Populating only
    /// the picker would leave the pane empty behind it, with nothing to return
    /// to on `Esc`.
    pub fn on_ghcr_packages(&mut self, packages: Vec<String>, truncated: bool) {
        self.ghcr_package_cache
            .insert(self.ghcr_cache_key(), packages.clone());

        if let Modal::GhcrPicker {
            packages: p,
            selected,
            loading,
            ..
        } = &mut self.modal
        {
            *p = packages.clone();
            *loading = false;
            let len = p.len();
            *selected = if len == 0 {
                0
            } else {
                (*selected).min(len - 1)
            };
        }

        // `has_more = false`: the GitHub API pagination is followed to
        // completion inside the fetch, so there is no cursor for the Repos
        // pane to continue from.
        self.on_repos_page(packages, false);

        if truncated {
            self.set_status("Package list truncated — refine with the picker filter");
        }
    }

    /// Report a failed package listing.
    ///
    /// Discovery is the only thing that fails here — browsing a *known* GHCR
    /// repository needs no GitHub API call at all, and works even anonymously.
    /// So when there is nothing to show, this hands over to the same
    /// browse-by-name fallback a missing catalog uses, rather than leaving an
    /// empty picker with no way forward. That is the whole experience for an
    /// anonymous profile, which cannot list packages at all: GitHub exposes no
    /// unauthenticated package listing, even for public packages.
    ///
    /// A cached list survives instead: a refetch failing mid-browse should not
    /// throw away packages the user is still picking from.
    pub fn on_ghcr_packages_error(&mut self, msg: String) {
        let has_cached = matches!(
            &self.modal,
            Modal::GhcrPicker { packages, .. } if !packages.is_empty()
        );

        if has_cached {
            if let Modal::GhcrPicker { loading, .. } = &mut self.modal {
                *loading = false;
            }
            self.set_status(format!("GHCR packages error: {msg}"));
            return;
        }

        // `on_repos_error` only opens the fallback over `Modal::None`, and it
        // also moves the Repos pane out of the `Loading` state
        // `start_ghcr_switch` set — otherwise the pane spins forever with the
        // reason visible only until the status message times out.
        self.modal = Modal::None;
        self.on_repos_error(msg, true);
    }

    // ------------------------------------------------------------------
    // GHCR owner picker
    // ------------------------------------------------------------------

    /// Open the owner picker, seeded from cache so it appears instantly.
    ///
    /// Leaves repo/tag/detail state alone — `Esc` returns to browsing exactly
    /// as it was, and nothing changes until an owner is actually chosen.
    pub fn open_ghcr_owner_picker(&mut self) {
        let profile_name = self.profiles[self.active_profile_idx].name.clone();
        let owners = self.ghcr_known_owners(&profile_name);
        let selected = self
            .ghcr_owner()
            .and_then(|current| owners.iter().position(|o| o == current))
            .unwrap_or(0);

        self.modal = Modal::GhcrOwnerPicker {
            input: InputState::default(),
            owners,
            selected,
            loading: true,
        };
    }

    /// Suggestions to show before (or without) a successful API call: whatever
    /// was last fetched, plus the owner currently configured.
    ///
    /// Including the current owner matters for a token that cannot call
    /// `/user/orgs` — an owner set in config, or reached by typing it earlier,
    /// stays one keystroke away instead of having to be retyped.
    fn ghcr_known_owners(&self, profile_name: &str) -> Vec<String> {
        let mut owners = self
            .ghcr_owner_cache
            .get(profile_name)
            .cloned()
            .unwrap_or_default();
        if let Some(current) = self.ghcr_owner()
            && !owners.iter().any(|o| o.eq_ignore_ascii_case(current))
        {
            owners.insert(0, current.to_owned());
        }
        owners
    }

    /// Merge fetched suggestions into the open picker.
    pub fn on_ghcr_owners(&mut self, owners: Vec<String>) {
        let profile_name = self.profiles[self.active_profile_idx].name.clone();
        self.ghcr_owner_cache
            .insert(profile_name.clone(), owners.clone());

        let merged = self.ghcr_known_owners(&profile_name);
        // Re-derive the highlight rather than clamping the old index: the list
        // arriving is what makes the current owner *findable*, and before it
        // lands the picker holds at most that one entry. Clamping instead
        // dropped the highlight onto row 0 the moment the fetch returned.
        let current = self.ghcr_owner().map(str::to_owned);
        if let Modal::GhcrOwnerPicker {
            input,
            owners: o,
            selected,
            loading,
        } = &mut self.modal
        {
            *loading = false;
            *selected = current
                .as_deref()
                .filter(|_| input.buffer.is_empty())
                .and_then(|c| merged.iter().position(|o| o == c))
                .unwrap_or(0);
            *o = merged;
        }
    }

    /// Report a failed suggestion fetch.
    ///
    /// Only the spinner stops. The suggestions are a convenience, not the
    /// input — the typed owner is always selectable — and `/user/orgs` needs
    /// the `read:org` scope that a `read:packages`-only PAT lacks, so an empty
    /// list here is an ordinary outcome rather than a fault worth a modal.
    pub fn on_ghcr_owners_error(&mut self, msg: String) {
        if let Modal::GhcrOwnerPicker { loading, .. } = &mut self.modal {
            *loading = false;
        }
        self.set_status(format!("GHCR owners unavailable: {msg} — type an owner"));
    }

    /// The owner picker's rows for its current input.
    pub fn ghcr_owner_rows(&self) -> Vec<PickerChoice> {
        let Modal::GhcrOwnerPicker { input, owners, .. } = &self.modal else {
            return Vec::new();
        };
        picker_choices(&input.buffer, owners)
    }

    /// Point the active GHCR profile at `owner` and start over beneath it.
    ///
    /// The change is in-memory only: the app never writes `config.toml`, and
    /// rewriting a user's config on a keystroke would be surprising.
    pub fn apply_ghcr_owner(&mut self, owner: String) {
        self.profiles[self.active_profile_idx].owner = Some(owner);
        self.reset_for_new_registry();
        // Same reasoning as `start_ghcr_switch`: the pane fills from this same
        // fetch, so it must not read as an empty result while it is in flight.
        self.repo_load = LoadState::Loading;

        let cached = self
            .ghcr_package_cache
            .get(&self.ghcr_cache_key())
            .cloned()
            .unwrap_or_default();
        self.modal = Modal::GhcrPicker {
            filter: InputState::default(),
            packages: cached,
            selected: 0,
            loading: true,
        };
    }

    // ------------------------------------------------------------------
    // AWS ECR
    // ------------------------------------------------------------------

    /// Cache key for the active profile's repository list: the profile, plus
    /// both halves of the AWS target it is currently pointed at.
    fn ecr_cache_key(&self) -> (String, Option<String>, Option<String>) {
        let profile = &self.profiles[self.active_profile_idx];
        (
            profile.name.clone(),
            profile.aws_profile.clone(),
            profile.region.clone(),
        )
    }

    /// Switch to an ECR profile.
    ///
    /// Unlike GHCR, no picker opens: an ECR repository list is an ordinary flat
    /// catalog, just fetched from AWS rather than `/v2/_catalog`, so it belongs
    /// in the Repos pane and nowhere else. Any cached list is shown immediately
    /// so a re-switch is not a blank pane behind a spinner.
    pub fn start_ecr_switch(&mut self, idx: usize) {
        self.active_profile_idx = idx;
        let profile = &self.profiles[idx];
        self.registry_name = profile.name.clone();
        self.registry_url = profile.display_url();
        self.reset_for_new_registry();
        self.repo_load = LoadState::Loading;

        let cached = self
            .ecr_repo_cache
            .get(&self.ecr_cache_key())
            .cloned()
            .unwrap_or_default();
        if !cached.is_empty() {
            self.on_repos_page(cached, false);
            self.repo_load = LoadState::Loading;
        }
    }

    /// Fill in the registry endpoint and repository list once AWS answers.
    ///
    /// `registry_url` arrives with the repositories because both come from the
    /// same connect: an ECR hostname embeds the account ID, so until
    /// `GetAuthorizationToken` returns there is nothing truthful to show in the
    /// title bar.
    pub fn on_ecr_connected(&mut self, registry_url: String, repos: Vec<String>) {
        self.registry_url = registry_url;
        self.ecr_repo_cache
            .insert(self.ecr_cache_key(), repos.clone());
        // `has_more = false`: the SDK paginator is drained inside the fetch, so
        // there is no cursor for the Repos pane to continue from.
        self.on_repos_page(repos, false);
    }

    /// Report a failed ECR connect.
    ///
    /// Hands over to the same browse-by-name fallback a missing catalog uses.
    /// That is not merely a graceful failure: `ecr:DescribeRepositories` and
    /// `ecr:GetAuthorizationToken` are separate permissions, so an identity
    /// that can pull images but not enumerate them lands here with browsing
    /// still perfectly possible by typing a name.
    pub fn on_ecr_error(&mut self, msg: String) {
        self.modal = Modal::None;
        self.on_repos_error(msg, true);
    }

    /// Open the AWS profile picker, the first stage of an ECR target switch.
    ///
    /// Leaves repo/tag/detail state alone: nothing changes until both stages
    /// are confirmed, so `Esc` returns to browsing exactly as it was.
    pub fn open_ecr_profile_picker(&mut self) {
        let suggestions = self.aws_profile_names();
        let current = self.profiles[self.active_profile_idx].aws_profile.clone();
        let selected = current
            .as_deref()
            .and_then(|c| suggestions.iter().position(|p| p == c))
            .unwrap_or(0);

        self.modal = Modal::EcrProfilePicker {
            input: InputState::default(),
            profiles: suggestions,
            selected,
        };
    }

    /// Suggestions for the profile picker: the parsed AWS config, plus the
    /// profile currently in use if it was not among them.
    fn aws_profile_names(&self) -> Vec<String> {
        let mut names: Vec<String> = self
            .aws_profile_cache
            .iter()
            .map(|p| p.name.clone())
            .collect();
        if let Some(current) = &self.profiles[self.active_profile_idx].aws_profile
            && !names.iter().any(|n| n == current)
        {
            names.insert(0, current.clone());
        }
        names
    }

    /// Record the AWS profiles read from disk.
    pub fn on_aws_profiles(&mut self, profiles: Vec<AwsProfile>) {
        self.aws_profile_cache = profiles;

        // Refresh an already-open picker in place: the read is fast enough that
        // the picker usually opens first.
        if let Modal::EcrProfilePicker {
            profiles: p,
            selected,
            ..
        } = &mut self.modal
        {
            *p = self
                .aws_profile_cache
                .iter()
                .map(|profile| profile.name.clone())
                .collect();
            let len = p.len();
            *selected = if len == 0 {
                0
            } else {
                (*selected).min(len - 1)
            };
        }
    }

    /// The profile picker's rows for its current input.
    pub fn ecr_profile_rows(&self) -> Vec<PickerChoice> {
        let Modal::EcrProfilePicker {
            input, profiles, ..
        } = &self.modal
        else {
            return Vec::new();
        };
        picker_choices(&input.buffer, profiles)
    }

    /// Open the region picker, carrying the AWS profile just chosen.
    ///
    /// The chosen profile's own configured region leads the suggestions, since
    /// it is overwhelmingly the one wanted; the rest of the static region list
    /// follows, and any region can still be typed.
    pub fn open_ecr_region_picker(&mut self, aws_profile: Option<String>) {
        let profile_region = aws_profile.as_deref().and_then(|name| {
            self.aws_profile_cache
                .iter()
                .find(|p| p.name == name)
                .and_then(|p| p.region.clone())
        });
        let current = self.profiles[self.active_profile_idx].region.clone();

        let regions = ecr_region_suggestions(profile_region.as_deref(), current.as_deref());
        let selected = current
            .as_deref()
            .or(profile_region.as_deref())
            .and_then(|r| regions.iter().position(|candidate| candidate == r))
            .unwrap_or(0);

        self.modal = Modal::EcrRegionPicker {
            input: InputState::default(),
            regions,
            selected,
            aws_profile,
        };
    }

    /// The region picker's rows for its current input.
    pub fn ecr_region_rows(&self) -> Vec<PickerChoice> {
        let Modal::EcrRegionPicker { input, regions, .. } = &self.modal else {
            return Vec::new();
        };
        picker_choices(&input.buffer, regions)
    }

    /// Point the active ECR profile at a new account and region, and start over
    /// beneath it.
    ///
    /// In-memory only, like [`Self::apply_ghcr_owner`] — the app never writes
    /// `config.toml`.
    pub fn apply_ecr_target(&mut self, aws_profile: Option<String>, region: Option<String>) {
        {
            let profile = &mut self.profiles[self.active_profile_idx];
            profile.aws_profile = aws_profile;
            profile.region = region;
            // The endpoint belonged to the *previous* account, so it must not
            // outlive the switch in the title bar.
            profile.url = None;
        }
        self.modal = Modal::None;
        let idx = self.active_profile_idx;
        self.start_ecr_switch(idx);
    }

    /// The picker's package list filtered by its current filter buffer
    /// (substring match, case-insensitive).
    pub fn ghcr_filtered_packages(&self) -> Vec<&String> {
        let Modal::GhcrPicker {
            filter, packages, ..
        } = &self.modal
        else {
            return Vec::new();
        };
        let f = filter.buffer.to_lowercase();
        if f.is_empty() {
            packages.iter().collect()
        } else {
            packages
                .iter()
                .filter(|p| p.to_lowercase().contains(&f))
                .collect()
        }
    }

    /// Move the Repos pane's selection onto `repo`, reporting whether it was
    /// there to select.
    ///
    /// Keeps the pane consistent with a repo chosen from the GHCR picker
    /// rather than by navigating the pane. The event loop reloads tags
    /// whenever the repo selection changes, so moving the selection is all
    /// that a pick needs to do — issuing a `BrowseRepo` as well would fetch
    /// the same tags twice, and `on_tags_page` appends, so the list would come
    /// back doubled. `false` means the caller still has to browse it some
    /// other way (it can be missing when a repo filter is active).
    pub fn select_repo_by_name(&mut self, repo: &str) -> bool {
        if let Some(idx) = self.repos.iter().position(|r| r == repo) {
            self.repos_state.select(Some(idx));
            return true;
        }
        false
    }

    pub fn on_artifactory_repos_error(&mut self, msg: String) {
        if let Modal::ArtifactoryPicker { loading, .. } = &mut self.modal {
            *loading = false;
        }
        self.set_status(format!("Artifactory repos error: {msg}"));
    }

    /// The picker's repo list filtered by its current filter buffer
    /// (substring match on repo-key, case-insensitive).
    pub fn artifactory_filtered_repos(&self) -> Vec<&ArtifactoryRepo> {
        let Modal::ArtifactoryPicker { filter, repos, .. } = &self.modal else {
            return Vec::new();
        };
        let f = filter.buffer.to_lowercase();
        if f.is_empty() {
            repos.iter().collect()
        } else {
            repos
                .iter()
                .filter(|r| r.key.to_lowercase().contains(&f))
                .collect()
        }
    }

    /// Descend into a chosen Artifactory repo-key: from here on it behaves
    /// exactly like any other registry (`scoped_url` is its `/v2/` root's
    /// non-`/v2/` base, used for display and pull-URL construction).
    pub fn enter_artifactory_repo(&mut self, repo_key: &str, scoped_url: String) {
        let profile_name = self.profiles[self.active_profile_idx].name.clone();
        self.registry_name = format!("{profile_name}/{repo_key}");
        self.registry_url = scoped_url;
        self.modal = Modal::None;
        self.reset_for_new_registry();
        self.current_artifactory_repo_key = Some(repo_key.to_owned());
        self.repo_load = LoadState::Loading;
    }

    // ------------------------------------------------------------------
    // Pagination hints
    // ------------------------------------------------------------------

    pub fn should_load_more_repos(&self) -> bool {
        if !self.repos_has_more || self.repo_load != LoadState::Idle {
            return false;
        }
        let selected = self.repos_state.selected().unwrap_or(0);
        selected + LOAD_AHEAD >= self.repos.len()
    }

    pub fn should_load_more_tags(&self) -> bool {
        if !self.tags_has_more || self.tag_load != LoadState::Idle {
            return false;
        }
        let selected = self.tags_state.selected().unwrap_or(0);
        selected + LOAD_AHEAD >= self.tags.len()
    }

    // ------------------------------------------------------------------
    // Filters
    // ------------------------------------------------------------------

    pub fn push_filter_char(&mut self, ch: char) {
        match self.filter_mode {
            Some(Focus::Repos) => {
                self.repo_filter.push(ch);
                self.apply_repo_filter();
            }
            Some(Focus::Tags) => {
                self.tag_filter.push(ch);
                self.apply_tag_filter_sort();
            }
            Some(Focus::Detail) | None => {}
        }
    }

    /// Append `s` to the active filter in one step (e.g. a terminal paste).
    /// Strips `\n`/`\r` for the same reason `InputState::insert_str` does —
    /// this is a single-line filter, not a text area.
    pub fn push_filter_str(&mut self, s: &str) {
        let s: String = s.chars().filter(|&c| c != '\n' && c != '\r').collect();
        match self.filter_mode {
            Some(Focus::Repos) => {
                self.repo_filter.push_str(&s);
                self.apply_repo_filter();
            }
            Some(Focus::Tags) => {
                self.tag_filter.push_str(&s);
                self.apply_tag_filter_sort();
            }
            Some(Focus::Detail) | None => {}
        }
    }

    pub fn pop_filter_char(&mut self) {
        match self.filter_mode {
            Some(Focus::Repos) => {
                self.repo_filter.pop();
                self.apply_repo_filter();
            }
            Some(Focus::Tags) => {
                self.tag_filter.pop();
                self.apply_tag_filter_sort();
            }
            Some(Focus::Detail) | None => {}
        }
    }

    pub fn clear_active_filter(&mut self) {
        match self.filter_mode {
            Some(Focus::Repos) => {
                self.repo_filter.clear();
                self.apply_repo_filter();
            }
            Some(Focus::Tags) => {
                self.tag_filter.clear();
                self.apply_tag_filter_sort();
            }
            Some(Focus::Detail) | None => {}
        }
        self.filter_mode = None;
    }

    fn apply_repo_filter(&mut self) {
        let filter = self.repo_filter.to_lowercase();
        self.repos = if filter.is_empty() {
            self.repos_all.clone()
        } else {
            self.repos_all
                .iter()
                .filter(|r| r.to_lowercase().contains(&filter))
                .cloned()
                .collect()
        };
        self.clamp_repo_selection();
    }

    fn apply_tag_filter_sort(&mut self) {
        let filter = self.tag_filter.to_lowercase();
        let mut filtered: Vec<String> = if filter.is_empty() {
            self.tags_all.clone()
        } else {
            self.tags_all
                .iter()
                .filter(|t| t.to_lowercase().contains(&filter))
                .cloned()
                .collect()
        };
        match self.tag_sort {
            SortOrder::NameAsc => filtered.sort(),
            SortOrder::NameDesc => {
                filtered.sort();
                filtered.reverse();
            }
        }
        self.tags = filtered;
        self.clamp_tag_selection();
    }

    fn clamp_repo_selection(&mut self) {
        let len = self.repos.len();
        if len == 0 {
            self.repos_state.select(None);
        } else {
            let i = self.repos_state.selected().unwrap_or(0).min(len - 1);
            self.repos_state.select(Some(i));
        }
    }

    fn clamp_tag_selection(&mut self) {
        let len = self.tags.len();
        if len == 0 {
            self.tags_state.select(None);
        } else if self.tags_state.selected().is_none() {
            self.tags_state.select(Some(0));
        } else {
            let i = self.tags_state.selected().unwrap_or(0).min(len - 1);
            self.tags_state.select(Some(i));
        }
    }

    // ------------------------------------------------------------------
    // Navigation
    // ------------------------------------------------------------------

    pub fn scroll_up(&mut self) {
        match self.focus {
            Focus::Repos => {
                let i = self.repos_state.selected().unwrap_or(0);
                if i > 0 {
                    self.repos_state.select(Some(i - 1));
                    self.tags.clear();
                    self.tags_state.select(None);
                }
            }
            Focus::Tags => {
                let i = self.tags_state.selected().unwrap_or(0);
                if i > 0 {
                    self.tags_state.select(Some(i - 1));
                }
            }
            Focus::Detail => {
                self.detail_scroll = self.detail_scroll.saturating_sub(1);
            }
        }
    }

    pub fn scroll_down(&mut self) {
        match self.focus {
            Focus::Repos => {
                let len = self.repos.len();
                if len == 0 {
                    return;
                }
                let i = self.repos_state.selected().unwrap_or(0);
                if i + 1 < len {
                    self.repos_state.select(Some(i + 1));
                    self.tags.clear();
                    self.tags_state.select(None);
                }
            }
            Focus::Tags => {
                let len = self.tags.len();
                if len == 0 {
                    return;
                }
                let i = self.tags_state.selected().unwrap_or(0);
                if i + 1 < len {
                    self.tags_state.select(Some(i + 1));
                }
            }
            Focus::Detail => {
                self.detail_scroll = self.detail_scroll.saturating_add(1);
            }
        }
    }

    pub fn selected_repo(&self) -> Option<&str> {
        self.repos_state
            .selected()
            .and_then(|i| self.repos.get(i))
            .map(String::as_str)
    }

    pub fn selected_tag(&self) -> Option<&str> {
        self.tags_state
            .selected()
            .and_then(|i| self.tags.get(i))
            .map(String::as_str)
    }

    // ------------------------------------------------------------------
    // Status
    // ------------------------------------------------------------------

    pub fn set_status(&mut self, msg: impl Into<String>) {
        self.status = Some(StatusMessage {
            text: msg.into(),
            expires_at: Instant::now() + STATUS_TTL,
        });
    }

    pub fn status_text(&self) -> Option<&str> {
        self.status.as_ref().map(|s| s.text.as_str())
    }

    pub fn on_delete_success(&mut self, repo: &str, tag: &str) {
        self.tags_all.retain(|t| t != tag);
        self.apply_tag_filter_sort();
        if self.current_tag.as_deref() == Some(tag) {
            self.detail = None;
            self.detail_load = LoadState::Idle;
            self.current_tag = None;
        }
        self.set_status(format!("✓ Deleted {repo}:{tag}"));
    }

    pub fn on_delete_error(&mut self, msg: String) {
        self.set_status(format!("✗ Delete failed: {msg}"));
    }

    pub fn on_retag_success(&mut self, new_tag: String) {
        if !self.tags_all.contains(&new_tag) {
            self.tags_all.push(new_tag.clone());
            self.apply_tag_filter_sort();
        }
        self.set_status(format!("✓ Tagged as {new_tag}"));
    }

    pub fn on_retag_error(&mut self, msg: String) {
        self.set_status(format!("✗ Retag failed: {msg}"));
    }

    pub fn resort_tags(&mut self) {
        self.apply_tag_filter_sort();
    }

    pub fn tick(&mut self) {
        self.spinner_tick = self.spinner_tick.wrapping_add(1);
        if let Some(s) = &self.status
            && Instant::now() >= s.expires_at
        {
            self.status = None;
        }
    }

    // ------------------------------------------------------------------
    // Modal-setup handlers (pure state, no async dependencies)
    // ------------------------------------------------------------------

    pub fn copy_pull_url(&mut self) {
        let Some(pull_url) = self.detail.as_ref().map(|d| d.pull_url.clone()) else {
            return;
        };
        match clipboard::copy_to_clipboard(&pull_url) {
            Ok(()) => self.set_status(format!("✓ Copied: {pull_url}")),
            Err(e) => self.set_status(format!("Clipboard error: {e}")),
        }
    }

    pub fn start_copy_image(&mut self) {
        let Some(tag) = self.selected_tag().map(str::to_owned) else {
            return;
        };
        let Some(repo) = self.current_repo.clone() else {
            return;
        };
        let prefilled = format!("{repo}:{tag}");
        let mut input = InputState::default();
        input.start(&prefilled);
        self.modal = Modal::Input {
            prompt: "Copy to (repo:tag):".to_owned(),
            input,
            on_confirm: InputAction::CopyImage {
                src_repo: repo,
                src_tag: tag,
            },
        };
    }

    pub fn start_retag(&mut self) {
        let Some(tag) = self.selected_tag().map(str::to_owned) else {
            return;
        };
        let Some(repo) = self.current_repo.clone() else {
            return;
        };
        self.modal = Modal::Input {
            prompt: format!("New tag for '{repo}:{tag}':"),
            input: InputState::default(),
            on_confirm: InputAction::Retag { repo, src_tag: tag },
        };
    }

    pub fn start_registry_select(&mut self) {
        let current = self.active_profile_idx;
        self.modal = Modal::RegistrySelect {
            selected_idx: current,
        };
    }

    pub fn start_delete(&mut self) {
        if self.focus == Focus::Tags
            && let Some(tag) = self.selected_tag().map(str::to_owned)
            && let Some(repo) = self.current_repo.clone()
        {
            let msg = format!("Delete '{repo}:{tag}'?");
            self.modal = Modal::Confirm {
                message: msg,
                on_confirm: ConfirmAction::DeleteManifest { repo, tag },
            };
        }
    }

    pub fn start_export(&mut self) {
        let Some(tag) = self.selected_tag().map(str::to_owned) else {
            return;
        };
        let Some(repo) = self.current_repo.clone() else {
            return;
        };
        let default_path = format!("{}-{}.tar", repo.replace('/', "-"), tag);
        let mut input = InputState::default();
        input.start(&default_path);
        self.modal = Modal::Input {
            prompt: "Export OCI tar to:".to_owned(),
            input,
            on_confirm: InputAction::Export { repo, tag },
        };
    }

    pub fn start_diff(&mut self) {
        let Some(tag) = self.selected_tag().map(str::to_owned) else {
            return;
        };
        let Some(repo) = self.current_repo.clone() else {
            return;
        };
        self.modal = Modal::Input {
            prompt: format!("Diff '{tag}' against tag:"),
            input: InputState::default(),
            on_confirm: InputAction::DiffAgainst { repo, tag_a: tag },
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{RegistryProfile, RegistryType};

    fn make_app() -> App {
        let profile = RegistryProfile {
            name: "test".to_owned(),
            url: Some("http://localhost:5000".to_owned()),
            username: None,
            registry_type: RegistryType::Standard,
            ..Default::default()
        };
        App::new(vec![profile], 0)
    }

    fn make_app_with_repos(repos: Vec<&str>) -> App {
        let mut app = make_app();
        app.on_repos_page(repos.into_iter().map(str::to_owned).collect(), false);
        app
    }

    fn make_app_with_tags(repo: &str, tags: Vec<&str>) -> App {
        let mut app = make_app();
        app.start_tags_load(repo.to_owned());
        app.on_tags_page(
            repo.to_owned(),
            tags.into_iter().map(str::to_owned).collect(),
            false,
        );
        app
    }

    #[test]
    fn new_initial_state() {
        let app = make_app();
        assert_eq!(app.focus, Focus::Repos);
        assert!(!app.should_quit);
        assert!(app.repos.is_empty());
        assert!(app.tags.is_empty());
        assert!(matches!(app.modal, Modal::None));
        assert_eq!(app.repo_load, LoadState::Idle);
        assert_eq!(app.tag_load, LoadState::Idle);
        assert_eq!(app.spinner_tick, 0);
        assert!(app.current_repo.is_none());
    }

    #[test]
    fn scroll_down_up_repos() {
        let mut app = make_app_with_repos(vec!["a", "b", "c"]);
        assert_eq!(app.repos_state.selected(), Some(0));
        app.scroll_down();
        assert_eq!(app.repos_state.selected(), Some(1));
        app.scroll_up();
        assert_eq!(app.repos_state.selected(), Some(0));
        // scroll_up at top stays at 0
        app.scroll_up();
        assert_eq!(app.repos_state.selected(), Some(0));
    }

    #[test]
    fn filter_push_pop_clear() {
        // "crow" contains no 'a', so only alpha+aleph match
        let mut app = make_app_with_repos(vec!["alpha", "crow", "aleph"]);
        app.filter_mode = Some(Focus::Repos);

        app.push_filter_char('a');
        assert_eq!(app.repo_filter, "a");
        assert_eq!(app.repos.len(), 2); // alpha, aleph

        app.push_filter_char('l');
        assert_eq!(app.repo_filter, "al");
        assert_eq!(app.repos.len(), 2); // alpha, aleph

        app.pop_filter_char();
        assert_eq!(app.repo_filter, "a");

        app.clear_active_filter();
        assert_eq!(app.repo_filter, "");
        assert!(app.filter_mode.is_none());
        assert_eq!(app.repos.len(), 3);
    }

    #[test]
    fn on_repos_page_populates_list() {
        let mut app = make_app();
        app.on_repos_page(vec!["r1".to_owned(), "r2".to_owned()], false);
        assert_eq!(app.repos, vec!["r1", "r2"]);
        assert_eq!(app.repo_load, LoadState::Idle);
        assert!(!app.repos_has_more);
    }

    #[test]
    fn on_repos_page_twice_accumulates() {
        let mut app = make_app();
        app.on_repos_page(vec!["r1".to_owned()], true);
        app.on_repos_page(vec!["r2".to_owned()], false);
        assert_eq!(app.repos, vec!["r1", "r2"]);
        assert!(!app.repos_has_more);
    }

    #[test]
    fn on_tags_page_ignores_stale_repo() {
        let mut app = make_app();
        app.start_tags_load("r1".to_owned());
        app.on_tags_page("r2".to_owned(), vec!["latest".to_owned()], false);
        assert!(app.tags.is_empty());
        assert_eq!(app.tag_load, LoadState::Loading);
    }

    #[test]
    fn start_tags_load_resets_state() {
        let mut app = make_app_with_tags("old", vec!["v1"]);
        assert!(!app.tags.is_empty());

        app.start_tags_load("new".to_owned());
        assert!(app.tags.is_empty());
        assert_eq!(app.tag_load, LoadState::Loading);
        assert_eq!(app.current_repo.as_deref(), Some("new"));
        assert!(app.detail.is_none());
    }

    #[test]
    fn start_registry_switch_resets_all() {
        let profile_a = RegistryProfile {
            name: "a".to_owned(),
            url: Some("http://a:5000".to_owned()),
            username: None,
            registry_type: RegistryType::Standard,
            ..Default::default()
        };
        let profile_b = RegistryProfile {
            name: "b".to_owned(),
            url: Some("http://b:5000".to_owned()),
            username: None,
            registry_type: RegistryType::Standard,
            ..Default::default()
        };
        let mut app = App::new(vec![profile_a, profile_b], 0);
        app.on_repos_page(vec!["r1".to_owned()], false);
        app.start_tags_load("r1".to_owned());
        app.on_tags_page("r1".to_owned(), vec!["v1".to_owned()], false);

        app.start_registry_switch(1);

        assert!(app.repos.is_empty());
        assert!(app.tags.is_empty());
        assert!(app.current_repo.is_none());
        assert_eq!(app.focus, Focus::Repos);
        assert_eq!(app.active_profile_idx, 1);
        assert_eq!(app.registry_name, "b");
    }

    fn artifactory_repo(key: &str) -> ArtifactoryRepo {
        ArtifactoryRepo {
            key: key.to_owned(),
            repo_type: "LOCAL".to_owned(),
            url: format!("https://artifactory.example.com/artifactory/{key}"),
            package_type: "Docker".to_owned(),
        }
    }

    #[test]
    fn start_artifactory_switch_opens_loading_picker() {
        let profile = RegistryProfile {
            name: "art".to_owned(),
            url: Some("https://artifactory.example.com/artifactory".to_owned()),
            username: None,
            registry_type: RegistryType::Artifactory,
            ..Default::default()
        };
        let mut app = App::new(vec![profile], 0);
        app.start_artifactory_switch(0);
        assert!(matches!(
            app.modal,
            Modal::ArtifactoryPicker { loading: true, .. }
        ));
        assert_eq!(app.repo_load, LoadState::Idle);
    }

    #[test]
    fn on_artifactory_repos_fills_picker_and_clears_loading() {
        let profile = RegistryProfile {
            name: "art".to_owned(),
            url: Some("https://artifactory.example.com/artifactory".to_owned()),
            username: None,
            registry_type: RegistryType::Artifactory,
            ..Default::default()
        };
        let mut app = App::new(vec![profile], 0);
        app.start_artifactory_switch(0);
        app.on_artifactory_repos(vec![
            artifactory_repo("docker-local"),
            artifactory_repo("docker-remote"),
        ]);
        assert!(matches!(
            app.modal,
            Modal::ArtifactoryPicker { loading: false, ref repos, .. } if repos.len() == 2
        ));
    }

    #[test]
    fn artifactory_filtered_repos_matches_substring_case_insensitive() {
        let profile = RegistryProfile {
            name: "art".to_owned(),
            url: Some("https://artifactory.example.com/artifactory".to_owned()),
            username: None,
            registry_type: RegistryType::Artifactory,
            ..Default::default()
        };
        let mut app = App::new(vec![profile], 0);
        app.start_artifactory_switch(0);
        app.on_artifactory_repos(vec![
            artifactory_repo("docker-local"),
            artifactory_repo("maven-local"),
        ]);

        assert_eq!(app.artifactory_filtered_repos().len(), 2);

        if let Modal::ArtifactoryPicker { filter, .. } = &mut app.modal {
            filter.start("DOCKER");
        }
        let filtered = app.artifactory_filtered_repos();
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].key, "docker-local");
    }

    #[test]
    fn enter_artifactory_repo_closes_modal_and_resets_state() {
        let profile = RegistryProfile {
            name: "art".to_owned(),
            url: Some("https://artifactory.example.com/artifactory".to_owned()),
            username: None,
            registry_type: RegistryType::Artifactory,
            ..Default::default()
        };
        let mut app = App::new(vec![profile], 0);
        app.start_artifactory_switch(0);
        app.on_artifactory_repos(vec![artifactory_repo("docker-local")]);

        let scoped_url =
            "https://artifactory.example.com/artifactory/api/docker/docker-local/".to_owned();
        app.enter_artifactory_repo("docker-local", scoped_url.clone());

        assert!(matches!(app.modal, Modal::None));
        assert_eq!(app.registry_name, "art/docker-local");
        assert_eq!(app.registry_url, scoped_url);
        assert_eq!(app.repo_load, LoadState::Loading);
        assert!(app.repos.is_empty());
    }

    #[test]
    fn restart_catalog_load_preserves_artifactory_repo_key() {
        let profile = RegistryProfile {
            name: "art".to_owned(),
            url: Some("https://artifactory.example.com/artifactory".to_owned()),
            username: None,
            registry_type: RegistryType::Artifactory,
            ..Default::default()
        };
        let mut app = App::new(vec![profile], 0);
        app.start_artifactory_switch(0);
        app.on_artifactory_repos(vec![artifactory_repo("docker-local")]);

        let scoped_url =
            "https://artifactory.example.com/artifactory/api/docker/docker-local/".to_owned();
        app.enter_artifactory_repo("docker-local", scoped_url.clone());
        app.on_repos_page(vec!["nginx".to_owned()], false);

        // Simulate a successful re-auth while browsing inside a repo-key.
        app.catalog_attempt = CatalogAttempt::AfterCredential;
        app.restart_catalog_load();

        // The catalog reloads...
        assert_eq!(app.repo_load, LoadState::Loading);
        assert!(app.repos.is_empty());
        // ...and restart_catalog_load leaves the stage alone — it's the
        // caller's job to set it, since the reload alone can't say which
        // retry it's for.
        assert_eq!(app.catalog_attempt, CatalogAttempt::AfterCredential);

        // ...but the user is NOT ejected from the repo-key they were in.
        // `start_registry_switch` would have cleared all three of these, while
        // the client stayed scoped to the repo-key.
        assert_eq!(
            app.current_artifactory_repo_key.as_deref(),
            Some("docker-local")
        );
        assert_eq!(app.registry_name, "art/docker-local");
        assert_eq!(app.registry_url, scoped_url);
    }

    fn app_inside_artifactory_repo() -> App {
        let profile = RegistryProfile {
            name: "art".to_owned(),
            url: Some("https://artifactory.example.com/artifactory".to_owned()),
            username: None,
            registry_type: RegistryType::Artifactory,
            ..Default::default()
        };
        let mut app = App::new(vec![profile], 0);
        app.start_artifactory_switch(0);
        app.on_artifactory_repos(vec![
            artifactory_repo("docker-local"),
            artifactory_repo("docker-remote"),
        ]);
        let scoped_url =
            "https://artifactory.example.com/artifactory/api/docker/docker-local/".to_owned();
        app.enter_artifactory_repo("docker-local", scoped_url);
        app.on_repos_page(vec!["myimage".to_owned()], false);
        app
    }

    #[test]
    fn open_artifactory_picker_cached_uses_cache_and_preselects_current_key() {
        let mut app = app_inside_artifactory_repo();

        app.open_artifactory_picker_cached();

        assert!(matches!(
            app.modal,
            Modal::ArtifactoryPicker { loading: true, ref repos, selected: 0, .. }
                if repos.len() == 2 && repos[0].key == "docker-local"
        ));
    }

    #[test]
    fn open_artifactory_picker_cached_does_not_reset_browsing_state() {
        let mut app = app_inside_artifactory_repo();
        assert_eq!(app.repos, vec!["myimage"]);

        app.open_artifactory_picker_cached();

        // Esc from the picker (Modal::None) should return to exactly this.
        assert_eq!(app.repos, vec!["myimage"]);
        assert_eq!(
            app.current_artifactory_repo_key.as_deref(),
            Some("docker-local")
        );
        assert_eq!(app.registry_name, "art/docker-local");
    }

    #[test]
    fn on_artifactory_repos_clamps_selection_on_background_refresh() {
        let mut app = app_inside_artifactory_repo();
        app.open_artifactory_picker_cached();
        if let Modal::ArtifactoryPicker { selected, .. } = &mut app.modal {
            *selected = 1;
        }

        // Refresh lands with a shorter list (docker-remote dropped upstream).
        app.on_artifactory_repos(vec![artifactory_repo("docker-local")]);

        assert!(matches!(
            app.modal,
            Modal::ArtifactoryPicker { loading: false, selected: 0, ref repos, .. }
                if repos.len() == 1
        ));
    }

    #[test]
    fn open_artifactory_picker_cached_empty_cache_opens_loading() {
        let profile = RegistryProfile {
            name: "art".to_owned(),
            url: Some("https://artifactory.example.com/artifactory".to_owned()),
            username: None,
            registry_type: RegistryType::Artifactory,
            ..Default::default()
        };
        let mut app = App::new(vec![profile], 0);

        app.open_artifactory_picker_cached();

        assert!(matches!(
            app.modal,
            Modal::ArtifactoryPicker { loading: true, ref repos, selected: 0, .. }
                if repos.is_empty()
        ));
    }

    #[test]
    fn tick_increments_spinner() {
        let mut app = make_app();
        assert_eq!(app.spinner_tick, 0);
        app.tick();
        assert_eq!(app.spinner_tick, 1);
        app.tick();
        assert_eq!(app.spinner_tick, 2);
    }

    #[test]
    fn tick_expires_status() {
        use std::time::{Duration, Instant};
        let mut app = make_app();
        app.status = Some(StatusMessage {
            text: "hello".to_owned(),
            expires_at: Instant::now() - Duration::from_secs(1),
        });
        app.tick();
        assert!(app.status_text().is_none());
    }

    #[test]
    fn start_copy_image_sets_modal() {
        let mut app = make_app_with_tags("myrepo", vec!["v1"]);
        app.start_copy_image();
        assert!(matches!(
            app.modal,
            Modal::Input {
                ref on_confirm,
                ..
            } if matches!(on_confirm, InputAction::CopyImage { src_repo, src_tag }
                if src_repo == "myrepo" && src_tag == "v1")
        ));
    }

    #[test]
    fn start_copy_image_noop_without_tag() {
        let mut app = make_app();
        app.start_copy_image();
        assert!(matches!(app.modal, Modal::None));
    }

    #[test]
    fn start_retag_sets_modal() {
        let mut app = make_app_with_tags("myrepo", vec!["v1"]);
        app.start_retag();
        assert!(matches!(
            app.modal,
            Modal::Input {
                ref on_confirm,
                ..
            } if matches!(on_confirm, InputAction::Retag { repo, src_tag }
                if repo == "myrepo" && src_tag == "v1")
        ));
    }

    #[test]
    fn start_delete_sets_confirm_modal() {
        let mut app = make_app_with_tags("myrepo", vec!["v1"]);
        app.focus = Focus::Tags;
        app.start_delete();
        assert!(matches!(
            app.modal,
            Modal::Confirm {
                ref on_confirm,
                ..
            } if matches!(on_confirm, ConfirmAction::DeleteManifest { repo, tag }
                if repo == "myrepo" && tag == "v1")
        ));
    }

    #[test]
    fn start_delete_noop_when_not_tags_focus() {
        let mut app = make_app_with_tags("myrepo", vec!["v1"]);
        app.focus = Focus::Repos;
        app.start_delete();
        assert!(matches!(app.modal, Modal::None));
    }

    #[test]
    fn should_load_more_repos_when_has_more() {
        let mut app = make_app();
        app.on_repos_page(vec!["r1".to_owned()], true);
        // selected=0, repos.len()=1, LOAD_AHEAD=20 → 0+20 >= 1 → true
        assert!(app.should_load_more_repos());
    }

    #[test]
    fn should_load_more_repos_false_when_no_more() {
        let mut app = make_app();
        app.on_repos_page(vec!["r1".to_owned()], false);
        assert!(!app.should_load_more_repos());
    }

    #[test]
    fn should_load_more_tags_when_has_more() {
        let mut app = make_app_with_tags("r", vec!["v1"]);
        app.tags_has_more = true;
        assert!(app.should_load_more_tags());
    }

    // ---- InspectModal (JSON viewer) ----

    const INSPECT_JSON: &str = r#"{
  "schemaVersion": 2,
  "config": {
    "digest": "sha256:abc",
    "size": 1234
  },
  "layers": [
    {
      "digest": "sha256:def"
    }
  ]
}"#;

    fn inspect_modal() -> InspectModal {
        let lines = INSPECT_JSON.lines().map(str::to_owned).collect();
        let mut m = InspectModal::new("img:tag".to_owned(), lines);
        m.set_viewport(4); // a small viewport to exercise paging/scroll
        m
    }

    #[test]
    fn cursor_moves_and_clamps_at_ends() {
        let mut m = inspect_modal();
        assert_eq!(m.cursor, 0);
        m.move_cursor(-1); // clamp at top
        assert_eq!(m.cursor, 0);
        m.jump_bottom();
        let last = m.visible.len() - 1;
        assert_eq!(m.cursor, last);
        m.move_cursor(1); // clamp at bottom
        assert_eq!(m.cursor, last);
    }

    #[test]
    fn page_steps_by_viewport_height() {
        let mut m = inspect_modal(); // viewport_h == 4
        m.page(1);
        assert_eq!(m.cursor, 4);
    }

    #[test]
    fn toggle_fold_keeps_cursor_on_the_opener() {
        let mut m = inspect_modal();
        // Move cursor onto the "config" opener (absolute line 2).
        m.move_cursor(2);
        assert_eq!(m.cursor_line(), 2);
        m.toggle_fold();
        // config interior hidden; cursor still on the opener line.
        assert_eq!(m.cursor_line(), 2);
        assert!(m.collapsed[2]);
        assert!(!m.visible.contains(&3));
        m.toggle_fold(); // unfold restores interior, cursor unchanged
        assert_eq!(m.cursor_line(), 2);
        assert!(m.visible.contains(&3));
    }

    #[test]
    fn collapse_all_then_expand_all_round_trips() {
        let mut m = inspect_modal();
        let full = m.visible.len();
        m.collapse_all();
        assert!(m.visible.len() < full);
        assert!(m.visible.contains(&0)); // root opener still shown
        m.expand_all();
        assert_eq!(m.visible.len(), full);
    }

    #[test]
    fn search_jumps_to_match_and_cycles() {
        let mut m = inspect_modal();
        m.start_search();
        assert!(m.search.active);
        for c in "digest".chars() {
            m.search.input.insert(c);
        }
        m.commit_search();
        assert!(!m.search.active);
        assert_eq!(m.search.matches, vec![3, 8]);
        assert_eq!(m.cursor_line(), 3); // first hit
        m.next_match();
        assert_eq!(m.cursor_line(), 8);
        m.next_match(); // wraps back to first
        assert_eq!(m.cursor_line(), 3);
        m.prev_match(); // wraps to last
        assert_eq!(m.cursor_line(), 8);
    }

    #[test]
    fn search_expands_folds_hiding_a_match() {
        let mut m = inspect_modal();
        m.collapse_all();
        assert!(!m.visible.contains(&8)); // nested "digest" hidden
        m.start_search();
        for c in "sha256:def".chars() {
            m.search.input.insert(c);
        }
        m.commit_search();
        assert_eq!(m.cursor_line(), 8);
        assert!(m.visible.contains(&8)); // ancestors auto-expanded
    }

    #[test]
    fn input_action_is_secret_only_for_credential_actions() {
        assert!(
            InputAction::EnterPassword {
                profile_name: "p".to_owned(),
                username: "u".to_owned(),
            }
            .is_secret()
        );

        for action in [
            InputAction::BrowseRepo,
            InputAction::CopyImage {
                src_repo: "r".to_owned(),
                src_tag: "t".to_owned(),
            },
            InputAction::Retag {
                repo: "r".to_owned(),
                src_tag: "t".to_owned(),
            },
            InputAction::Export {
                repo: "r".to_owned(),
                tag: "t".to_owned(),
            },
            InputAction::DiffAgainst {
                repo: "r".to_owned(),
                tag_a: "t".to_owned(),
            },
        ] {
            assert!(!action.is_secret(), "{action:?} must not be masked");
        }
    }

    #[test]
    fn catalog_attempt_defaults_to_initial() {
        let profile = RegistryProfile {
            name: "local".to_owned(),
            url: Some("http://localhost:5000".to_owned()),
            username: None,
            registry_type: RegistryType::Standard,
            ..Default::default()
        };
        let app = App::new(vec![profile], 0);
        assert_eq!(app.catalog_attempt, CatalogAttempt::Initial);
    }

    #[test]
    fn reset_for_new_registry_resets_catalog_attempt() {
        // A stale AfterCredential/AfterReread stage from the previous
        // registry must not suppress a legitimate silent-reread or prompt on
        // the next one.
        let profiles = vec![
            RegistryProfile {
                name: "a".to_owned(),
                url: Some("http://a.example.com".to_owned()),
                username: None,
                registry_type: RegistryType::Standard,
                ..Default::default()
            },
            RegistryProfile {
                name: "b".to_owned(),
                url: Some("http://b.example.com".to_owned()),
                username: None,
                registry_type: RegistryType::Standard,
                ..Default::default()
            },
        ];
        let mut app = App::new(profiles, 0);
        app.catalog_attempt = CatalogAttempt::AfterCredential;

        app.start_registry_switch(1);

        assert_eq!(app.catalog_attempt, CatalogAttempt::Initial);
    }

    #[test]
    fn push_filter_str_appends_and_strips_newlines() {
        let mut app = make_app_with_repos(vec!["nginx", "budgeteer", "nginx-proxy"]);
        app.filter_mode = Some(Focus::Repos);

        app.push_filter_str("ngi\nnx\r\n");

        assert_eq!(app.repo_filter, "nginx");
        assert_eq!(
            app.repos,
            vec!["nginx".to_owned(), "nginx-proxy".to_owned()]
        );
    }

    #[test]
    fn push_filter_str_is_a_noop_without_an_active_filter() {
        let mut app = make_app_with_repos(vec!["nginx"]);
        assert_eq!(app.filter_mode, None);

        app.push_filter_str("nginx");

        assert_eq!(app.repo_filter, "");
    }

    // -----------------------------------------------------------------------
    // GHCR package picker
    // -----------------------------------------------------------------------

    // -----------------------------------------------------------------------
    // AWS ECR
    // -----------------------------------------------------------------------

    fn make_ecr_app() -> App {
        let profile = RegistryProfile {
            name: "ecr".to_owned(),
            registry_type: RegistryType::Ecr,
            aws_profile: Some("pgmac".to_owned()),
            region: Some("ap-southeast-2".to_owned()),
            ..Default::default()
        };
        App::new(vec![profile], 0)
    }

    /// The registry endpoint is not known until AWS answers, so the title bar
    /// must say what is being resolved rather than show a blank.
    #[test]
    fn an_ecr_switch_shows_the_aws_target_until_the_endpoint_resolves() {
        let mut app = make_ecr_app();
        app.start_ecr_switch(0);

        assert_eq!(app.registry_url, "ecr (pgmac / ap-southeast-2)");
        assert!(matches!(app.repo_load, LoadState::Loading));

        app.on_ecr_connected(
            "https://1.dkr.ecr.ap-southeast-2.amazonaws.com".to_owned(),
            vec!["team/api".to_owned()],
        );

        assert_eq!(
            app.registry_url,
            "https://1.dkr.ecr.ap-southeast-2.amazonaws.com"
        );
        assert_eq!(app.repos, vec!["team/api".to_owned()]);
        assert!(matches!(app.repo_load, LoadState::Idle));
    }

    /// The cache-key test: keyed by profile name alone, a target switch would
    /// serve the previous account's repositories, which reads as an AWS fault
    /// rather than a cache bug.
    #[test]
    fn changing_the_aws_target_does_not_serve_the_previous_accounts_repos() {
        let mut app = make_ecr_app();
        app.start_ecr_switch(0);
        app.on_ecr_connected(
            "https://1.dkr.ecr.ap-southeast-2.amazonaws.com".to_owned(),
            vec!["account-one/app".to_owned()],
        );

        app.apply_ecr_target(Some("other".to_owned()), Some("us-east-1".to_owned()));

        assert!(
            app.repos.is_empty(),
            "expected no repos for the new target, got {:?}",
            app.repos
        );
        assert!(matches!(app.repo_load, LoadState::Loading));

        // Returning to the first target serves its cached list again.
        app.apply_ecr_target(Some("pgmac".to_owned()), Some("ap-southeast-2".to_owned()));
        assert_eq!(app.repos, vec!["account-one/app".to_owned()]);
    }

    /// A stale endpoint outliving a target switch would show the old account's
    /// hostname in the title bar while browsing the new one.
    #[test]
    fn switching_target_clears_the_resolved_endpoint() {
        let mut app = make_ecr_app();
        app.on_ecr_connected(
            "https://1.dkr.ecr.ap-southeast-2.amazonaws.com".to_owned(),
            Vec::new(),
        );
        app.profiles[0].url = Some("https://1.dkr.ecr.ap-southeast-2.amazonaws.com".to_owned());

        app.apply_ecr_target(Some("other".to_owned()), Some("us-east-1".to_owned()));

        assert!(app.profiles[0].url.is_none());
        assert_eq!(app.registry_url, "ecr (other / us-east-1)");
    }

    /// `DescribeRepositories` and `GetAuthorizationToken` are separate
    /// permissions, so an identity that can pull but not enumerate must still
    /// be able to browse by typing a name.
    #[test]
    fn an_ecr_failure_offers_browse_by_name_and_never_a_credential_prompt() {
        let mut app = make_ecr_app();
        app.start_ecr_switch(0);

        app.on_ecr_error("AccessDeniedException: ecr:DescribeRepositories".to_owned());

        match &app.modal {
            Modal::Input { prompt, .. } => assert!(prompt.contains("repo name"), "{prompt}"),
            other => panic!("expected the browse-by-name input, got {other:?}"),
        }
    }

    #[test]
    fn the_profile_picker_offers_the_typed_value_and_the_parsed_suggestions() {
        let mut app = make_ecr_app();
        app.on_aws_profiles(vec![
            AwsProfile {
                name: "default".to_owned(),
                region: Some("ap-southeast-2".to_owned()),
            },
            AwsProfile {
                name: "pgmac".to_owned(),
                region: Some("us-east-1".to_owned()),
            },
        ]);
        app.open_ecr_profile_picker();

        // No input yet: suggestions only, with the profile in use highlighted.
        assert_eq!(
            app.ecr_profile_rows(),
            vec![
                PickerChoice::Listed("default".to_owned()),
                PickerChoice::Listed("pgmac".to_owned()),
            ]
        );
        let Modal::EcrProfilePicker { selected, .. } = &app.modal else {
            panic!("expected Modal::EcrProfilePicker");
        };
        assert_eq!(*selected, 1, "the profile in use should be highlighted");

        if let Modal::EcrProfilePicker { input, .. } = &mut app.modal {
            input.buffer = "sandbox".to_owned();
        }
        assert_eq!(
            app.ecr_profile_rows().first(),
            Some(&PickerChoice::Typed("sandbox".to_owned())),
            "a profile that is not in ~/.aws/config must still be reachable"
        );
    }

    /// The region picker is the second half of one choice, so it has to carry
    /// the profile forward — applying the region alone would point the registry
    /// at a new region of the *old* account.
    #[test]
    fn the_region_picker_carries_the_chosen_profile() {
        let mut app = make_ecr_app();
        app.on_aws_profiles(vec![AwsProfile {
            name: "sandbox".to_owned(),
            region: Some("eu-west-1".to_owned()),
        }]);

        app.open_ecr_region_picker(Some("sandbox".to_owned()));

        let Modal::EcrRegionPicker {
            aws_profile,
            regions,
            ..
        } = &app.modal
        else {
            panic!("expected Modal::EcrRegionPicker");
        };
        assert_eq!(aws_profile.as_deref(), Some("sandbox"));
        // The region in use leads, then the chosen profile's own default.
        assert_eq!(regions[0], "ap-southeast-2");
        assert_eq!(regions[1], "eu-west-1");
    }

    #[test]
    fn region_suggestions_put_the_likely_ones_first_without_duplicating_them() {
        let regions = ecr_region_suggestions(Some("eu-west-1"), Some("us-east-1"));

        assert_eq!(regions[0], "us-east-1");
        assert_eq!(regions[1], "eu-west-1");
        assert_eq!(
            regions.iter().filter(|r| *r == "us-east-1").count(),
            1,
            "a preferred region must not also appear in the static list"
        );
        assert!(regions.contains(&"ap-southeast-2".to_owned()));
    }

    #[test]
    fn region_suggestions_fall_back_to_the_static_list() {
        let regions = ecr_region_suggestions(None, None);

        assert_eq!(regions.len(), AWS_REGIONS.len());
        assert_eq!(regions[0], AWS_REGIONS[0]);
    }

    fn make_ghcr_app() -> App {
        let profile = RegistryProfile {
            name: "ghcr".to_owned(),
            url: Some("https://ghcr.io".to_owned()),
            registry_type: RegistryType::Ghcr,
            ..Default::default()
        };
        App::new(vec![profile], 0)
    }

    fn ghcr_packages() -> Vec<String> {
        [
            "homebrew/brew",
            "homebrew/core/git",
            "homebrew/core/sqldiff",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect()
    }

    /// The pane must not read as an empty catalog while the GitHub API call is
    /// still out — that is what `Idle` would look like to the renderer.
    #[test]
    fn start_ghcr_switch_opens_a_loading_picker() {
        let mut app = make_ghcr_app();
        app.start_ghcr_switch(0);

        assert!(matches!(app.modal, Modal::GhcrPicker { loading: true, .. }));
        assert_eq!(app.repo_load, LoadState::Loading);
    }

    /// One fetch, two surfaces. A GHCR package is an ordinary repository, so
    /// the list is both the picker's contents and the Repos pane's — filling
    /// only the picker would leave an empty pane behind it with nothing to
    /// return to on `Esc`.
    #[test]
    fn on_ghcr_packages_fills_both_the_picker_and_the_repos_pane() {
        let mut app = make_ghcr_app();
        app.start_ghcr_switch(0);
        app.on_ghcr_packages(ghcr_packages(), false);

        let Modal::GhcrPicker {
            packages, loading, ..
        } = &app.modal
        else {
            panic!("expected Modal::GhcrPicker");
        };
        assert_eq!(packages.len(), 3);
        assert!(!loading);

        assert_eq!(app.repos, ghcr_packages());
        assert_eq!(app.repo_load, LoadState::Idle);
    }

    #[test]
    fn ghcr_filter_matches_any_path_segment_case_insensitively() {
        let mut app = make_ghcr_app();
        app.start_ghcr_switch(0);
        app.on_ghcr_packages(ghcr_packages(), false);

        if let Modal::GhcrPicker { filter, .. } = &mut app.modal {
            filter.buffer = "SQL".to_owned();
        }

        assert_eq!(
            app.ghcr_filtered_packages(),
            vec![&"homebrew/core/sqldiff".to_owned()]
        );
    }

    /// With nothing to list, the picker is a dead end — but browsing a *known*
    /// GHCR repo needs no GitHub API call and works even anonymously, so the
    /// failure has to land on the browse-by-name fallback. This is the whole
    /// experience for a profile with no PAT, since GitHub exposes no anonymous
    /// package listing.
    #[test]
    fn on_ghcr_packages_error_falls_back_to_browse_by_name() {
        let mut app = make_ghcr_app();
        app.start_ghcr_switch(0);
        app.on_ghcr_packages_error("no GitHub token".to_owned());

        assert!(
            matches!(
                &app.modal,
                Modal::Input {
                    on_confirm: InputAction::BrowseRepo,
                    ..
                }
            ),
            "expected the browse-by-name fallback, got {:?}",
            std::mem::discriminant(&app.modal)
        );
        // The pane must not be left spinning on the state start_ghcr_switch set.
        assert!(matches!(app.repo_load, LoadState::Error(_)));
    }

    /// A refetch failing mid-browse must not throw away the list the user is
    /// still picking from.
    #[test]
    fn on_ghcr_packages_error_keeps_a_cached_list() {
        let mut app = make_ghcr_app();
        app.start_ghcr_switch(0);
        app.on_ghcr_packages(ghcr_packages(), false);

        app.on_ghcr_packages_error("503 Service Unavailable".to_owned());

        let Modal::GhcrPicker {
            packages, loading, ..
        } = &app.modal
        else {
            panic!("expected the cached picker to survive the error");
        };
        assert_eq!(packages.len(), 3);
        assert!(!loading, "spinner must stop even though the list survived");
    }

    /// The return value is what stops a pick fetching tags twice: `true` means
    /// the event loop's selection-change reload will cover it, so the caller
    /// must not also issue a `BrowseRepo`.
    #[test]
    fn select_repo_by_name_moves_the_repos_pane_selection() {
        let mut app = make_ghcr_app();
        app.start_ghcr_switch(0);
        app.on_ghcr_packages(ghcr_packages(), false);

        assert!(app.select_repo_by_name("homebrew/core/sqldiff"));
        assert_eq!(app.repos_state.selected(), Some(2));

        // A name that isn't in the pane leaves the selection alone rather than
        // clearing it or pointing past the end — and reports the miss, so the
        // caller can fall back to browsing it by name.
        assert!(!app.select_repo_by_name("absent/package"));
        assert_eq!(app.repos_state.selected(), Some(2));
    }

    // -----------------------------------------------------------------------
    // GHCR owner picker
    // -----------------------------------------------------------------------

    fn owners() -> Vec<String> {
        ["pgmac", "pgmac-net", "Homebrew"]
            .into_iter()
            .map(str::to_owned)
            .collect()
    }

    /// The typed row is what makes an owner nobody can enumerate reachable —
    /// an org the token can't see, or any account at all when browsing without
    /// a PAT. It has to lead, so `Enter` on freshly typed text does the
    /// obvious thing.
    #[test]
    fn ghcr_owner_choices_offers_the_typed_owner_first() {
        let rows = picker_choices("rust-lang", &owners());
        assert_eq!(rows[0], PickerChoice::Typed("rust-lang".to_owned()));
        assert_eq!(rows[0].label(), r#"Use "rust-lang""#);
        assert_eq!(rows[0].owner(), "rust-lang");
    }

    /// ...but not when it already names a suggestion, or the picker would show
    /// `Use "pgmac"` directly above `pgmac`.
    #[test]
    fn ghcr_owner_choices_suppresses_the_typed_row_on_an_exact_match() {
        for typed in ["pgmac", "PGMAC", "  pgmac  "] {
            let rows = picker_choices(typed, &owners());
            assert!(
                rows.iter().all(|r| matches!(r, PickerChoice::Listed(_))),
                "{typed:?} names a known owner, so no typed row"
            );
        }
    }

    /// The filter is a substring match, so a longer name containing the typed
    /// one would otherwise be ranked above it — typing a full name and pressing
    /// Enter would then open a different entry entirely.
    #[test]
    fn an_exact_match_outranks_a_longer_name_that_merely_contains_it() {
        let suggestions = vec![
            "aerofit-pgmac".to_owned(),
            "pgmac".to_owned(),
            "pgmac-net".to_owned(),
        ];

        let rows = picker_choices("pgmac", &suggestions);

        assert_eq!(rows[0], PickerChoice::Listed("pgmac".to_owned()));
        // The others stay available, just below.
        assert!(rows.contains(&PickerChoice::Listed("aerofit-pgmac".to_owned())));
        assert_eq!(
            rows.iter().filter(|r| r.owner() == "pgmac").count(),
            1,
            "the hoisted match must not also appear in the filtered tail"
        );
    }

    #[test]
    fn ghcr_owner_choices_filters_suggestions_case_insensitively() {
        let rows = picker_choices("HOME", &owners());
        assert_eq!(
            rows,
            vec![
                PickerChoice::Typed("HOME".to_owned()),
                PickerChoice::Listed("Homebrew".to_owned()),
            ]
        );
    }

    #[test]
    fn ghcr_owner_choices_lists_everything_when_empty() {
        let rows = picker_choices("", &owners());
        assert_eq!(rows.len(), 3);
        assert!(rows.iter().all(|r| matches!(r, PickerChoice::Listed(_))));
    }

    fn selected_owner(app: &App) -> &str {
        let Modal::GhcrOwnerPicker {
            owners, selected, ..
        } = &app.modal
        else {
            panic!("expected Modal::GhcrOwnerPicker");
        };
        &owners[*selected]
    }

    #[test]
    fn owner_picker_preselects_the_current_owner() {
        let mut app = make_ghcr_app();
        app.apply_ghcr_owner("pgmac-net".to_owned());
        app.on_ghcr_owners(owners());
        app.open_ghcr_owner_picker();

        assert_eq!(selected_owner(&app), "pgmac-net");
    }

    /// The real ordering: the picker opens first and the suggestions land a
    /// moment later. Before they do, the current owner is the *only* row, so
    /// clamping the old index instead of re-deriving it dropped the highlight
    /// onto row 0 exactly when the list became useful.
    #[test]
    fn owner_picker_keeps_the_current_owner_selected_when_suggestions_arrive() {
        let mut app = make_ghcr_app();
        app.apply_ghcr_owner("pgmac-net".to_owned());
        app.open_ghcr_owner_picker();

        app.on_ghcr_owners(owners());

        assert_eq!(selected_owner(&app), "pgmac-net");
    }

    /// ...but not once the user has started typing: their input drives the
    /// rows then, and row 0 is the typed owner.
    #[test]
    fn owner_picker_leaves_a_typed_selection_alone_when_suggestions_arrive() {
        let mut app = make_ghcr_app();
        app.apply_ghcr_owner("pgmac-net".to_owned());
        app.open_ghcr_owner_picker();
        if let Modal::GhcrOwnerPicker { input, .. } = &mut app.modal {
            input.buffer = "rust-lang".to_owned();
        }

        app.on_ghcr_owners(owners());

        assert_eq!(
            app.ghcr_owner_rows().first(),
            Some(&PickerChoice::Typed("rust-lang".to_owned()))
        );
        let Modal::GhcrOwnerPicker { selected, .. } = &app.modal else {
            panic!("expected Modal::GhcrOwnerPicker");
        };
        assert_eq!(*selected, 0);
    }

    /// A token scoped to just `read:packages` cannot call `/user/orgs`, so the
    /// suggestion list is routinely empty. The current owner must still be
    /// offered rather than having to be retyped.
    #[test]
    fn owner_picker_offers_the_current_owner_without_any_suggestions() {
        let mut app = make_ghcr_app();
        app.apply_ghcr_owner("Homebrew".to_owned());
        app.open_ghcr_owner_picker();

        assert_eq!(
            app.ghcr_owner_rows(),
            vec![PickerChoice::Listed("Homebrew".to_owned())]
        );
    }

    /// Suggestions are a convenience, not the input — a failed fetch stops the
    /// spinner and nothing else, leaving the text box usable.
    #[test]
    fn owner_picker_survives_a_failed_suggestion_fetch() {
        let mut app = make_ghcr_app();
        app.open_ghcr_owner_picker();
        app.on_ghcr_owners_error("403 Forbidden".to_owned());

        assert!(matches!(
            app.modal,
            Modal::GhcrOwnerPicker { loading: false, .. }
        ));
    }

    /// The package cache is keyed by owner as well as profile. Keyed by
    /// profile alone, switching owner would serve the previous owner's
    /// packages — a cache bug that would look like a GitHub API fault.
    #[test]
    fn changing_owner_does_not_serve_the_previous_owners_packages() {
        let mut app = make_ghcr_app();
        app.apply_ghcr_owner("Homebrew".to_owned());
        app.on_ghcr_packages(ghcr_packages(), false);
        assert_eq!(app.repos.len(), 3);

        app.apply_ghcr_owner("pgmac-net".to_owned());

        let Modal::GhcrPicker { packages, .. } = &app.modal else {
            panic!("expected the package picker to reopen for the new owner");
        };
        assert!(packages.is_empty(), "no cache entry for the new owner yet");
        assert!(app.repos.is_empty(), "Repos pane cleared for the new owner");
        assert_eq!(app.ghcr_owner(), Some("pgmac-net"));

        // Going back is served from cache rather than refetched.
        app.apply_ghcr_owner("Homebrew".to_owned());
        let Modal::GhcrPicker { packages, .. } = &app.modal else {
            panic!("expected the package picker");
        };
        assert_eq!(packages.len(), 3);
    }

    /// Changing owner starts over beneath it: a repo/tag from the previous
    /// owner must not linger.
    #[test]
    fn changing_owner_clears_repo_and_tag_state() {
        let mut app = make_ghcr_app();
        app.apply_ghcr_owner("Homebrew".to_owned());
        app.on_ghcr_packages(ghcr_packages(), false);
        app.start_tags_load("homebrew/core/git".to_owned());

        app.apply_ghcr_owner("pgmac-net".to_owned());

        assert!(app.current_repo.is_none());
        assert!(app.tags.is_empty());
        assert_eq!(app.repo_load, LoadState::Loading);
    }

    // -----------------------------------------------------------------------
    // Contextual help
    // -----------------------------------------------------------------------

    /// `help_context_for` is what decides which keys `?` documents. A picker
    /// mapped to the wrong context would show keys that don't apply to it, so
    /// every non-`Normal` `Modal` variant gets its own case pinned here.
    #[test]
    fn help_context_for_maps_every_modal_variant() {
        assert_eq!(
            help_context_for(&Modal::None, Focus::Tags),
            HelpContext::Normal(Focus::Tags)
        );
        assert_eq!(
            help_context_for(
                &Modal::Confirm {
                    message: "Delete?".to_owned(),
                    on_confirm: ConfirmAction::DeleteManifest {
                        repo: "r".to_owned(),
                        tag: "t".to_owned(),
                    },
                },
                Focus::Repos
            ),
            HelpContext::Normal(Focus::Repos),
            "Confirm shows its own inline keys, so it falls back to Normal"
        );
        assert_eq!(
            help_context_for(
                &Modal::Input {
                    prompt: "Name:".to_owned(),
                    input: InputState::default(),
                    on_confirm: InputAction::BrowseRepo,
                },
                Focus::Repos
            ),
            HelpContext::Normal(Focus::Repos),
            "Input is unreachable from ? (it's a legitimate character there)"
        );
        assert_eq!(
            help_context_for(
                &Modal::Inspect(Box::new(InspectModal::new(
                    "img:tag".to_owned(),
                    vec!["{}".to_owned()],
                ))),
                Focus::Tags
            ),
            HelpContext::Inspect
        );
        assert_eq!(
            help_context_for(
                &Modal::SearchPicker {
                    input: InputState::default(),
                    results: Vec::new(),
                    selected: 0,
                    searching: false,
                },
                Focus::Repos
            ),
            HelpContext::SearchPicker
        );
        assert_eq!(
            help_context_for(
                &Modal::ArtifactoryPicker {
                    filter: InputState::default(),
                    repos: Vec::new(),
                    selected: 0,
                    loading: false,
                },
                Focus::Repos
            ),
            HelpContext::FilterPicker
        );
        assert_eq!(
            help_context_for(
                &Modal::GhcrPicker {
                    filter: InputState::default(),
                    packages: Vec::new(),
                    selected: 0,
                    loading: false,
                },
                Focus::Repos
            ),
            HelpContext::FilterPicker,
            "Artifactory and GHCR packages share one picker context"
        );
        assert_eq!(
            help_context_for(
                &Modal::GhcrOwnerPicker {
                    input: InputState::default(),
                    owners: Vec::new(),
                    selected: 0,
                    loading: false,
                },
                Focus::Repos
            ),
            HelpContext::ChoicePicker,
            "the owner picker gets its own context, not FilterPicker's"
        );
        assert_eq!(
            help_context_for(
                &Modal::EcrProfilePicker {
                    input: InputState::default(),
                    profiles: Vec::new(),
                    selected: 0,
                },
                Focus::Repos
            ),
            HelpContext::ChoicePicker,
            "both ECR pickers share the owner picker's context — same bindings"
        );
        assert_eq!(
            help_context_for(
                &Modal::EcrRegionPicker {
                    input: InputState::default(),
                    regions: Vec::new(),
                    selected: 0,
                    aws_profile: None,
                },
                Focus::Repos
            ),
            HelpContext::ChoicePicker
        );
        assert_eq!(
            help_context_for(&Modal::RegistrySelect { selected_idx: 0 }, Focus::Repos),
            HelpContext::RegistrySelect
        );
        assert_eq!(
            help_context_for(
                &Modal::LayerDiff(Box::new(LayerDiffModal {
                    tag_a: "a".to_owned(),
                    tag_b: "b".to_owned(),
                    layers: Vec::new(),
                    scroll: 0,
                })),
                Focus::Repos
            ),
            HelpContext::LayerDiff
        );
        assert_eq!(
            help_context_for(
                &Modal::Help {
                    scroll: 0,
                    context: HelpContext::Inspect,
                },
                Focus::Repos
            ),
            HelpContext::Normal(Focus::Repos),
            "? cannot reach Help from Help, so this case only guards against a panic"
        );
    }

    /// `open_help` in `event.rs` reads `app.focus` at the moment `?` is
    /// pressed, so `Normal` tracks whichever panel is focused rather than
    /// always defaulting to `Repos`.
    #[test]
    fn help_context_for_normal_tracks_every_focus() {
        for focus in [Focus::Repos, Focus::Tags, Focus::Detail] {
            assert_eq!(
                help_context_for(&Modal::None, focus),
                HelpContext::Normal(focus)
            );
        }
    }
}

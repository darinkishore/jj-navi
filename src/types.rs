//! Core domain and presentation types used by `jj-navi`.

use clap::ValueEnum;
use std::fmt;
use std::path::PathBuf;
use time::OffsetDateTime;

use crate::error::{Error, Result};

/// Validated workspace name.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct WorkspaceName(String);

impl WorkspaceName {
    /// Create a validated workspace name.
    ///
    /// # Errors
    ///
    /// Returns an error if the name is empty, uses path separators, or
    /// contains whitespace.
    pub fn new(value: impl Into<String>) -> Result<Self> {
        let value = value.into();

        if value.is_empty()
            || value == "."
            || value == ".."
            || value.contains('/')
            || value.contains('\\')
            || value.chars().any(char::is_whitespace)
        {
            return Err(Error::InvalidWorkspaceName(value));
        }

        Ok(Self(value))
    }

    #[must_use]
    /// Borrow the validated workspace name as a string slice.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for WorkspaceName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Validated workspace path template.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkspaceTemplate(String);

impl WorkspaceTemplate {
    /// Create a validated workspace template.
    ///
    /// # Errors
    ///
    /// Returns an error if the template contains unsupported placeholders or
    /// unmatched braces.
    pub fn new(value: impl Into<String>) -> Result<Self> {
        let value = value.into();
        validate_workspace_template(&value)?;
        Ok(Self(value))
    }

    #[must_use]
    /// Borrow the template as a string slice.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    #[must_use]
    /// Render the template for a repo and workspace name.
    pub fn render(&self, repo: &str, workspace: &WorkspaceName) -> PathBuf {
        let mut rendered = String::new();
        let mut chars = self.0.chars().peekable();

        while let Some(ch) = chars.next() {
            if ch == '{' {
                let mut placeholder = String::new();

                for next in chars.by_ref() {
                    if next == '}' {
                        break;
                    }
                    placeholder.push(next);
                }

                match placeholder.as_str() {
                    "repo" => rendered.push_str(repo),
                    "workspace" => rendered.push_str(workspace.as_str()),
                    _ => {
                        rendered.push('{');
                        rendered.push_str(&placeholder);
                        rendered.push('}');
                    }
                }
            } else {
                rendered.push(ch);
            }
        }

        PathBuf::from(rendered)
    }
}

impl Default for WorkspaceTemplate {
    fn default() -> Self {
        Self(String::from("../{repo}.{workspace}"))
    }
}

/// Shell kinds supported by shell integration.
#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub enum ShellKind {
    /// Bash shell.
    Bash,
    /// Zsh shell.
    Zsh,
}

impl ShellKind {
    /// Parse a supported shell kind.
    ///
    /// # Errors
    ///
    /// Returns an error if the shell is not supported.
    pub fn new(value: &str) -> Result<Self> {
        match value {
            "bash" => Ok(Self::Bash),
            "zsh" => Ok(Self::Zsh),
            other => Err(Error::UnsupportedShell(other.to_owned())),
        }
    }

    /// Detect a supported shell from the `SHELL` environment variable.
    ///
    /// # Errors
    ///
    /// Returns an error if `SHELL` is missing or unsupported.
    pub fn detect() -> Result<Self> {
        let shell = std::env::var("SHELL").map_err(|_| Error::ShellDetection)?;
        let shell_name = std::path::Path::new(&shell)
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or(Error::ShellDetection)?;
        Self::new(shell_name)
    }

    #[must_use]
    /// Return the shell name used in CLI output and shell code.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Bash => "bash",
            Self::Zsh => "zsh",
        }
    }

    #[must_use]
    /// Return the shell rc filename for this shell.
    pub fn rc_file_name(self) -> &'static str {
        match self {
            Self::Bash => ".bashrc",
            Self::Zsh => ".zshrc",
        }
    }
}

/// Repo-scoped `jj-navi` configuration.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct RepoConfig {
    /// Template used when planning new workspace paths.
    pub workspace_template: WorkspaceTemplate,
    /// Lane workflow configuration.
    pub lane: LaneConfig,
}

/// Validated repo-relative lane write-set path prefix.
///
/// Normalized: forward slashes, no leading `./`, no trailing `/`, never
/// absolute, and never escaping the repo root.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct LanePath(String);

impl LanePath {
    /// Create a validated lane path prefix.
    ///
    /// # Errors
    ///
    /// Returns an error if the path is empty, absolute, escapes the repo
    /// root, or uses backslashes.
    pub fn new(value: impl Into<String>) -> Result<Self> {
        let raw = value.into();
        let trimmed = raw.trim().trim_start_matches("./").trim_end_matches('/');

        if trimmed.is_empty()
            || trimmed.starts_with('/')
            || trimmed.contains('\\')
            || trimmed == "."
            || trimmed == ".."
            || trimmed.starts_with("../")
            || trimmed.contains("/../")
            || trimmed.ends_with("/..")
            || trimmed.contains("/./")
            || trimmed.ends_with("/.")
            // Write-set paths are literal prefixes; glob syntax would be
            // accepted but silently match nothing, so reject it outright.
            || trimmed.contains('*')
            || trimmed.contains('?')
            || trimmed.contains('[')
            || trimmed.contains(']')
        {
            return Err(Error::InvalidLanePath(raw));
        }

        Ok(Self(trimmed.to_owned()))
    }

    #[must_use]
    /// Borrow the normalized path prefix as a string slice.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    #[must_use]
    /// Check whether a repo-relative file path falls under this prefix.
    pub fn contains(&self, path: &str) -> bool {
        let path = path.trim_start_matches("./");
        path == self.0 || path.strip_prefix(self.0.as_str()).is_some_and(|rest| rest.starts_with('/'))
    }

    #[must_use]
    /// Check whether two prefixes claim any common paths.
    pub fn overlaps(&self, other: &Self) -> bool {
        self.contains(other.as_str()) || other.contains(self.as_str())
    }
}

impl fmt::Display for LanePath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Durable lifecycle state of a lane. Live facts (sync, conflicts, scope
/// drift) are always derived from `jj`; only lifecycle transitions persist.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LaneLifecycle {
    /// The lane is active and may land work.
    Open,
    /// The lane landed its work and was retired.
    Closed,
    /// The lane was archived and discarded without landing.
    Abandoned,
}

impl LaneLifecycle {
    #[must_use]
    /// Stable string form used in the registry and JSON output.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Closed => "closed",
            Self::Abandoned => "abandoned",
        }
    }

    #[must_use]
    /// Parse the stable string form.
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "open" => Some(Self::Open),
            "closed" => Some(Self::Closed),
            "abandoned" => Some(Self::Abandoned),
            _ => None,
        }
    }
}

/// A revision reference reported by lane operations.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LaneRev {
    /// Commit id (short form).
    pub commit_id: String,
    /// Change id (short form).
    pub change_id: String,
    /// First line of the description.
    pub message: String,
}

/// Outcome of `lane open`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LaneOpenOutcome {
    /// Lane name.
    pub name: WorkspaceName,
    /// Created workspace directory.
    pub path: PathBuf,
    /// Trunk head the lane was based on.
    pub base: LaneRev,
    /// Declared write-set.
    pub paths: Vec<LanePath>,
    /// Whether the workspace was created sparse.
    pub sparse: bool,
}

/// One lane row in `lane list`, with live state derived from `jj`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LaneListEntry {
    /// Lane name.
    pub name: WorkspaceName,
    /// Durable lifecycle state.
    pub lifecycle: LaneLifecycle,
    /// Declared write-set.
    pub paths: Vec<LanePath>,
    /// Whether the lane's jj workspace still exists.
    pub workspace_exists: bool,
    /// Whether the lane is rebased onto the current trunk head.
    pub synced: bool,
    /// Non-empty changes the lane carries beyond trunk.
    pub ahead: usize,
    /// Trunk changes the lane has not absorbed.
    pub behind: usize,
    /// Conflicted commits in the lane chain.
    pub conflicts: usize,
    /// Changed paths outside the declared write-set.
    pub unscoped: Vec<String>,
    /// Commit id of the lane's most recent landing, if any.
    pub last_land: Option<String>,
}

/// Outcome of syncing one lane.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LaneSyncOutcome {
    /// Lane name.
    pub name: WorkspaceName,
    /// Whether the lane's jj workspace still exists.
    pub workspace_exists: bool,
    /// Whether a stale working copy had to be recovered first.
    pub recovered_stale: bool,
    /// Whether the lane was rebased onto the trunk head.
    pub rebased: bool,
    /// Conflicted commits after the rebase, if any.
    pub conflicts: Vec<LaneRev>,
    /// Out-of-scope paths dropped by `--drop-unscoped`.
    pub dropped: Vec<String>,
}

/// Fan-out result for one peer lane after a landing.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LaneFanoutEntry {
    /// Peer lane name.
    pub name: WorkspaceName,
    /// Whether the peer was rebased onto the new head.
    pub rebased: bool,
    /// Conflicted commits the rebase produced in the peer lane.
    pub conflicts: usize,
    /// Error message if the peer could not be rebased.
    pub error: Option<String>,
}

/// Outcome of `lane land`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LaneLandOutcome {
    /// Lane name.
    pub name: WorkspaceName,
    /// The landed head revision, now the trunk head.
    pub landed: LaneRev,
    /// Number of non-empty changes fast-forwarded onto trunk.
    pub landed_changes: usize,
    /// Gate command that passed, if one ran.
    pub gate: Option<String>,
    /// Per-peer fan-out results.
    pub fanout: Vec<LaneFanoutEntry>,
    /// Whether the lane was closed after landing.
    pub closed: bool,
}

/// Outcome of `lane abandon`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LaneAbandonOutcome {
    /// Lane name.
    pub name: WorkspaceName,
    /// Where the lane's diff was archived, if it had one.
    pub archive: Option<PathBuf>,
    /// Workspace directory that was deleted, if it existed.
    pub removed_directory: Option<PathBuf>,
}

/// Garbage-collection plan for ghost workspaces and orphaned lanes.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct LaneGcPlan {
    /// Workspaces registered in `jj` whose directories are gone.
    pub ghost_workspaces: Vec<WorkspaceName>,
    /// Open registry lanes with no corresponding `jj` workspace.
    pub orphaned_lanes: Vec<WorkspaceName>,
}

/// Lane workflow configuration stored in the repo config file.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LaneConfig {
    /// Workspace whose working-copy parent is the trunk head.
    pub trunk: WorkspaceName,
    /// Gate command run before every landing (via `sh -c`), if configured.
    pub gate: Option<String>,
    /// Create lane workspaces sparse, materializing only the write-set and
    /// context paths.
    pub sparse: bool,
    /// Extra read-only paths materialized into sparse lane workspaces.
    pub context_paths: Vec<LanePath>,
}

impl Default for LaneConfig {
    fn default() -> Self {
        Self {
            trunk: WorkspaceName(String::from("default")),
            gate: None,
            sparse: false,
            context_paths: Vec::new(),
        }
    }
}

/// Shared path source used by workspace health snapshots.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WorkspacePathSource {
    /// Path comes from the currently opened workspace root.
    CurrentWorkspace,
    /// Path comes from `jj workspace root --name`.
    JjRecorded,
    /// Path comes from the repo-primary root fallback.
    RepoPrimary,
    /// Path comes from validated `navi` metadata.
    NaviMetadata,
    /// Path comes from the deterministic workspace template.
    Template,
}

impl WorkspacePathSource {
    /// Whether this source is a validated fallback rather than direct JJ truth.
    #[must_use]
    pub const fn is_inferred(self) -> bool {
        matches!(self, Self::NaviMetadata | Self::Template)
    }

    /// Whether `switch` should warn when navigating via this source.
    #[must_use]
    pub const fn needs_switch_warning(self) -> bool {
        matches!(self, Self::Template)
    }

    /// Return the machine-readable label for this source.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::CurrentWorkspace => "current_workspace",
            Self::JjRecorded => "jj_recorded",
            Self::RepoPrimary => "repo_primary",
            Self::NaviMetadata => "navi_metadata",
            Self::Template => "template",
        }
    }
}

/// Presence of repo-scoped metadata for a workspace.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WorkspaceMetadataStatus {
    /// No metadata record exists for the workspace.
    MissingRecord,
    /// Metadata record exists, but it does not currently expose a path.
    PresentWithoutPath,
    /// Metadata record exists and contains a path.
    PresentWithPath,
}

impl WorkspaceMetadataStatus {
    /// Return the machine-readable label for this metadata status.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::MissingRecord => "missing_record",
            Self::PresentWithoutPath => "present_without_path",
            Self::PresentWithPath => "present_with_path",
        }
    }
}

/// Shared path snapshot for one workspace.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkspacePathSnapshot {
    /// Absolute workspace path chosen by resolution.
    pub path: PathBuf,
    /// How trustworthy the resolved path is.
    pub state: WorkspacePathState,
    /// Which source produced the chosen path.
    pub source: WorkspacePathSource,
}

/// Shared health snapshot for one workspace.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkspaceHealthSnapshot {
    /// Compact list-facing health statuses.
    pub statuses: Vec<WorkspaceListStatus>,
    /// Repo-scoped metadata presence for this workspace.
    pub metadata_status: WorkspaceMetadataStatus,
}

/// Shared repo-domain snapshot for one workspace.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkspaceSnapshot {
    /// Whether this workspace is the current working copy.
    pub is_current: bool,
    /// Workspace name.
    pub name: WorkspaceName,
    /// Resolved path snapshot.
    pub path: WorkspacePathSnapshot,
    /// Derived workspace health snapshot.
    pub health: WorkspaceHealthSnapshot,
    /// Short commit identifier.
    pub commit_id: String,
    /// Short change identifier.
    pub change_id: String,
    /// First-line commit description.
    pub message: String,
    /// Whether this workspace was made current before rendering.
    pub freshness: WorkspaceFreshnessSnapshot,
    /// Compact diff statistics for the working-copy commit.
    pub diff: WorkspaceDiffSnapshot,
    /// Workspace age metadata.
    pub age: WorkspaceAgeSnapshot,
}

/// Whether Navi made a workspace's JJ state current before rendering it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkspaceFreshnessSnapshot {
    /// Machine-readable freshness state.
    pub status: WorkspaceFreshnessStatus,
    /// Optional user-facing reason when freshness could not be established.
    pub reason: Option<String>,
}

impl WorkspaceFreshnessSnapshot {
    /// Return a successful freshness snapshot.
    #[must_use]
    pub const fn current() -> Self {
        Self {
            status: WorkspaceFreshnessStatus::Current,
            reason: None,
        }
    }

    /// Return a freshness snapshot for a skipped missing path.
    #[must_use]
    pub fn skipped_missing() -> Self {
        Self {
            status: WorkspaceFreshnessStatus::SkippedMissing,
            reason: Some(String::from("workspace path is missing")),
        }
    }

    /// Return a freshness snapshot for a skipped stale path.
    #[must_use]
    pub fn skipped_stale() -> Self {
        Self {
            status: WorkspaceFreshnessStatus::SkippedStale,
            reason: Some(String::from("workspace path is stale")),
        }
    }

    /// Return a freshness snapshot for a skipped untrusted path.
    #[must_use]
    pub fn skipped_untrusted() -> Self {
        Self {
            status: WorkspaceFreshnessStatus::SkippedUntrusted,
            reason: Some(String::from("workspace path is not trusted")),
        }
    }

    /// Return a failed freshness snapshot.
    #[must_use]
    pub fn failed(reason: impl Into<String>) -> Self {
        Self {
            status: WorkspaceFreshnessStatus::Failed,
            reason: Some(reason.into()),
        }
    }

    /// Return a timed out freshness snapshot.
    #[must_use]
    pub fn timed_out() -> Self {
        Self {
            status: WorkspaceFreshnessStatus::TimedOut,
            reason: Some(String::from(
                "workspace could not be made current before the deadline",
            )),
        }
    }
}

impl Default for WorkspaceFreshnessSnapshot {
    fn default() -> Self {
        Self::current()
    }
}

/// Machine-readable workspace freshness state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WorkspaceFreshnessStatus {
    /// Workspace was made current before rendering.
    Current,
    /// Workspace path is missing and could not be refreshed.
    SkippedMissing,
    /// Workspace path is stale and could not be refreshed safely.
    SkippedStale,
    /// Workspace path is not trusted enough to run JJ in it.
    SkippedUntrusted,
    /// JJ failed while making the workspace current.
    Failed,
    /// JJ exceeded Navi's deadline while making the workspace current.
    TimedOut,
}

impl WorkspaceFreshnessStatus {
    /// Return the machine-readable freshness label.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Current => "current",
            Self::SkippedMissing => "skipped_missing",
            Self::SkippedStale => "skipped_stale",
            Self::SkippedUntrusted => "skipped_untrusted",
            Self::Failed => "failed",
            Self::TimedOut => "timed_out",
        }
    }
}

/// Compact diff statistics for a workspace's working-copy commit.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkspaceDiffSnapshot {
    /// Whether diff statistics were available.
    pub status: WorkspaceDiffStatus,
    /// Number of changed files, when known.
    pub files_changed: Option<u32>,
    /// Number of inserted lines, when known.
    pub insertions: Option<u32>,
    /// Number of deleted lines, when known.
    pub deletions: Option<u32>,
}

impl WorkspaceDiffSnapshot {
    /// Return unknown diff statistics.
    #[must_use]
    pub const fn unknown() -> Self {
        Self {
            status: WorkspaceDiffStatus::Unknown,
            files_changed: None,
            insertions: None,
            deletions: None,
        }
    }
}

impl Default for WorkspaceDiffSnapshot {
    fn default() -> Self {
        Self::unknown()
    }
}

/// Whether workspace diff statistics were available.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WorkspaceDiffStatus {
    /// Diff statistics were collected.
    Available,
    /// Diff statistics could not be collected.
    Unknown,
}

impl WorkspaceDiffStatus {
    /// Return the machine-readable diff status label.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Available => "available",
            Self::Unknown => "unknown",
        }
    }
}

/// Workspace creation metadata used for compact age rendering.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkspaceAgeSnapshot {
    /// Creation timestamp recorded by Navi, when available.
    pub created_at: Option<OffsetDateTime>,
}

impl WorkspaceAgeSnapshot {
    /// Return an unknown age snapshot.
    #[must_use]
    pub const fn unknown() -> Self {
        Self { created_at: None }
    }
}

impl Default for WorkspaceAgeSnapshot {
    fn default() -> Self {
        Self::unknown()
    }
}

/// Render-ready workspace row for `navi list`.
///
/// This stays as a human-output adapter, not the shared repo-domain model.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkspaceListEntry {
    /// Whether this row represents the active workspace.
    pub is_current: bool,
    /// Workspace name.
    pub name: WorkspaceName,
    /// Compact status labels shown in the list table.
    pub statuses: Vec<WorkspaceListStatus>,
    /// Display path shown in the table.
    pub path: PathBuf,
    /// How trustworthy the rendered path is.
    pub path_state: WorkspacePathState,
    /// Short commit identifier.
    pub commit_id: String,
    /// Short change identifier.
    pub change_id: String,
    /// First-line commit description.
    pub message: String,
    /// Whether this workspace was made current before rendering.
    pub freshness: WorkspaceFreshnessSnapshot,
    /// Compact diff statistics for the working-copy commit.
    pub diff: WorkspaceDiffSnapshot,
    /// Workspace age metadata.
    pub age: WorkspaceAgeSnapshot,
}

/// Executable merge operation across JJ workspaces.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkspaceMerge {
    /// Workspace that contains work to bring over.
    pub source: WorkspaceMergeSide,
    /// Workspace that should receive the duplicated work.
    pub target: WorkspaceMergeSide,
    /// Non-empty source revisions selected for duplicate/rebase.
    pub revisions: Vec<WorkspaceMergeRevision>,
    /// Source root commit used as the rebase source after duplication.
    pub source_root_commit_id: String,
    /// Source head commit used to place the target workspace after rebase.
    pub source_head_commit_id: String,
}

/// Snapshot details included for each side of a workspace merge.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkspaceMergeSide {
    /// Workspace snapshot after freshness validation.
    pub snapshot: WorkspaceSnapshot,
    /// Display path relative to the currently opened workspace.
    pub display_path: PathBuf,
}

/// One non-empty source revision selected for workspace merge.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkspaceMergeRevision {
    /// Short commit identifier.
    pub commit_id: String,
    /// Short change identifier.
    pub change_id: String,
    /// First-line commit description.
    pub message: String,
}

/// Completed workspace merge details.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkspaceMergeOutcome {
    /// Prepared operation inputs.
    pub merge: WorkspaceMerge,
    /// Change ID created by `jj duplicate` for the source root.
    pub duplicated_root_change_id: String,
    /// Change ID created by `jj duplicate` for the source head.
    pub duplicated_head_change_id: String,
    /// Raw `jj duplicate` diagnostic output.
    pub duplicate_output: String,
    /// Raw `jj rebase` diagnostic output.
    pub rebase_output: String,
    /// Raw `jj new` diagnostic output from updating the target workspace.
    pub new_output: String,
}

/// Role of a workspace in merge validation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WorkspaceMergeRole {
    /// Source workspace role.
    Source,
    /// Target workspace role.
    Target,
}

impl WorkspaceMergeRole {
    /// Return the human-facing role label.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Source => "source",
            Self::Target => "target",
        }
    }
}

impl fmt::Display for WorkspaceMergeRole {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

/// Display state for a workspace path rendered by `navi list`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WorkspacePathState {
    /// Path is confirmed from the current workspace or JJ.
    Confirmed,
    /// Path was inferred from validated `navi` fallback data.
    Inferred,
    /// Best known path does not exist on disk.
    Missing,
    /// Best known path exists but no longer validates.
    Stale,
}

impl WorkspacePathState {
    /// Return the machine-readable label for this path state.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Confirmed => "confirmed",
            Self::Inferred => "inferred",
            Self::Missing => "missing",
            Self::Stale => "stale",
        }
    }
}

/// Compact status label rendered by `navi list`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WorkspaceListStatus {
    /// Workspace looks healthy.
    Ok,
    /// Workspace path came from validated fallback data.
    Inferred,
    /// Best known workspace path is missing.
    Missing,
    /// Best known workspace path is stale.
    Stale,
    /// JJ knows the workspace but `navi` metadata does not.
    JjOnly,
    /// Workspace could not be made current before rendering.
    NotCurrent,
}

impl WorkspaceListStatus {
    /// Return the human-facing label for this status.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Inferred => "inferred",
            Self::Missing => "missing",
            Self::Stale => "stale",
            Self::JjOnly => "jj-only",
            Self::NotCurrent => "not-current",
        }
    }
}

fn validate_workspace_template(value: &str) -> Result<()> {
    let mut chars = value.chars().peekable();

    while let Some(ch) = chars.next() {
        match ch {
            '{' => {
                let mut placeholder = String::new();

                loop {
                    match chars.next() {
                        Some('}') => break,
                        Some(next) => placeholder.push(next),
                        None => {
                            return Err(Error::InvalidWorkspaceTemplate(value.to_owned()));
                        }
                    }
                }

                if placeholder != "repo" && placeholder != "workspace" {
                    return Err(Error::InvalidWorkspaceTemplate(value.to_owned()));
                }
            }
            '}' => return Err(Error::InvalidWorkspaceTemplate(value.to_owned())),
            _ => {}
        }
    }

    Ok(())
}

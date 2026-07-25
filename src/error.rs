use std::path::PathBuf;

use thiserror::Error;

use crate::types::WorkspaceMergeRole;

/// Crate-wide error type for CLI, discovery, and `jj` integration failures.
#[derive(Debug, Error)]
pub enum Error {
    /// The current directory is not inside a Jujutsu workspace.
    #[error("error: not in a jj workspace")]
    NotInWorkspace,

    /// A workspace name violates `jj-navi` validation rules.
    #[error("error: invalid workspace name '{0}'")]
    InvalidWorkspaceName(String),

    /// The current directory still contains `.jj`, but is no longer a live workspace.
    #[error(
        "error: current directory is no longer a registered jj workspace\nhint: cd into another workspace or recreate this workspace with jj"
    )]
    OrphanedWorkspace,

    /// The repo name could not be derived from the current workspace root.
    #[error("error: failed to determine repo name")]
    RepoName,

    /// The workspace root unexpectedly has no parent directory.
    #[error("error: workspace root has no parent: {0}")]
    WorkspaceRootHasNoParent(PathBuf),

    /// The requested workspace does not exist.
    #[error("error: workspace does not exist\nhint: use --create")]
    WorkspaceDoesNotExist,

    /// The named workspace does not exist in `jj`.
    #[error("error: workspace '{0}' does not exist")]
    WorkspaceNotFound(String),

    /// The workspace exists, but no validated directory could be found.
    #[error(
        "error: workspace '{workspace}' exists, but its directory could not be resolved\nhint: last known path: {path}"
    )]
    WorkspaceDirectoryUnavailable {
        /// Workspace name.
        workspace: String,
        /// Best-known display path.
        path: String,
    },

    /// Removing the current workspace would orphan the active directory.
    #[error("error: cannot remove current workspace\nhint: switch to another workspace first")]
    CannotRemoveCurrentWorkspace,

    /// Removing this workspace directory would remove shared repo storage.
    #[error(
        "error: cannot remove workspace '{workspace}' because its directory contains shared jj repo storage\nhint: navi remove only deletes workspaces whose directory does not own shared repo storage: {path}"
    )]
    CannotRemoveWorkspaceWithSharedRepoStorage {
        /// Workspace name.
        workspace: String,
        /// Directory that owns shared repo storage.
        path: String,
    },

    /// The user declined a destructive workspace removal prompt.
    #[error("error: remove cancelled\nhint: rerun with --yes to skip confirmation")]
    RemoveCancelled,

    /// Merge would compare a workspace with itself.
    #[error(
        "error: cannot merge workspace '{0}' into itself\nhint: choose a different --into workspace"
    )]
    MergeSameWorkspace(String),

    /// Merge could not find the requested workspace.
    #[error(
        "error: merge {role} workspace '{workspace}' does not exist\nhint: run navi list and choose an existing workspace"
    )]
    MergeWorkspaceMissing {
        /// Source or target role.
        role: WorkspaceMergeRole,
        /// Requested workspace name.
        workspace: String,
    },

    /// Merge found more than one requested workspace.
    #[error(
        "error: merge {role} workspace '{workspace}' is ambiguous\nhint: inspect jj workspace list before merging"
    )]
    MergeWorkspaceAmbiguous {
        /// Source or target role.
        role: WorkspaceMergeRole,
        /// Requested workspace name.
        workspace: String,
    },

    /// Merge found an unsafe workspace state.
    #[error(
        "error: merge {role} workspace '{workspace}' is not ready: {reason}\nhint: run navi list and fix the workspace before merging"
    )]
    MergeWorkspaceUnavailable {
        /// Source or target role.
        role: WorkspaceMergeRole,
        /// Requested workspace name.
        workspace: String,
        /// Reason the workspace is unsafe.
        reason: String,
    },

    /// Merge source has no non-empty work to duplicate.
    #[error(
        "error: merge source workspace '{source_workspace}' has no non-empty changes not already in target workspace '{target}'"
    )]
    MergeSourceEmpty {
        /// Source workspace name.
        source_workspace: String,
        /// Target workspace name.
        target: String,
    },

    /// Merge source has a shape this command does not handle safely yet.
    #[error(
        "error: merge source workspace '{source_workspace}' has multiple independent roots relative to target workspace '{target}'\nhint: merge one linear workspace stack at a time"
    )]
    MergeSourceMultipleRoots {
        /// Source workspace name.
        source_workspace: String,
        /// Target workspace name.
        target: String,
    },

    /// Merge source has a shape this command does not handle safely yet.
    #[error(
        "error: merge source workspace '{source_workspace}' has multiple independent heads relative to target workspace '{target}'\nhint: merge one linear workspace stack at a time"
    )]
    MergeSourceMultipleHeads {
        /// Source workspace name.
        source_workspace: String,
        /// Target workspace name.
        target: String,
    },

    /// `jj duplicate` succeeded but Navi could not identify the duplicated root.
    #[error(
        "error: duplicated source workspace '{source_workspace}', but could not identify the duplicated root change\nhint: rebase was not attempted; inspect the new duplicate with jj log"
    )]
    MergeDuplicateRootUnknown {
        /// Source workspace name.
        source_workspace: String,
    },

    /// `jj duplicate` succeeded but Navi could not identify the duplicated head.
    #[error(
        "error: duplicated source workspace '{source_workspace}', but could not identify the duplicated head change\nhint: rebase was not attempted; inspect the new duplicate with jj log"
    )]
    MergeDuplicateHeadUnknown {
        /// Source workspace name.
        source_workspace: String,
    },

    /// `jj rebase` failed after duplication.
    #[error(
        "error: merge stopped during rebase\nhint: duplicated work remains in the repo and source workspace was not rewritten; run jj resolve --list, resolve conflicts, then jj squash\n{stderr}"
    )]
    MergeRebaseFailed {
        /// Rebase stderr.
        stderr: String,
    },

    /// Directory deletion failed after the workspace was already forgotten.
    #[error(
        "error: failed to delete workspace directory after forgetting workspace '{workspace}'\nhint: jj no longer tracks this workspace and navi metadata was removed; inspect and delete manually: {path}\n{source}"
    )]
    WorkspaceDirectoryDeleteAfterForgetFailed {
        /// Workspace name.
        workspace: String,
        /// Directory that could not be deleted.
        path: String,
        /// Underlying filesystem error.
        source: std::io::Error,
    },

    /// The `.jj/repo` pointer file is empty or points to a non-directory.
    #[error("error: invalid repo pointer in {0}")]
    InvalidRepoPointer(PathBuf),

    /// The `.jj/repo` pointer could not be resolved to an on-disk path.
    #[error("error: invalid repo pointer in {path}\n{message}")]
    RepoPointerResolution {
        /// Path to the pointer file that failed to resolve.
        path: PathBuf,
        /// Underlying resolution error message.
        message: String,
    },

    /// The configured workspace template is syntactically invalid.
    #[error("error: invalid workspace template '{0}'")]
    InvalidWorkspaceTemplate(String),

    /// Repo config could not be parsed or validated.
    #[error("error: invalid repo config in {path}\n{message}")]
    InvalidRepoConfig {
        /// Config file path.
        path: PathBuf,
        /// Validation or parse message.
        message: String,
    },

    /// Repo-scoped navi state could not be parsed or validated.
    #[error("error: invalid repo state in {path}\n{message}")]
    InvalidRepoState {
        /// State file path.
        path: PathBuf,
        /// Validation or parse message.
        message: String,
    },

    /// No previous workspace has been recorded for this repo.
    #[error(
        "error: no previous workspace recorded for this repository\nhint: switch to a different workspace first"
    )]
    NoPreviousWorkspace,

    /// The recorded previous workspace no longer exists in this repo.
    #[error(
        "error: previous workspace '{0}' no longer exists in this repository\nhint: switch to an existing workspace first"
    )]
    PreviousWorkspaceNotFound(String),

    /// The repo-primary workspace could not be resolved.
    #[error(
        "error: primary workspace could not be resolved\nhint: run navi list and switch by workspace name"
    )]
    PrimaryWorkspaceUnavailable,

    /// A symbolic switch target was used where a workspace name is required.
    #[error(
        "error: '{0}' is a reserved switch target\nhint: use a workspace name with --create or --revision"
    )]
    ReservedSwitchTarget(String),

    /// Workspace metadata could not be parsed or validated.
    #[error("error: invalid workspace metadata in {path}\n{message}")]
    InvalidWorkspaceMetadata {
        /// Metadata file path.
        path: PathBuf,
        /// Validation or parse message.
        message: String,
    },

    /// `jj workspace list` returned output that `jj-navi` could not parse.
    #[error("error: invalid jj workspace list entry\n{0}")]
    InvalidJjWorkspaceListEntry(String),

    /// The requested shell is not supported.
    #[error("error: unsupported shell '{0}'")]
    UnsupportedShell(String),

    /// A shell argument is required for shell-init generation.
    #[error("error: shell name required\nhint: use one of: bash, zsh")]
    ShellRequired,

    /// The current shell could not be inferred from `$SHELL`.
    #[error("error: unable to detect shell from $SHELL")]
    ShellDetection,

    /// `$HOME` is required for shell installation.
    #[error("error: $HOME is not set")]
    HomeDirectory,

    /// The target shell rc file contains an invalid managed block.
    #[error("error: invalid shell rc file at {path}\n{message}")]
    InvalidShellRcFile {
        /// Shell rc path.
        path: PathBuf,
        /// Validation message.
        message: &'static str,
    },

    /// Shell integration requires a UTF-8 renderable path.
    #[error("error: shell integration requires a UTF-8 workspace path")]
    ShellDirectivePathNotUtf8,

    /// A `jj` command failed.
    #[error("error: jj command failed: {command}\n{stderr}")]
    JjCommandFailed {
        /// Rendered `jj` command line.
        command: String,
        /// Trimmed stderr output from `jj`.
        stderr: String,
    },

    /// The installed `jj` version is older than the supported floor.
    #[error("error: jj {minimum} or newer required\nhint: found {found}")]
    UnsupportedJjVersion {
        /// Installed `jj --version` output.
        found: String,
        /// Minimum supported version.
        minimum: &'static str,
    },

    /// JSON output could not be serialized.
    #[error("error: failed to serialize json output\n{0}")]
    JsonSerialization(String),

    /// A lane write-set path violates validation rules.
    #[error(
        "error: invalid lane path '{0}'\nhint: use repo-relative path prefixes like src/module or docs/guide.md"
    )]
    InvalidLanePath(String),

    /// The lane registry could not be parsed or validated.
    #[error("error: invalid lane registry in {path}\n{message}")]
    InvalidLaneRegistry {
        /// Registry file path.
        path: PathBuf,
        /// Validation or parse message.
        message: String,
    },

    /// A lane with this name is already open.
    #[error("error: lane '{0}' is already open\nhint: run navi lane list")]
    LaneExists(String),

    /// The named lane is not in the registry.
    #[error("error: lane '{0}' is not registered\nhint: run navi lane list")]
    LaneNotFound(String),

    /// The named lane is registered but no longer open.
    #[error("error: lane '{name}' is {lifecycle}, not open")]
    LaneNotOpen {
        /// Lane name.
        name: String,
        /// Terminal lifecycle state.
        lifecycle: &'static str,
    },

    /// The lane name is reserved for the trunk workspace.
    #[error("error: '{0}' is the trunk workspace and cannot be a lane")]
    LaneNameReserved(String),

    /// Two lanes would claim overlapping write-set paths.
    #[error(
        "error: lane path '{path}' overlaps open lane '{other}' (its path '{other_path}')\nhint: coordinate with that lane or rerun with --allow-overlap"
    )]
    LaneOverlap {
        /// Requested path.
        path: String,
        /// Existing open lane owning the overlap.
        other: String,
        /// The overlapping path in the existing lane.
        other_path: String,
    },

    /// The configured trunk workspace does not exist.
    #[error("error: trunk workspace '{0}' does not exist\nhint: check [lane] trunk in navi config")]
    LaneTrunkMissing(String),

    /// The trunk working copy is not in a landable state.
    #[error("error: trunk workspace '{trunk}' is not ready: {reason}")]
    LaneTrunkNotReady {
        /// Trunk workspace name.
        trunk: String,
        /// Why the trunk cannot accept a landing.
        reason: String,
    },

    /// Trunk working-copy dirt intersects the lane's write-set.
    #[error(
        "error: trunk working copy has uncommitted changes inside lane '{lane}' write-set:\n{paths}\nhint: land or restore those trunk changes first; unrelated trunk dirt does not block landing"
    )]
    LaneTrunkDirtyInScope {
        /// Lane name.
        lane: String,
        /// Newline-joined offending paths.
        paths: String,
    },

    /// The lane workspace no longer exists in `jj`.
    #[error(
        "error: lane '{0}' has no jj workspace\nhint: run navi lane gc to reconcile the registry"
    )]
    LaneWorkspaceMissing(String),

    /// The lane is not rebased onto the current trunk head.
    #[error(
        "error: lane '{lane}' is not synced onto the trunk head ({behind} trunk change(s) missing)\nhint: run navi lane sync {lane}"
    )]
    LaneNotSynced {
        /// Lane name.
        lane: String,
        /// Number of trunk changes the lane has not absorbed.
        behind: usize,
    },

    /// The lane chain still contains conflicted commits.
    #[error(
        "error: lane '{lane}' has {count} conflicted change(s)\nhint: resolve conflicts in the lane workspace, then retry"
    )]
    LaneConflicted {
        /// Lane name.
        lane: String,
        /// Conflicted commit count.
        count: usize,
    },

    /// The lane has no work to land.
    #[error("error: lane '{0}' has no changes to land")]
    LaneNothingToLand(String),

    /// The landing head has no description and no message was provided.
    #[error(
        "error: lane '{0}' head has no description\nhint: rerun with -m to describe the landing"
    )]
    LaneNeedsMessage(String),

    /// The lane diff touches paths outside its declared write-set.
    #[error(
        "error: lane '{lane}' has changes outside its write-set:\n{paths}\nhint: extend the lane with navi lane claim, or drop them with navi lane sync {lane} --drop-unscoped"
    )]
    LaneUnscopedChanges {
        /// Lane name.
        lane: String,
        /// Newline-joined offending paths.
        paths: String,
    },

    /// The configured gate command rejected the landing.
    #[error(
        "error: gate command failed ({command})\nhint: fix the lane and retry, or land with --no-gate if the gate itself is broken"
    )]
    LaneGateFailed {
        /// Gate command line.
        command: String,
    },

    /// Closing a lane requires it to be fully landed.
    #[error(
        "error: lane '{lane}' still has unlanded work\nhint: land it first, or use navi lane abandon to archive and discard"
    )]
    LaneNotLanded {
        /// Lane name.
        lane: String,
    },

    /// Another navi process held the repo mutation lock past the timeout.
    #[error(
        "error: another navi operation is holding the repo lock at {path}\nhint: waited {waited_ms}ms; retry, or raise NAVI_LOCK_TIMEOUT_MS"
    )]
    MutationLockTimeout {
        /// Lock file path.
        path: String,
        /// How long this process waited before giving up.
        waited_ms: u128,
    },

    /// The trunk head moved while a landing was in flight (for example while
    /// the gate was running).
    #[error(
        "error: trunk head moved while landing lane '{lane}' (was {expected}, now {found})\nhint: run navi lane sync {lane}, then land again"
    )]
    LaneTrunkMoved {
        /// Lane name.
        lane: String,
        /// Trunk head the landing validated against.
        expected: String,
        /// Trunk head observed after the gate.
        found: String,
    },

    /// `lane land --close` cannot delete the workspace the command runs in.
    #[error(
        "error: cannot close lane '{0}' from inside its own workspace\nhint: run from the trunk workspace, or omit --close"
    )]
    LaneCloseFromInside(String),

    /// `switch --revision` conflicts with an existing workspace.
    #[error(
        "error: workspace '{workspace}' already exists; --revision only applies when creating\nhint: drop -r to switch, or pick a new workspace name"
    )]
    WorkspaceExistsWithRevision {
        /// Existing workspace name.
        workspace: String,
    },

    /// An underlying I/O operation failed.
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// Crate-wide result alias.
pub type Result<T> = std::result::Result<T, Error>;

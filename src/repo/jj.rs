use std::ffi::OsString;
use std::fs;
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use crate::error::{Error, Result};
use crate::types::{
    WorkspaceDiffSnapshot, WorkspaceDiffStatus, WorkspaceFreshnessSnapshot, WorkspaceName,
};

const DEFAULT_WORKSPACE_CURRENT_TIMEOUT: Duration = Duration::from_secs(2);
const DEFAULT_WORKSPACE_DIFF_TIMEOUT: Duration = Duration::from_secs(2);
static TEMP_OUTPUT_COUNTER: AtomicU64 = AtomicU64::new(0);

fn workspace_current_timeout() -> Duration {
    timeout_from_env("NAVI_SNAPSHOT_TIMEOUT_MS", DEFAULT_WORKSPACE_CURRENT_TIMEOUT)
}

fn workspace_diff_timeout() -> Duration {
    timeout_from_env("NAVI_DIFF_TIMEOUT_MS", DEFAULT_WORKSPACE_DIFF_TIMEOUT)
}

fn timeout_from_env(var: &str, default: Duration) -> Duration {
    std::env::var(var)
        .ok()
        .and_then(|value| value.trim().parse::<u64>().ok())
        .map_or(default, Duration::from_millis)
}

/// Render a workspace name as a safely quoted revset symbol (`"name"@`).
///
/// Unquoted interpolation lets characters like `|`, `~`, or `::` in a
/// workspace name change the meaning of the surrounding revset.
pub(crate) fn workspace_revset_symbol(name: &WorkspaceName) -> String {
    format!("{}@", quote_revset_string(name.as_str()))
}

fn quote_revset_string(value: &str) -> String {
    let mut quoted = String::with_capacity(value.len() + 2);
    quoted.push('"');
    for ch in value.chars() {
        match ch {
            '"' => quoted.push_str("\\\""),
            '\\' => quoted.push_str("\\\\"),
            other => quoted.push(other),
        }
    }
    quoted.push('"');
    quoted
}

/// Render a repo-relative path as an exact-match `file:` fileset pattern.
///
/// `jj` parses positional path arguments as fileset expressions, so raw
/// paths containing `&`, `~`, `*`, or quotes either error or silently match
/// the wrong set of files.
pub(crate) fn fileset_exact_pattern(path: &str) -> String {
    format!("file:{}", quote_revset_string(path))
}

/// Undo jj template string quoting on ingested names.
///
/// `jj workspace list -T name` renders names that need quoting (for example
/// `a|b`) wrapped in double quotes with backslash escapes.
fn unquote_template_string(value: &str) -> String {
    let inner = value
        .strip_prefix('"')
        .and_then(|rest| rest.strip_suffix('"'));
    let Some(inner) = inner else {
        return value.to_owned();
    };

    let mut unquoted = String::with_capacity(inner.len());
    let mut chars = inner.chars();
    while let Some(ch) = chars.next() {
        if ch == '\\' {
            match chars.next() {
                Some('n') => unquoted.push('\n'),
                Some('r') => unquoted.push('\r'),
                Some('t') => unquoted.push('\t'),
                Some('0') => unquoted.push('\0'),
                Some(other) => unquoted.push(other),
                None => unquoted.push('\\'),
            }
        } else {
            unquoted.push(ch);
        }
    }
    unquoted
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct JjWorkspaceListEntry {
    pub(crate) name: WorkspaceName,
    pub(crate) is_current: bool,
    pub(crate) commit_id: String,
    pub(crate) change_id: String,
    pub(crate) message: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct JjRevisionSummary {
    pub(crate) commit_id: String,
    pub(crate) change_id: String,
    pub(crate) message: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct JjCommandOutput {
    pub(crate) stdout: String,
    pub(crate) stderr: String,
}

pub(crate) struct JjClient<'a> {
    workspace_root: &'a Path,
}

pub(crate) fn config_list(path: &Path, name: &str) -> Option<String> {
    let output = jj_command()
        .args([
            OsString::from("config"),
            OsString::from("list"),
            OsString::from("--include-defaults"),
            OsString::from(name),
        ])
        .current_dir(path)
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    Some(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Resolve the user's full jj configuration (with defaults) as TOML text,
/// exactly as their `jj` binary sees it. Used to give the embedded engine
/// config parity.
pub(crate) fn config_list_all(path: &Path) -> Result<String> {
    let args = [
        OsString::from("config"),
        OsString::from("list"),
        OsString::from("--include-defaults"),
    ];
    let output = jj_command().args(&args).current_dir(path).output()?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        Err(Error::JjCommandFailed {
            command: format_command(&args),
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        })
    }
}

pub(crate) fn snapshot_working_copy_at(path: &Path) -> WorkspaceFreshnessSnapshot {
    let args = [
        OsString::from("--quiet"),
        OsString::from("util"),
        OsString::from("snapshot"),
    ];
    let timeout = workspace_current_timeout();

    let mut result = run_with_timeout(path, &args, timeout);
    // Umbrella: a stale working copy is weather, not an error. Recover it
    // once and retry before reporting failure.
    if let TimedCommandResult::Failure(stderr) = &result
        && stderr_reports_stale(stderr)
    {
        let update = [
            OsString::from("workspace"),
            OsString::from("update-stale"),
        ];
        if matches!(
            run_with_timeout(path, &update, timeout),
            TimedCommandResult::Success(_)
        ) {
            result = run_with_timeout(path, &args, timeout);
        }
    }

    match result {
        TimedCommandResult::Success(_) => WorkspaceFreshnessSnapshot::current(),
        TimedCommandResult::Failure(stderr) => WorkspaceFreshnessSnapshot::failed(
            meaningful_stderr(&stderr, "jj could not make the workspace current"),
        ),
        TimedCommandResult::TimedOut => WorkspaceFreshnessSnapshot::timed_out(),
        TimedCommandResult::Io(error) => WorkspaceFreshnessSnapshot::failed(format!(
            "failed to run jj while making the workspace current: {error}"
        )),
    }
}

pub(crate) fn diff_stat_at(path: &Path) -> WorkspaceDiffSnapshot {
    let args = [
        OsString::from("--ignore-working-copy"),
        OsString::from("diff"),
        OsString::from("--stat"),
        OsString::from("-r"),
        OsString::from("@"),
    ];

    match run_with_timeout(path, &args, workspace_diff_timeout()) {
        TimedCommandResult::Success(stdout) => parse_diff_stat(&stdout),
        TimedCommandResult::Failure(_)
        | TimedCommandResult::TimedOut
        | TimedCommandResult::Io(_) => WorkspaceDiffSnapshot::unknown(),
    }
}

const MINIMUM_JJ_VERSION: JjVersion = JjVersion {
    major: 0,
    minor: 39,
    patch: 0,
};
const MINIMUM_JJ_VERSION_STR: &str = "0.39.0";

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) struct JjVersion {
    pub(crate) major: u64,
    pub(crate) minor: u64,
    pub(crate) patch: u64,
}

impl<'a> JjClient<'a> {
    pub(crate) fn new(workspace_root: &'a Path) -> Self {
        Self { workspace_root }
    }

    pub(crate) fn ensure_supported_version(&self) -> Result<()> {
        if std::env::var_os("NAVI_NO_VERSION_CHECK").is_some_and(|value| !value.is_empty()) {
            return Ok(());
        }

        let args = [OsString::from("--version")];
        let output = self.run(&args)?;
        let found = output.trim().to_owned();
        let Some(version) = parse_jj_version(&output) else {
            return Err(Error::UnsupportedJjVersion {
                found,
                minimum: MINIMUM_JJ_VERSION_STR,
            });
        };

        if version < MINIMUM_JJ_VERSION {
            return Err(Error::UnsupportedJjVersion {
                found,
                minimum: MINIMUM_JJ_VERSION_STR,
            });
        }

        Ok(())
    }

    pub(crate) fn current_workspace_name(&self) -> Result<WorkspaceName> {
        let output = self.run_ignoring_working_copy(&[
            OsString::from("workspace"),
            OsString::from("list"),
            OsString::from("-T"),
            OsString::from("if(target.current_working_copy(), name ++ \"\\n\", \"\")"),
        ])?;

        let name = output
            .lines()
            .find(|line| !line.is_empty())
            .ok_or(Error::OrphanedWorkspace)?;

        WorkspaceName::new(unquote_template_string(name))
    }

    pub(crate) fn list_workspaces(&self) -> Result<Vec<JjWorkspaceListEntry>> {
        let output = self.run_ignoring_working_copy(&[
            OsString::from("workspace"),
            OsString::from("list"),
            OsString::from("-T"),
            OsString::from(
                "name ++ \"\\0\" ++ if(target.current_working_copy(), \"1\", \"0\") ++ \"\\0\" ++ target.commit_id().short(12) ++ \"\\0\" ++ target.change_id().short(12) ++ \"\\0\" ++ target.description().first_line() ++ \"\\n\"",
            ),
        ])?;

        output
            .lines()
            .filter(|line| !line.is_empty())
            .map(parse_workspace_line)
            .collect()
    }

    pub(crate) fn workspace_forget(&self, workspace: &WorkspaceName) -> Result<()> {
        self.run(&[
            OsString::from("workspace"),
            OsString::from("forget"),
            OsString::from(workspace.as_str()),
        ])
        .map(|_| ())
    }

    pub(crate) fn workspace_add(
        &self,
        workspace: &WorkspaceName,
        target_root: &Path,
        revision: Option<&str>,
    ) -> Result<()> {
        let args = workspace_add_args(workspace, target_root, revision);

        self.run(&args).map(|_| ())
    }

    pub(crate) fn revisions(&self, revset: &str) -> Result<Vec<JjRevisionSummary>> {
        let output = self.run_ignoring_working_copy(&[
            OsString::from("log"),
            OsString::from("-r"),
            OsString::from(revset),
            OsString::from("--no-graph"),
            OsString::from("-T"),
            OsString::from(
                "commit_id.short(12) ++ \"\\0\" ++ change_id.short(12) ++ \"\\0\" ++ description.first_line() ++ \"\\n\"",
            ),
        ])?;

        output
            .lines()
            .filter(|line| !line.is_empty())
            .map(parse_revision_line)
            .collect()
    }

    pub(crate) fn duplicate(&self, revset: &str) -> Result<JjCommandOutput> {
        self.run_capture(&[OsString::from("duplicate"), OsString::from(revset)])
    }

    /// Resolve one conflicted file in `revision` to prepared content.
    ///
    /// Uses a one-shot configured merge tool that copies `prepared` into
    /// place, so jj performs the commit rewrite, descendant rebases, and
    /// op-log entry natively.
    pub(crate) fn resolve_with_prepared_file(
        &self,
        revision: &str,
        path: &str,
        prepared: &Path,
    ) -> Result<()> {
        let prepared = prepared.to_str().ok_or_else(|| Error::Engine {
            message: String::from("prepared resolution path is not UTF-8"),
        })?;
        self.run(&[
            OsString::from("--config"),
            OsString::from("merge-tools.navi-apply.program=cp"),
            OsString::from("--config"),
            OsString::from(format!(
                "merge-tools.navi-apply.merge-args=[\"{prepared}\", \"$output\"]"
            )),
            OsString::from("resolve"),
            OsString::from("-r"),
            OsString::from(revision),
            OsString::from("--tool"),
            OsString::from("navi-apply"),
            OsString::from(fileset_exact_pattern(path)),
        ])
        .map(|_| ())
    }

    /// Rebase every visible child of `parent` onto `destination`.
    pub(crate) fn rebase_children_onto(&self, parent: &str, destination: &str) -> Result<()> {
        self.run(&[
            OsString::from("rebase"),
            OsString::from("-s"),
            OsString::from(format!("children({parent})")),
            OsString::from("-d"),
            OsString::from(destination),
        ])
        .map(|_| ())
    }

    /// Abandon commits by id in a single operation (atomic, `jj op undo`
    /// reverses the whole batch).
    pub(crate) fn abandon_commits(&self, commit_ids: &[String]) -> Result<()> {
        let revset = commit_ids.join(" | ");
        self.run(&[
            OsString::from("abandon"),
            OsString::from("-r"),
            OsString::from(revset),
        ])
        .map(|_| ())
    }

    pub(crate) fn rebase_source_onto(&self, source: &str, target: &str) -> Result<JjCommandOutput> {
        let output = self.run_capture(&[
            OsString::from("rebase"),
            OsString::from("-s"),
            OsString::from(source),
            OsString::from("-d"),
            OsString::from(target),
        ]);

        output.map_err(|error| match error {
            Error::JjCommandFailed { stderr, .. } => Error::MergeRebaseFailed { stderr },
            other => other,
        })
    }

    pub(crate) fn new_working_copy(&self, revision: &str) -> Result<JjCommandOutput> {
        self.run_capture(&[OsString::from("new"), OsString::from(revision)])
    }

    pub(crate) fn has_conflicts(&self, revision: &str) -> Result<bool> {
        let revset = format!("{revision} & conflicts()");
        Ok(!self.revisions(&revset)?.is_empty())
    }

    pub(crate) fn workspace_root(&self, workspace: &WorkspaceName) -> Result<PathBuf> {
        let args = [
            OsString::from("workspace"),
            OsString::from("root"),
            OsString::from("--name"),
            OsString::from(workspace.as_str()),
        ];
        let output = self.run_ignoring_working_copy(&args)?;
        let root = output.trim();

        if root.is_empty() {
            return Err(Error::JjCommandFailed {
                command: format_command(&args),
                stderr: String::from("jj returned an empty workspace root"),
            });
        }

        Ok(PathBuf::from(root))
    }

    /// Snapshot this client's working copy, making it current.
    pub(crate) fn snapshot(&self) -> Result<()> {
        self.run(&[OsString::from("util"), OsString::from("snapshot")])
            .map(|_| ())
    }

    /// Recover a stale working copy via `jj workspace update-stale`.
    pub(crate) fn workspace_update_stale(&self) -> Result<()> {
        self.run(&[
            OsString::from("workspace"),
            OsString::from("update-stale"),
        ])
        .map(|_| ())
    }

    /// Snapshot, recovering a stale working copy once if needed.
    pub(crate) fn snapshot_recovering_stale(&self) -> Result<bool> {
        match self.snapshot() {
            Ok(()) => Ok(false),
            Err(Error::JjCommandFailed { stderr, .. })
                if stderr.to_lowercase().contains("stale") =>
            {
                self.workspace_update_stale()?;
                self.snapshot()?;
                Ok(true)
            }
            Err(error) => Err(error),
        }
    }

    /// Check whether `ancestor` is an ancestor of (or equal to) `descendant`.
    pub(crate) fn is_ancestor(&self, ancestor: &str, descendant: &str) -> Result<bool> {
        let revset = format!("({ancestor}) & ::({descendant})");
        Ok(!self.revisions(&revset)?.is_empty())
    }

    /// Count revisions matching a revset.
    pub(crate) fn count(&self, revset: &str) -> Result<usize> {
        Ok(self.revisions(revset)?.len())
    }

    /// Check whether a revision has an empty tree diff against its parents.
    pub(crate) fn is_empty_commit(&self, revision: &str) -> Result<bool> {
        let revset = format!("({revision}) & empty()");
        Ok(!self.revisions(&revset)?.is_empty())
    }

    /// Describe a revision with a message.
    pub(crate) fn describe(&self, revision: &str, message: &str) -> Result<()> {
        self.run(&[
            OsString::from("describe"),
            OsString::from("-r"),
            OsString::from(revision),
            OsString::from("-m"),
            OsString::from(message),
        ])
        .map(|_| ())
    }

    /// List changed repo-relative paths between two revisions.
    pub(crate) fn changed_paths(&self, from: &str, to: &str) -> Result<Vec<String>> {
        let output = self.run_ignoring_working_copy(&[
            OsString::from("diff"),
            OsString::from("--summary"),
            OsString::from("--from"),
            OsString::from(from),
            OsString::from("--to"),
            OsString::from(to),
        ])?;

        Ok(parse_diff_summary_paths(&output))
    }

    /// List paths changed by a revision against its parents.
    pub(crate) fn changed_paths_in(&self, revision: &str) -> Result<Vec<String>> {
        let output = self.run_ignoring_working_copy(&[
            OsString::from("diff"),
            OsString::from("--summary"),
            OsString::from("-r"),
            OsString::from(revision),
        ])?;

        Ok(parse_diff_summary_paths(&output))
    }

    /// Render a git-format diff of a revision against its parents.
    pub(crate) fn diff_git_rev(&self, revision: &str) -> Result<String> {
        self.run_ignoring_working_copy(&[
            OsString::from("diff"),
            OsString::from("--git"),
            OsString::from("-r"),
            OsString::from(revision),
        ])
    }

    /// Render a git-format diff between two revisions.
    pub(crate) fn diff_git(&self, from: &str, to: &str) -> Result<String> {
        self.run_ignoring_working_copy(&[
            OsString::from("diff"),
            OsString::from("--git"),
            OsString::from("--from"),
            OsString::from(from),
            OsString::from("--to"),
            OsString::from(to),
        ])
    }

    /// Rebase the branch containing `branch` onto `destination`.
    pub(crate) fn rebase_branch_onto(&self, branch: &str, destination: &str) -> Result<()> {
        self.run(&[
            OsString::from("rebase"),
            OsString::from("-b"),
            OsString::from(branch),
            OsString::from("-d"),
            OsString::from(destination),
        ])
        .map(|_| ())
    }

    /// Rebase the subtree rooted at `source` onto `destination`.
    pub(crate) fn rebase_source(&self, source: &str, destination: &str) -> Result<()> {
        self.run(&[
            OsString::from("rebase"),
            OsString::from("-s"),
            OsString::from(source),
            OsString::from("-d"),
            OsString::from(destination),
        ])
        .map(|_| ())
    }

    /// Restore paths in this client's working copy from a revision.
    ///
    /// Paths are passed as exact-match `file:` fileset patterns so names
    /// containing fileset metacharacters (`&`, `~`, `*`, quotes) restore the
    /// named file instead of silently matching a different set.
    pub(crate) fn restore_paths(&self, from: &str, paths: &[String]) -> Result<()> {
        let mut args = vec![
            OsString::from("restore"),
            OsString::from("--from"),
            OsString::from(from),
        ];
        for path in paths {
            args.push(OsString::from(fileset_exact_pattern(path)));
        }
        self.run(&args).map(|_| ())
    }

    /// Configure sparse patterns for this client's workspace (additive).
    pub(crate) fn sparse_set(&self, paths: &[String]) -> Result<()> {
        let mut args = vec![OsString::from("sparse"), OsString::from("set")];
        for path in paths {
            args.push(OsString::from("--add"));
            args.push(OsString::from(path));
        }
        self.run(&args).map(|_| ())
    }

    /// Replace this workspace's sparse patterns with exactly `paths`.
    pub(crate) fn sparse_set_exact(&self, paths: &[String]) -> Result<()> {
        let mut args = vec![
            OsString::from("sparse"),
            OsString::from("set"),
            OsString::from("--clear"),
        ];
        for path in paths {
            args.push(OsString::from("--add"));
            args.push(OsString::from(path));
        }
        self.run(&args).map(|_| ())
    }

    /// Whether this workspace is sparse (patterns narrower than the root).
    pub(crate) fn sparse_is_active(&self) -> Result<bool> {
        let output = self.run(&[OsString::from("sparse"), OsString::from("list")])?;
        let patterns: Vec<&str> = output
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .collect();
        Ok(patterns != ["."])
    }

    /// Create a workspace with explicit sparse-pattern handling.
    pub(crate) fn workspace_add_sparse(
        &self,
        workspace: &WorkspaceName,
        target_root: &Path,
        revision: Option<&str>,
        sparse_patterns: &str,
    ) -> Result<()> {
        let mut args = workspace_add_args(workspace, target_root, revision);
        let destination = args.pop().unwrap_or_default();
        args.push(OsString::from("--sparse-patterns"));
        args.push(OsString::from(sparse_patterns));
        args.push(destination);
        self.run(&args).map(|_| ())
    }

    fn run(&self, args: &[OsString]) -> Result<String> {
        let output = self.run_capture(args)?;
        Ok(output.stdout)
    }

    fn run_capture(&self, args: &[OsString]) -> Result<JjCommandOutput> {
        let result = self.run_capture_once(args);

        // Umbrella: recover a stale working copy once and retry. Nothing was
        // mutated by the failed attempt, so the retry is safe. Guard against
        // recursing on `workspace update-stale` itself.
        if let Err(Error::JjCommandFailed { stderr, .. }) = &result
            && stderr_reports_stale(stderr)
            && !is_update_stale_command(args)
        {
            let update = [
                OsString::from("workspace"),
                OsString::from("update-stale"),
            ];
            if self.run_capture_once(&update).is_ok() {
                return self.run_capture_once(args);
            }
        }

        result
    }

    fn run_capture_once(&self, args: &[OsString]) -> Result<JjCommandOutput> {
        let output = jj_command()
            .args(args)
            .current_dir(self.workspace_root)
            .output()?;

        if output.status.success() {
            Ok(JjCommandOutput {
                stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
                stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            })
        } else {
            Err(Error::JjCommandFailed {
                command: format_command(args),
                stderr: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
            })
        }
    }

    fn run_ignoring_working_copy(&self, args: &[OsString]) -> Result<String> {
        let mut full_args = Vec::with_capacity(args.len() + 1);
        full_args.push(OsString::from("--ignore-working-copy"));
        full_args.extend(args.iter().cloned());
        self.run(&full_args)
    }
}

fn workspace_add_args(
    workspace: &WorkspaceName,
    target_root: &Path,
    revision: Option<&str>,
) -> Vec<OsString> {
    let mut args = vec![
        OsString::from("workspace"),
        OsString::from("add"),
        OsString::from("--name"),
        OsString::from(workspace.as_str()),
    ];

    if let Some(revision) = revision {
        args.push(OsString::from("-r"));
        args.push(OsString::from(revision));
    }

    args.push(target_root.as_os_str().to_owned());
    args
}

fn format_command(args: &[OsString]) -> String {
    let rendered = args
        .iter()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join(" ");
    format!("jj {rendered}")
}

enum TimedCommandResult {
    Success(String),
    Failure(String),
    TimedOut,
    Io(std::io::Error),
}

fn run_with_timeout(path: &Path, args: &[OsString], timeout: Duration) -> TimedCommandResult {
    let (stdout_file, stdout_path) = match temp_output_file("stdout") {
        Ok(output) => output,
        Err(error) => return TimedCommandResult::Io(error),
    };
    let (stderr_file, stderr_path) = match temp_output_file("stderr") {
        Ok(output) => output,
        Err(error) => {
            let _ = fs::remove_file(stdout_path);
            return TimedCommandResult::Io(error);
        }
    };

    let mut child = match jj_command()
        .args(args)
        .current_dir(path)
        .stdout(stdout_file)
        .stderr(stderr_file)
        .spawn()
    {
        Ok(child) => child,
        Err(error) => {
            let _ = fs::remove_file(stdout_path);
            let _ = fs::remove_file(stderr_path);
            return TimedCommandResult::Io(error);
        }
    };

    let deadline = Instant::now() + timeout;

    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let stdout = read_temp_output(&stdout_path);
                let stderr = read_temp_output(&stderr_path);
                let _ = fs::remove_file(stdout_path);
                let _ = fs::remove_file(stderr_path);

                return match (status.success(), stdout, stderr) {
                    (true, Ok(stdout), _) => TimedCommandResult::Success(stdout),
                    (false, _, Ok(stderr)) => TimedCommandResult::Failure(stderr.trim().to_owned()),
                    (_, Err(error), _) | (_, _, Err(error)) => TimedCommandResult::Io(error),
                };
            }
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    let _ = fs::remove_file(stdout_path);
                    let _ = fs::remove_file(stderr_path);
                    return TimedCommandResult::TimedOut;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(error) => {
                let _ = fs::remove_file(stdout_path);
                let _ = fs::remove_file(stderr_path);
                return TimedCommandResult::Io(error);
            }
        }
    }
}

fn jj_command() -> Command {
    let binary = std::env::var_os("NAVI_JJ_BIN")
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| OsString::from("jj"));
    let mut command = Command::new(binary);
    command
        // Pin jj's output format so user config (ui.color, pagers) cannot
        // rewrite what navi parses.
        .args(["--color=never", "--no-pager"])
        .env_remove("COMPLETE")
        .env_remove("_CLAP_COMPLETE_INDEX")
        .env_remove("_CLAP_IFS");
    command
}

fn stderr_reports_stale(stderr: &str) -> bool {
    let stderr = stderr.to_lowercase();
    stderr.contains("stale") && stderr.contains("working copy")
}

fn is_update_stale_command(args: &[OsString]) -> bool {
    args.iter().any(|arg| arg == "update-stale")
}

fn temp_output_file(kind: &str) -> std::io::Result<(File, PathBuf)> {
    let id = TEMP_OUTPUT_COUNTER.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!("jj-navi-{}-{id}-{kind}.tmp", std::process::id()));
    let file = File::options().write(true).create_new(true).open(&path)?;
    Ok((file, path))
}

fn read_temp_output(path: &Path) -> std::io::Result<String> {
    let mut output = String::new();
    File::open(path)?.read_to_string(&mut output)?;
    Ok(output)
}

fn meaningful_stderr(stderr: &str, fallback: &str) -> String {
    if stderr.trim().is_empty() {
        fallback.to_owned()
    } else {
        stderr.trim().to_owned()
    }
}

fn parse_diff_summary_paths(output: &str) -> Vec<String> {
    let mut paths = Vec::new();
    for line in output.lines() {
        let Some((_, path)) = line.split_once(' ') else {
            continue;
        };
        // Rename summaries render as "R {old => new}"-style path groups;
        // expand them so both sides count as touched paths.
        if let Some((prefix, rest)) = path.split_once('{')
            && let Some((group, suffix)) = rest.split_once('}')
            && let Some((old, new)) = group.split_once(" => ")
        {
            paths.push(format!("{prefix}{old}{suffix}"));
            paths.push(format!("{prefix}{new}{suffix}"));
            continue;
        }
        paths.push(path.to_owned());
    }
    paths.sort();
    paths.dedup();
    paths
}

fn parse_diff_stat(output: &str) -> WorkspaceDiffSnapshot {
    let Some(line) = output.lines().rev().find(|line| line.contains(" changed")) else {
        return WorkspaceDiffSnapshot::unknown();
    };

    WorkspaceDiffSnapshot {
        status: WorkspaceDiffStatus::Available,
        files_changed: number_before(line, " file"),
        insertions: number_before(line, " insertion"),
        deletions: number_before(line, " deletion"),
    }
}

fn number_before(line: &str, marker: &str) -> Option<u32> {
    let before_marker = line.split(marker).next()?;
    before_marker
        .split(|ch: char| !ch.is_ascii_digit())
        .rfind(|part| !part.is_empty())?
        .parse()
        .ok()
}

fn parse_jj_version(output: &str) -> Option<JjVersion> {
    let token = output
        .split_whitespace()
        .find(|part| part.chars().next().is_some_and(|ch| ch.is_ascii_digit()))?;
    let mut parts = token.split('.');

    Some(JjVersion {
        major: parse_version_component(parts.next()?)?,
        minor: parse_version_component(parts.next()?)?,
        patch: parse_version_component(parts.next()?)?,
    })
}

fn parse_version_component(component: &str) -> Option<u64> {
    let digits = component
        .chars()
        .take_while(char::is_ascii_digit)
        .collect::<String>();

    if digits.is_empty() {
        return None;
    }

    digits.parse().ok()
}

fn parse_workspace_line(line: &str) -> Result<JjWorkspaceListEntry> {
    let mut parts = line.splitn(5, '\0');
    let (Some(name), Some(is_current), Some(commit_id), Some(change_id), Some(message)) = (
        parts.next(),
        parts.next(),
        parts.next(),
        parts.next(),
        parts.next(),
    ) else {
        return Err(Error::InvalidJjWorkspaceListEntry(line.to_owned()));
    };

    let is_current = match is_current {
        "0" => false,
        "1" => true,
        _ => return Err(Error::InvalidJjWorkspaceListEntry(line.to_owned())),
    };

    Ok(JjWorkspaceListEntry {
        name: WorkspaceName::new(unquote_template_string(name))?,
        is_current,
        commit_id: commit_id.to_owned(),
        change_id: change_id.to_owned(),
        message: message.to_owned(),
    })
}

fn parse_revision_line(line: &str) -> Result<JjRevisionSummary> {
    let mut parts = line.splitn(3, '\0');
    let (Some(commit_id), Some(change_id), Some(message)) =
        (parts.next(), parts.next(), parts.next())
    else {
        return Err(Error::InvalidJjWorkspaceListEntry(line.to_owned()));
    };

    Ok(JjRevisionSummary {
        commit_id: commit_id.to_owned(),
        change_id: change_id.to_owned(),
        message: message.to_owned(),
    })
}

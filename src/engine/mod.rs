//! Embedded `jj-lib` engine for deep, read-only repository analysis.
//!
//! The engine opens the repo the same way `jj` does and answers questions
//! the CLI surface is thin on: operation-level provenance (which op minted
//! which commit, when, by whom) and consistent whole-repo queries evaluated
//! against a single immutable snapshot.
//!
//! Boundaries, by design:
//! - **Read-only.** The engine never mutates repo state and never touches
//!   working-copy state files; mutations stay on the `jj` CLI so navi and
//!   the user's jj binary can never disagree about working-copy formats.
//! - **Config parity.** The engine is fed the output of
//!   `jj config list --include-defaults` from the user's own `jj`, so it
//!   resolves settings exactly as their binary does.

use std::collections::BTreeMap;
use std::collections::HashMap;
use std::collections::HashSet;
use std::path::Path;
use std::sync::Arc;

use futures::TryStreamExt as _;
use jj_lib::backend::CommitId;
use jj_lib::config::{ConfigLayer, ConfigSource, StackedConfig};
use jj_lib::evolution::walk_predecessors;
use jj_lib::object_id::ObjectId as _;
use jj_lib::op_walk;
use jj_lib::repo::{ReadonlyRepo, Repo as _, StoreFactories};
use jj_lib::revset::RevsetExpression;
use jj_lib::settings::UserSettings;
use jj_lib::workspace::{Workspace, default_working_copy_factories};

use crate::error::{Error, Result};

/// Fallback identity so settings resolution cannot fail on a machine with no
/// jj user config; real values from the resolved config layer take priority.
const FALLBACK_CONFIG: &str = concat!(
    "user.name = \"navi\"\n",
    "user.email = \"navi@localhost\"\n",
    "operation.hostname = \"navi\"\n",
    "operation.username = \"navi\"\n",
);

pub(crate) struct Engine {
    repo: Arc<ReadonlyRepo>,
    operation_username: String,
}

/// One divergent change: a change id with more than one visible commit.
#[derive(Clone, Debug)]
pub struct DivergentChange {
    /// Change id in jj's reverse-hex display form.
    pub change_id: String,
    /// The visible sibling commits of this change.
    pub siblings: Vec<DivergentSibling>,
}

/// One visible sibling commit of a divergent change.
#[derive(Clone, Debug)]
pub struct DivergentSibling {
    /// Commit id (short hex).
    pub commit_id: String,
    /// First line of the description.
    pub description: String,
    /// Author email.
    pub author_email: String,
    /// Committer timestamp, milliseconds since epoch.
    pub committer_millis: i64,
    /// Operation that created or last rewrote this commit, when known.
    pub op: Option<OpProvenance>,
    /// Workspaces whose working copy is currently this commit.
    pub wc_of: Vec<String>,
    /// Whether the sibling has visible child commits.
    pub has_children: bool,
    /// Whether some workspace's working copy is this commit or one of its
    /// descendants (touching it would rewrite a live chain from outside).
    pub blocks_wc: bool,
}

/// Provenance of a commit: the operation that minted it.
#[derive(Clone, Debug)]
pub struct OpProvenance {
    /// Operation id (short hex).
    pub op_id: String,
    /// Operation end time, milliseconds since epoch.
    pub end_millis: i64,
    /// Operation username.
    pub username: String,
    /// Operation hostname.
    pub hostname: String,
    /// Operation description (for example `snapshot working copy`).
    pub description: String,
}

/// Operation-log activity summary.
#[derive(Clone, Copy, Debug)]
pub struct OpChurn {
    /// Operations whose end time falls within the window.
    pub recent: usize,
    /// Whether the walk stopped at the safety cap rather than the window.
    pub capped: bool,
}

fn engine_error(context: &str, error: impl std::fmt::Display) -> Error {
    Error::Engine {
        message: format!("{context}: {error}"),
    }
}

impl Engine {
    /// Open the repo at its current operation head.
    ///
    /// `resolved_jj_config` is the TOML output of
    /// `jj config list --include-defaults` from the user's jj binary.
    ///
    /// # Errors
    ///
    /// Returns an error if the workspace, settings, or repo cannot be
    /// loaded.
    pub fn open(workspace_root: &Path, resolved_jj_config: &str) -> Result<Self> {
        let mut config = StackedConfig::with_defaults();
        let fallback = ConfigLayer::parse(ConfigSource::EnvBase, FALLBACK_CONFIG)
            .map_err(|error| engine_error("internal fallback config", error))?;
        config.add_layer(fallback);
        let resolved = ConfigLayer::parse(ConfigSource::User, resolved_jj_config)
            .map_err(|error| engine_error("resolved jj config", error))?;
        config.add_layer(resolved);

        let settings = UserSettings::from_config(config)
            .map_err(|error| engine_error("jj settings", error))?;
        let workspace = Workspace::load(
            &settings,
            workspace_root,
            &StoreFactories::default(),
            &default_working_copy_factories(),
        )
        .map_err(|error| engine_error("load workspace", error))?;
        let repo = pollster::block_on(workspace.repo_loader().load_at_head())
            .map_err(|error| engine_error("load repo at head", error))?;
        let operation_username = settings.operation_username().to_owned();

        Ok(Self {
            repo,
            operation_username,
        })
    }

    /// The op-log username this user's operations are recorded under.
    pub fn operation_username(&self) -> &str {
        &self.operation_username
    }

    /// Enumerate every divergent change with sibling details and operation
    /// provenance, all read from one immutable repo snapshot.
    ///
    /// # Errors
    ///
    /// Returns an error if revset evaluation or store reads fail.
    pub fn divergent_changes(&self) -> Result<Vec<DivergentChange>> {
        let repo = self.repo.as_ref();
        let expression = RevsetExpression::all().intersection(&RevsetExpression::divergent());
        let revset = expression
            .evaluate(repo)
            .map_err(|error| engine_error("evaluate divergent()", error))?;
        let pairs: Vec<(CommitId, jj_lib::backend::ChangeId)> =
            pollster::block_on(revset.commit_change_ids().try_collect())
                .map_err(|error| engine_error("collect divergent commits", error))?;
        drop(revset);

        let mut groups: BTreeMap<Vec<u8>, (String, Vec<CommitId>)> = BTreeMap::new();
        for (commit_id, change_id) in pairs {
            let key = change_id.as_bytes().to_vec();
            let entry = groups
                .entry(key)
                .or_insert_with(|| (reverse_hex(&change_id.hex()), Vec::new()));
            entry.1.push(commit_id);
        }

        let all_ids: Vec<CommitId> = groups
            .values()
            .flat_map(|(_, ids)| ids.iter().cloned())
            .collect();
        let provenance = self.provenance_for(&all_ids)?;
        let children = self.commits_with_visible_children(&all_ids)?;
        let blocked = self.commits_with_wc_descendant(&all_ids)?;

        let mut wc_of: HashMap<CommitId, Vec<String>> = HashMap::new();
        for (name, commit_id) in self.repo.view().wc_commit_ids() {
            wc_of
                .entry(commit_id.clone())
                .or_default()
                .push(name.as_str().to_owned());
        }

        let mut changes = Vec::new();
        for (change_id, ids) in groups.into_values() {
            let mut siblings = Vec::new();
            for id in ids {
                let commit = self
                    .repo
                    .store()
                    .get_commit(&id)
                    .map_err(|error| engine_error("load commit", error))?;
                siblings.push(DivergentSibling {
                    commit_id: short_hex(&id.hex()),
                    description: commit
                        .description()
                        .lines()
                        .next()
                        .unwrap_or_default()
                        .to_owned(),
                    author_email: commit.author().email.clone(),
                    committer_millis: commit.committer().timestamp.timestamp.0,
                    op: provenance.get(&id).cloned(),
                    wc_of: wc_of.get(&id).cloned().unwrap_or_default(),
                    has_children: children.contains(&id),
                    blocks_wc: blocked.contains(&id),
                });
            }
            // Newest first so callers can default to first-is-winner.
            siblings.sort_by_key(|sibling| {
                std::cmp::Reverse(
                    sibling
                        .op
                        .as_ref()
                        .map_or(sibling.committer_millis, |op| op.end_millis),
                )
            });
            changes.push(DivergentChange {
                change_id,
                siblings,
            });
        }
        Ok(changes)
    }

    /// Count operations newer than `window_secs`, walking at most `cap` ops.
    ///
    /// # Errors
    ///
    /// Returns an error if the operation log cannot be walked.
    pub fn op_churn(&self, window_secs: u64, cap: usize) -> Result<OpChurn> {
        let now_millis = i64::try_from(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|duration| duration.as_millis())
                .unwrap_or_default(),
        )
        .unwrap_or(i64::MAX);
        let cutoff = now_millis.saturating_sub_unsigned(window_secs.saturating_mul(1000));

        let mut recent = 0;
        let mut walked = 0;
        let mut capped = false;
        let stream = op_walk::walk_ancestors(std::slice::from_ref(self.repo.operation()));
        pollster::block_on(async {
            futures::pin_mut!(stream);
            while let Some(op) = stream
                .try_next()
                .await
                .map_err(|error| engine_error("walk op log", error))?
            {
                walked += 1;
                let end = op.metadata().time.end.timestamp.0;
                if end >= cutoff {
                    recent += 1;
                } else {
                    break;
                }
                if walked >= cap {
                    capped = true;
                    break;
                }
            }
            Ok::<_, Error>(())
        })?;

        Ok(OpChurn { recent, capped })
    }

    /// Resolve the creating/last-rewriting operation for each commit id.
    fn provenance_for(&self, ids: &[CommitId]) -> Result<HashMap<CommitId, OpProvenance>> {
        let mut remaining: HashSet<CommitId> = ids.iter().cloned().collect();
        let mut provenance = HashMap::new();
        if remaining.is_empty() {
            return Ok(provenance);
        }

        let stream = walk_predecessors(self.repo.as_ref(), ids);
        pollster::block_on(async {
            futures::pin_mut!(stream);
            while let Some(entry) = stream
                .try_next()
                .await
                .map_err(|error| engine_error("walk predecessors", error))?
            {
                let id = entry.commit.id().clone();
                if remaining.remove(&id)
                    && let Some(op) = &entry.operation
                {
                    let metadata = op.metadata();
                    provenance.insert(
                        id,
                        OpProvenance {
                            op_id: short_hex(&op.id().hex()),
                            end_millis: metadata.time.end.timestamp.0,
                            username: metadata.username.clone(),
                            hostname: metadata.hostname.clone(),
                            description: metadata.description.clone(),
                        },
                    );
                }
                if remaining.is_empty() {
                    break;
                }
            }
            Ok::<_, Error>(())
        })?;

        Ok(provenance)
    }

    /// Which of `ids` have visible children (descendant work stacked on
    /// them).
    fn commits_with_visible_children(&self, ids: &[CommitId]) -> Result<HashSet<CommitId>> {
        let repo = self.repo.as_ref();
        let parents: HashSet<CommitId> = ids.iter().cloned().collect();
        let expression = RevsetExpression::commits(ids.to_vec()).children();
        let revset = expression
            .evaluate(repo)
            .map_err(|error| engine_error("evaluate children()", error))?;
        let children: Vec<CommitId> = pollster::block_on(revset.stream().try_collect())
            .map_err(|error| engine_error("collect children", error))?;
        drop(revset);

        let mut with_children = HashSet::new();
        for child in children {
            let commit = repo
                .store()
                .get_commit(&child)
                .map_err(|error| engine_error("load child commit", error))?;
            for parent in commit.parent_ids() {
                if parents.contains(parent) {
                    with_children.insert(parent.clone());
                }
            }
        }
        Ok(with_children)
    }
}

/// A structurally union-merged file: the resolved bytes plus how many
/// sides the conflict had (jj's `resolve --tool` can only apply 2-sided
/// resolutions; more sides need the squash path).
pub struct UnionFileMerge {
    /// The union-resolved file content.
    pub content: Vec<u8>,
    /// Number of conflict sides after simplification.
    pub sides: usize,
}

/// Append the union of every side's lines: side order first-seen, each
/// non-whitespace line emitted once even when several sides carry it
/// (rebase echoes duplicate entries across sides). Whitespace-only lines
/// always pass through so formatting survives.
fn union_hunk_lines<T: AsRef<[u8]>>(merged: &mut Vec<u8>, sides: impl Iterator<Item = T>) {
    let mut seen: HashSet<Vec<u8>> = HashSet::new();
    let mut prior_sides: HashSet<Vec<u8>> = HashSet::new();
    for side in sides {
        for line in side.as_ref().split_inclusive(|byte| *byte == b'\n') {
            let whitespace_only = line.iter().all(u8::is_ascii_whitespace);
            if whitespace_only || !prior_sides.contains(line) {
                merged.extend_from_slice(line);
            }
            seen.insert(line.to_vec());
        }
        prior_sides.extend(seen.drain());
    }
}

/// One conflict root: a commit where conflicts begin (its parents are
/// conflict-free), with the set of conflicted paths.
#[derive(Clone, Debug, serde::Serialize)]
pub struct ConflictRoot {
    /// Commit id (short hex), usable in revsets.
    pub commit_id: String,
    /// Number of visible descendants (including self) that carry conflicts.
    pub conflicted_descendants: usize,
    /// Conflicted repo-relative paths in this commit's tree.
    pub paths: Vec<String>,
}

impl Engine {
    /// Enumerate conflict roots with blast radius and conflicted paths.
    ///
    /// # Errors
    ///
    /// Returns an error if revset evaluation or tree reads fail.
    pub fn conflict_roots(&self) -> Result<Vec<ConflictRoot>> {
        use jj_lib::revset::RevsetFilterPredicate;

        let repo = self.repo.as_ref();
        let conflicted = RevsetExpression::all()
            .filtered(RevsetFilterPredicate::HasConflict);
        let roots_expr = conflicted.roots();
        let revset = roots_expr
            .evaluate(repo)
            .map_err(|error| engine_error("evaluate conflict roots", error))?;
        let root_ids: Vec<CommitId> = pollster::block_on(revset.stream().try_collect())
            .map_err(|error| engine_error("collect conflict roots", error))?;
        drop(revset);

        let mut roots = Vec::new();
        for id in root_ids {
            let commit = self
                .repo
                .store()
                .get_commit(&id)
                .map_err(|error| engine_error("load conflict root", error))?;
            let tree = commit.tree();
            let paths: Vec<String> = tree
                .conflicts()
                .map(|(path, _value)| path.as_internal_file_string().to_owned())
                .collect();

            let blast = RevsetExpression::commits(vec![id.clone()])
                .descendants()
                .filtered(RevsetFilterPredicate::HasConflict);
            let revset = blast
                .evaluate(repo)
                .map_err(|error| engine_error("evaluate blast radius", error))?;
            let conflicted_descendants: Vec<CommitId> =
                pollster::block_on(revset.stream().try_collect())
                    .map_err(|error| engine_error("collect blast radius", error))?;
            drop(revset);

            roots.push(ConflictRoot {
                commit_id: short_hex(&id.hex()),
                conflicted_descendants: conflicted_descendants.len(),
                paths,
            });
        }
        roots.sort_by_key(|root| std::cmp::Reverse(root.conflicted_descendants));
        Ok(roots)
    }

    /// Resolve a (possibly short) hex commit id against the repo snapshot.
    fn resolve_commit_prefix(&self, commit_id: &str) -> Result<CommitId> {
        let repo = self.repo.as_ref();
        // Heads first (cheap), then a revset sweep over all visible commits.
        if let Some(id) = self
            .repo
            .view()
            .heads()
            .iter()
            .find(|id| id.hex().starts_with(commit_id))
            .cloned()
        {
            return Ok(id);
        }
        let expr = RevsetExpression::all();
        let revset = expr
            .evaluate(repo)
            .map_err(|error| engine_error("evaluate all()", error))?;
        let ids: Vec<CommitId> = pollster::block_on(revset.stream().try_collect())
            .map_err(|error| engine_error("resolve commit id", error))?;
        drop(revset);
        ids.into_iter()
            .find(|id| id.hex().starts_with(commit_id))
            .ok_or_else(|| Error::Engine {
                message: format!("commit {commit_id} not found"),
            })
    }

    /// Paths whose tree value differs between two commits (short hex ids).
    ///
    /// # Errors
    ///
    /// Returns an error if either commit cannot be resolved or the diff
    /// stream fails.
    pub fn changed_paths_between(&self, from: &str, to: &str) -> Result<Vec<String>> {
        use futures::StreamExt as _;
        use jj_lib::matchers::EverythingMatcher;

        let from = self
            .repo
            .store()
            .get_commit(&self.resolve_commit_prefix(from)?)
            .map_err(|error| engine_error("load diff base", error))?;
        let to = self
            .repo
            .store()
            .get_commit(&self.resolve_commit_prefix(to)?)
            .map_err(|error| engine_error("load diff target", error))?;

        let from_tree = from.tree();
        let to_tree = to.tree();
        let stream = from_tree.diff_stream(&to_tree, &EverythingMatcher);
        let entries: Vec<_> = pollster::block_on(stream.collect::<Vec<_>>());
        Ok(entries
            .into_iter()
            .map(|entry| entry.path.as_internal_file_string().to_owned())
            .collect())
    }

    /// Union-merge a conflicted file at `commit_id` (short hex): jj's hunk
    /// merge with conflicted hunks resolved by keeping every side's lines —
    /// a line survives (once) if any side has it. Rebase-echo conflicts
    /// carry heavily overlapping sides, so a plain concatenation would
    /// duplicate entries; the union dedupes non-whitespace lines across
    /// sides while preserving first-seen order.
    ///
    /// Returns `None` when the path's conflict is not a clean file conflict
    /// (deleted side, binary, non-file terms) and must be handled manually.
    ///
    /// # Errors
    ///
    /// Returns an error on store failures or if the commit/path is unknown.
    pub fn union_merge_file(&self, commit_id: &str, path: &str) -> Result<Option<UnionFileMerge>> {
        use jj_lib::repo_path::RepoPath;

        let id = self.resolve_commit_prefix(commit_id)?;

        let commit = self
            .repo
            .store()
            .get_commit(&id)
            .map_err(|error| engine_error("load commit", error))?;
        let tree = commit.tree();
        let repo_path = RepoPath::from_internal_string(path)
            .map_err(|error| engine_error("parse path", error))?;
        let value = pollster::block_on(tree.path_value(repo_path))
            .map_err(|error| engine_error("read path value", error))?;
        if value.is_resolved() {
            return Ok(None);
        }
        let Some(file_merge) = value.to_file_merge() else {
            return Ok(None); // non-file terms (directories, symlinks, absent)
        };
        // Cancel matching add/remove pairs first: rebase echoes often
        // simplify to fewer sides (or resolve outright).
        let file_merge = file_merge.simplify();
        if file_merge.iter().any(Option::is_none) {
            return Ok(None); // a side deleted the file; not union material
        }
        let sides = file_merge.adds().count();

        let contents = pollster::block_on(jj_lib::conflicts::extract_as_single_hunk(
            &file_merge,
            self.repo.store(),
            repo_path,
        ))
        .map_err(|error| engine_error("read conflict contents", error))?;
        if contents
            .iter()
            .any(|content| content.contains(&0u8))
        {
            return Ok(None); // binary content; refuse
        }

        let options = jj_lib::tree_merge::MergeOptions::from_settings(self.repo.settings())
            .map_err(|error| engine_error("merge options", error))?;
        let mut merged = Vec::new();
        match jj_lib::files::merge_hunks(&contents, &options) {
            jj_lib::files::MergeResult::Resolved(content) => merged.extend_from_slice(&content),
            jj_lib::files::MergeResult::Conflict(hunks) => {
                for hunk in hunks {
                    if let Some(resolved) = hunk.as_resolved() {
                        merged.extend_from_slice(resolved);
                    } else {
                        union_hunk_lines(&mut merged, hunk.adds());
                    }
                }
            }
        }
        Ok(Some(UnionFileMerge {
            content: merged,
            sides,
        }))
    }
    fn commits_with_wc_descendant(&self, ids: &[CommitId]) -> Result<HashSet<CommitId>> {
        let repo = self.repo.as_ref();
        let wc_ids: Vec<CommitId> = repo.view().wc_commit_ids().values().cloned().collect();
        if wc_ids.is_empty() || ids.is_empty() {
            return Ok(HashSet::new());
        }

        // Working copies sitting on any of `ids`.
        let affected = RevsetExpression::commits(ids.to_vec())
            .descendants()
            .intersection(&RevsetExpression::commits(wc_ids));
        let revset = affected
            .evaluate(repo)
            .map_err(|error| engine_error("evaluate wc descendants", error))?;
        let affected_wcs: Vec<CommitId> = pollster::block_on(revset.stream().try_collect())
            .map_err(|error| engine_error("collect wc descendants", error))?;
        drop(revset);

        // Attribute each affected working copy back to the ids beneath it.
        let mut blocked = HashSet::new();
        for wc in affected_wcs {
            let ancestors = RevsetExpression::commits(vec![wc])
                .ancestors()
                .intersection(&RevsetExpression::commits(ids.to_vec()));
            let revset = ancestors
                .evaluate(repo)
                .map_err(|error| engine_error("evaluate wc ancestry", error))?;
            let hits: Vec<CommitId> = pollster::block_on(revset.stream().try_collect())
                .map_err(|error| engine_error("collect wc ancestry", error))?;
            blocked.extend(hits);
        }
        Ok(blocked)
    }
}

fn short_hex(hex: &str) -> String {
    hex.chars().take(12).collect()
}

/// jj displays change ids in the reverse-hex alphabet (`z`..`k`).
fn reverse_hex(hex: &str) -> String {
    hex.chars()
        .take(12)
        .map(|ch| match ch {
            '0'..='9' => char::from(b'z' - (ch as u8 - b'0')),
            'a'..='f' => char::from(b'p' - (ch as u8 - b'a')),
            other => other,
        })
        .collect()
}

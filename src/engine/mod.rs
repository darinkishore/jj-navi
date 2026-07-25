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

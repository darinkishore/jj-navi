//! Lane workflow operations.
//!
//! A lane is a jj workspace plus a declared write-set, registered in repo
//! storage. The workflow keeps every lane rebased onto the trunk head
//! (small, early, attributed conflicts) and lands work by fast-forwarding
//! trunk onto an already-synced lane chain, so landing itself can never
//! conflict. `jj` stays the source of truth for all live state; the
//! registry stores only declarations and lifecycle facts.

use std::fs;
use std::path::PathBuf;
use std::process::Command;

use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::error::{Error, Result};
use crate::types::{
    LaneAbandonOutcome, LaneFanoutEntry, LaneGcPlan, LaneLandOutcome, LaneLifecycle,
    LaneListEntry, LaneOpenOutcome, LanePath, LaneRev, LaneSyncOutcome, WorkspaceName,
    WorkspacePathState,
};

use super::config::{ensure_repo_config, navi_dir_path};
use super::jj::JjClient;
use super::lane_store::{LaneRecord, LaneStore};
use super::metadata::WorkspaceMetadataStore;
use super::workspace::NaviWorkspace;

const ARCHIVE_DIR: &str = "archive";

struct TrunkContext {
    name: WorkspaceName,
    head_commit: String,
}

impl NaviWorkspace {
    /// Open a lane: register its write-set and create its workspace on the
    /// current trunk head.
    ///
    /// # Errors
    ///
    /// Returns an error on name/path validation failures, write-set overlap
    /// with another open lane, or any failing `jj` operation.
    pub fn lane_open(
        &self,
        name: &WorkspaceName,
        paths: Vec<LanePath>,
        allow_overlap: bool,
        sparse: Option<bool>,
    ) -> Result<LaneOpenOutcome> {
        let config = self.repo_config();
        if *name == config.lane.trunk {
            return Err(Error::LaneNameReserved(name.as_str().to_owned()));
        }
        if paths.is_empty() {
            return Err(Error::InvalidLanePath(String::from(
                "(lane requires at least one --path)",
            )));
        }

        let mut store = LaneStore::load(self.repo_storage_path())?;
        if self.workspace_exists(name)? {
            return Err(Error::LaneExists(name.as_str().to_owned()));
        }
        if !allow_overlap {
            check_overlap_excluding(&store, name, &paths)?;
        }

        let trunk = self.resolve_trunk()?;
        let base = self.rev(&trunk.head_commit)?;
        let sparse = sparse.unwrap_or(config.lane.sparse);
        let target_root = self.planned_workspace_root(name);
        ensure_repo_config(self.repo_storage_path(), config)?;

        let jj = JjClient::new(self.workspace_root());
        let patterns = if sparse { "empty" } else { "full" };
        jj.workspace_add_sparse(name, &target_root, Some(&trunk.head_commit), patterns)?;

        if sparse {
            let mut materialize: Vec<String> =
                paths.iter().map(|path| path.as_str().to_owned()).collect();
            materialize.push(String::from(".gitignore"));
            materialize.extend(
                config
                    .lane
                    .context_paths
                    .iter()
                    .map(|path| path.as_str().to_owned()),
            );
            materialize.sort();
            materialize.dedup();
            let lane_jj = JjClient::new(&target_root);
            lane_jj.sparse_set(&materialize)?;
        }

        let mut metadata = WorkspaceMetadataStore::load(self.repo_storage_path())?;
        metadata.record_workspace(
            name,
            &target_root,
            &config.workspace_template,
            Some(&trunk.head_commit),
        );
        metadata.save()?;

        store.insert(LaneRecord {
            name: name.clone(),
            paths: paths.clone(),
            created_at: OffsetDateTime::now_utc(),
            lifecycle: LaneLifecycle::Open,
            closed_at: None,
            last_land: None,
        })?;
        store.save()?;

        Ok(LaneOpenOutcome {
            name: name.clone(),
            path: target_root,
            base,
            paths,
            sparse,
        })
    }

    /// Extend an open lane's write-set with additional paths.
    ///
    /// # Errors
    ///
    /// Returns an error if the lane is not open or a new path overlaps
    /// another open lane's write-set.
    pub fn lane_claim(
        &self,
        name: &WorkspaceName,
        paths: Vec<LanePath>,
        allow_overlap: bool,
    ) -> Result<Vec<LanePath>> {
        let mut store = LaneStore::load(self.repo_storage_path())?;
        let record = require_open_lane(&store, name)?;

        if !allow_overlap {
            check_overlap_excluding(&store, name, &paths)?;
        }

        let mut merged = record.paths.clone();
        merged.extend(paths);
        merged.sort();
        merged.dedup();
        store.replace_paths(name, merged.clone())?;
        store.save()?;

        // Materialize newly claimed paths into a sparse lane workspace; a
        // full workspace already has them and extra adds are harmless.
        let target_root = self.planned_workspace_root(name);
        if target_root.is_dir() {
            let lane_jj = JjClient::new(&target_root);
            let add: Vec<String> = merged.iter().map(|path| path.as_str().to_owned()).collect();
            lane_jj.sparse_set(&add)?;
        }

        Ok(merged)
    }

    /// Report every registered lane with live sync/scope state from `jj`.
    ///
    /// # Errors
    ///
    /// Returns an error if the registry cannot be read or trunk cannot be
    /// resolved.
    pub fn lane_list(&self) -> Result<Vec<LaneListEntry>> {
        let store = LaneStore::load(self.repo_storage_path())?;
        let trunk = self.resolve_trunk()?;
        let jj = JjClient::new(self.workspace_root());

        let mut entries = Vec::new();
        for record in store.all_lanes() {
            let mut entry = LaneListEntry {
                name: record.name.clone(),
                lifecycle: record.lifecycle,
                paths: record.paths.clone(),
                workspace_exists: false,
                synced: false,
                ahead: 0,
                behind: 0,
                conflicts: 0,
                unscoped: Vec::new(),
                last_land: record
                    .last_land
                    .as_ref()
                    .map(|land| land.head_commit.clone()),
            };

            if record.lifecycle == LaneLifecycle::Open {
                entry.workspace_exists = self.workspace_exists(&record.name)?;
                if entry.workspace_exists {
                    // Snapshot the lane (bounded, best-effort) so weather
                    // reflects on-disk work, not the last time jj ran there.
                    if let Ok(root) = self.existing_lane_root(&record.name) {
                        let _ = super::jj::snapshot_working_copy_at(&root);
                    }
                    let lane_sym = lane_symbol(&record.name);
                    entry.synced = jj.is_ancestor(&trunk.head_commit, &lane_sym)?;
                    entry.ahead =
                        jj.count(&format!("({}..{lane_sym}) ~ empty()", trunk.head_commit))?;
                    entry.behind =
                        jj.count(&format!("::{} ~ ::{lane_sym}", trunk.head_commit))?;
                    entry.conflicts = jj.count(&format!(
                        "conflicts() & (::{lane_sym} ~ ::{})",
                        trunk.head_commit
                    ))?;
                    let base = scope_base(&jj, &trunk.head_commit, &lane_sym)?;
                    entry.unscoped =
                        unscoped_paths(&jj.changed_paths(&base, &lane_sym)?, &record.paths);
                }
            }

            entries.push(entry);
        }

        Ok(entries)
    }

    /// Sync lanes onto the current trunk head.
    ///
    /// # Errors
    ///
    /// Returns an error if a named lane is unknown or trunk cannot be
    /// resolved. Individual lane rebase conflicts are reported, not errors.
    pub fn lane_sync(
        &self,
        name: Option<&WorkspaceName>,
        drop_unscoped: bool,
    ) -> Result<Vec<LaneSyncOutcome>> {
        let store = LaneStore::load(self.repo_storage_path())?;
        let targets: Vec<LaneRecord> = match name {
            Some(name) => vec![require_open_lane(&store, name)?.clone()],
            None => store.open_lanes().into_iter().cloned().collect(),
        };
        let trunk = self.resolve_trunk()?;
        let jj = JjClient::new(self.workspace_root());

        let mut outcomes = Vec::new();
        for record in targets {
            outcomes.push(self.sync_one(&jj, &trunk, &record, drop_unscoped)?);
        }
        Ok(outcomes)
    }

    /// Land a lane: fast-forward trunk onto the lane's synced chain, then
    /// rebase every other open lane onto the new head.
    ///
    /// # Errors
    ///
    /// Returns an error if the lane is unsynced, conflicted, out of scope,
    /// dirty against trunk's working copy inside the write-set, or if the
    /// gate command fails.
    pub fn lane_land(
        &self,
        name: &WorkspaceName,
        message: Option<&str>,
        no_gate: bool,
        close: bool,
    ) -> Result<LaneLandOutcome> {
        let mut store = LaneStore::load(self.repo_storage_path())?;
        let record = require_open_lane(&store, name)?.clone();
        let lane_root = self.existing_lane_root(name)?;
        let lane_jj = JjClient::new(&lane_root);
        lane_jj.snapshot_recovering_stale()?;

        let trunk = self.resolve_trunk()?;
        let trunk_root = self.trunk_root(&trunk)?;
        let trunk_jj = JjClient::new(&trunk_root);
        let jj = JjClient::new(self.workspace_root());
        let lane_sym = lane_symbol(name);

        if !jj.is_ancestor(&trunk.head_commit, &lane_sym)? {
            let behind = jj.count(&format!("::{} ~ ::{lane_sym}", trunk.head_commit))?;
            return Err(Error::LaneNotSynced {
                lane: name.as_str().to_owned(),
                behind,
            });
        }

        let conflicts = jj.count(&format!(
            "conflicts() & ({}..{lane_sym})",
            trunk.head_commit
        ))?;
        if conflicts > 0 {
            return Err(Error::LaneConflicted {
                lane: name.as_str().to_owned(),
                count: conflicts,
            });
        }

        if lane_jj.is_empty_commit("@")? {
            let parents = lane_jj.revisions("parents(@)")?;
            if let [parent] = parents.as_slice()
                && parent.commit_id == trunk.head_commit
            {
                return Err(Error::LaneNothingToLand(name.as_str().to_owned()));
            }
        }

        // All refusals happen before any mutation: a rejected landing must
        // leave no describe/park artifacts behind. The lane working copy's
        // endpoint diff equals the future landed diff, so scope-check it
        // directly.
        let changed = jj.changed_paths(&trunk.head_commit, &lane_sym)?;
        let unscoped = unscoped_paths(&changed, &record.paths);
        if !unscoped.is_empty() {
            return Err(Error::LaneUnscopedChanges {
                lane: name.as_str().to_owned(),
                paths: unscoped.join("\n"),
            });
        }

        // Trunk dirt outside the write-set rides along untouched; dirt
        // inside it would silently merge with the landing, so refuse.
        trunk_jj.snapshot_recovering_stale()?;
        let trunk_dirty = jj.changed_paths_in(&format!("{}@", trunk.name.as_str()))?;
        let dirty_in_scope: Vec<String> = trunk_dirty
            .into_iter()
            .filter(|path| record.paths.iter().any(|lane_path| lane_path.contains(path)))
            .collect();
        if !dirty_in_scope.is_empty() {
            return Err(Error::LaneTrunkDirtyInScope {
                lane: name.as_str().to_owned(),
                paths: dirty_in_scope.join("\n"),
            });
        }

        let gate = if no_gate {
            None
        } else {
            self.repo_config().lane.gate.clone()
        };
        if let Some(command) = &gate {
            run_gate(command, &lane_root)?;
        }

        let head_commit = Self::finalize_landing_head(&lane_jj, &trunk, name, message)?;

        let landed_changes = jj.count(&format!(
            "({}..{head_commit}) ~ empty()",
            trunk.head_commit
        ))?;

        // Fast-forward: move trunk's working copy onto the landed head.
        // Run inside the trunk workspace so its working copy stays current.
        trunk_jj.rebase_source("@", &head_commit)?;

        store.record_land(name, &head_commit, OffsetDateTime::now_utc())?;
        store.save()?;

        let fanout = self.fan_out(&jj, &store, name, &head_commit);

        let closed = if close {
            self.close_landed_lane(&mut store, name, &head_commit)?;
            true
        } else {
            false
        };

        Ok(LaneLandOutcome {
            name: name.clone(),
            landed: self.rev(&head_commit)?,
            landed_changes,
            gate,
            fanout,
            closed,
        })
    }

    /// Close a fully landed lane: forget its workspace and delete its
    /// directory.
    ///
    /// # Errors
    ///
    /// Returns an error if the lane still has unlanded, non-empty changes.
    pub fn lane_close(&self, name: &WorkspaceName) -> Result<PathBuf> {
        let mut store = LaneStore::load(self.repo_storage_path())?;
        require_open_lane(&store, name)?;
        let lane_root = self.existing_lane_root(name)?;
        let lane_jj = JjClient::new(&lane_root);
        lane_jj.snapshot_recovering_stale()?;

        let trunk = self.resolve_trunk()?;
        let jj = JjClient::new(self.workspace_root());
        let lane_sym = lane_symbol(name);
        let unlanded = jj.count(&format!(
            "(::{lane_sym} ~ ::{}) ~ empty()",
            trunk.head_commit
        ))?;
        if unlanded > 0 {
            return Err(Error::LaneNotLanded {
                lane: name.as_str().to_owned(),
            });
        }

        let removable = self.resolve_removable_workspace_path(name)?;
        self.forget_workspace(name)?;
        fs::remove_dir_all(&removable).map_err(|source| {
            Error::WorkspaceDirectoryDeleteAfterForgetFailed {
                workspace: name.as_str().to_owned(),
                path: removable.display().to_string(),
                source,
            }
        })?;

        store.set_lifecycle(name, LaneLifecycle::Closed, OffsetDateTime::now_utc())?;
        store.save()?;
        Ok(removable)
    }

    /// Abandon a lane: archive its diff, forget its workspace, and delete
    /// its directory.
    ///
    /// # Errors
    ///
    /// Returns an error if the lane is unknown or archival fails.
    pub fn lane_abandon(&self, name: &WorkspaceName) -> Result<LaneAbandonOutcome> {
        let mut store = LaneStore::load(self.repo_storage_path())?;
        require_open_lane(&store, name)?;

        let jj = JjClient::new(self.workspace_root());
        let workspace_exists = self.workspace_exists(name)?;
        let mut archive = None;
        let mut removed_directory = None;

        if workspace_exists {
            let lane_sym = lane_symbol(name);
            let trunk = self.resolve_trunk()?;
            if let Ok(root) = self.existing_lane_root(name) {
                let lane_jj = JjClient::new(&root);
                let _ = lane_jj.snapshot_recovering_stale();
            }
            let base = scope_base(&jj, &trunk.head_commit, &lane_sym)?;
            let diff = jj.diff_git(&base, &lane_sym)?;
            if !diff.trim().is_empty() {
                archive = Some(self.write_archive(name, &diff)?);
            }

            let removable = self.resolve_removable_workspace_path(name)?;
            self.forget_workspace(name)?;
            fs::remove_dir_all(&removable).map_err(|source| {
                Error::WorkspaceDirectoryDeleteAfterForgetFailed {
                    workspace: name.as_str().to_owned(),
                    path: removable.display().to_string(),
                    source,
                }
            })?;
            removed_directory = Some(removable);
        }

        store.set_lifecycle(name, LaneLifecycle::Abandoned, OffsetDateTime::now_utc())?;
        store.save()?;

        Ok(LaneAbandonOutcome {
            name: name.clone(),
            archive,
            removed_directory,
        })
    }

    /// Plan garbage collection: ghost workspaces (registered in `jj` but
    /// directory gone) and orphaned open lanes (registry entry without a
    /// `jj` workspace).
    ///
    /// # Errors
    ///
    /// Returns an error if workspace discovery fails.
    pub fn lane_gc_plan(&self) -> Result<LaneGcPlan> {
        let store = LaneStore::load(self.repo_storage_path())?;
        let trunk_name = self.repo_config().lane.trunk.clone();
        let jj = JjClient::new(self.workspace_root());

        let mut plan = LaneGcPlan::default();
        let mut registered = Vec::new();
        for entry in jj.list_workspaces()? {
            registered.push(entry.name.clone());
            if entry.name == trunk_name || entry.name == *self.current_workspace_name() {
                continue;
            }
            let resolved = self.resolve_workspace_path(&entry.name)?;
            if resolved.state == WorkspacePathState::Missing {
                plan.ghost_workspaces.push(entry.name);
            }
        }

        for record in store.open_lanes() {
            if !registered.contains(&record.name) {
                plan.orphaned_lanes.push(record.name.clone());
            }
        }

        Ok(plan)
    }

    /// Apply a garbage collection plan: forget ghosts, mark orphaned lanes
    /// abandoned.
    ///
    /// # Errors
    ///
    /// Returns an error if a forget or registry save fails.
    pub fn lane_gc_apply(&self, plan: &LaneGcPlan) -> Result<()> {
        for workspace in &plan.ghost_workspaces {
            self.forget_workspace(workspace)?;
        }

        if !plan.orphaned_lanes.is_empty() {
            let mut store = LaneStore::load(self.repo_storage_path())?;
            let now = OffsetDateTime::now_utc();
            for lane in &plan.orphaned_lanes {
                store.set_lifecycle(lane, LaneLifecycle::Abandoned, now)?;
            }
            store.save()?;
        }

        Ok(())
    }

    fn sync_one(
        &self,
        jj: &JjClient,
        trunk: &TrunkContext,
        record: &LaneRecord,
        drop_unscoped: bool,
    ) -> Result<LaneSyncOutcome> {
        let mut outcome = LaneSyncOutcome {
            name: record.name.clone(),
            workspace_exists: self.workspace_exists(&record.name)?,
            recovered_stale: false,
            rebased: false,
            conflicts: Vec::new(),
            dropped: Vec::new(),
        };
        if !outcome.workspace_exists {
            return Ok(outcome);
        }

        let lane_root = self.existing_lane_root(&record.name)?;
        let lane_jj = JjClient::new(&lane_root);
        outcome.recovered_stale = lane_jj.snapshot_recovering_stale()?;

        let lane_sym = lane_symbol(&record.name);
        if !jj.is_ancestor(&trunk.head_commit, &lane_sym)? {
            jj.rebase_branch_onto(&lane_sym, &trunk.head_commit)?;
            outcome.rebased = true;
            // The rebase rewrote the lane's working-copy commit from
            // outside the lane workspace; make it current again.
            lane_jj.workspace_update_stale()?;
        }

        outcome.conflicts = jj
            .revisions(&format!(
                "conflicts() & ({}..{lane_sym})",
                trunk.head_commit
            ))?
            .into_iter()
            .map(to_lane_rev)
            .collect();

        if drop_unscoped {
            let unscoped = unscoped_paths(
                &jj.changed_paths(&trunk.head_commit, &lane_sym)?,
                &record.paths,
            );
            if !unscoped.is_empty() {
                lane_jj.restore_paths(&trunk.head_commit, &unscoped)?;
                outcome.dropped = unscoped;
            }
        }

        Ok(outcome)
    }

    /// Determine and finalize the commit to land. A non-empty lane working
    /// copy is described and parked behind a fresh empty child so landed
    /// history is never a live working-copy commit.
    fn finalize_landing_head(
        lane_jj: &JjClient,
        trunk: &TrunkContext,
        name: &WorkspaceName,
        message: Option<&str>,
    ) -> Result<String> {
        let wc = lane_jj
            .revisions("@")?
            .into_iter()
            .next()
            .ok_or_else(|| Error::LaneWorkspaceMissing(name.as_str().to_owned()))?;

        if lane_jj.is_empty_commit("@")? {
            let parents = lane_jj.revisions("parents(@)")?;
            let [parent] = parents.as_slice() else {
                return Err(Error::LaneTrunkNotReady {
                    trunk: trunk.name.as_str().to_owned(),
                    reason: String::from("lane working copy has multiple parents"),
                });
            };
            if parent.commit_id == trunk.head_commit {
                return Err(Error::LaneNothingToLand(name.as_str().to_owned()));
            }
            if parent.message.is_empty() {
                let Some(message) = message else {
                    return Err(Error::LaneNeedsMessage(name.as_str().to_owned()));
                };
                lane_jj.describe(&parent.commit_id, message)?;
                let described = lane_jj.revisions("parents(@)")?;
                let [described] = described.as_slice() else {
                    return Err(Error::LaneWorkspaceMissing(name.as_str().to_owned()));
                };
                return Ok(described.commit_id.clone());
            }
            return Ok(parent.commit_id.clone());
        }

        if wc.message.is_empty() {
            let Some(message) = message else {
                return Err(Error::LaneNeedsMessage(name.as_str().to_owned()));
            };
            lane_jj.describe("@", message)?;
        }
        // Park the described work behind a fresh empty working copy.
        lane_jj.new_working_copy("@")?;
        let landed = lane_jj.revisions("parents(@)")?;
        let [landed] = landed.as_slice() else {
            return Err(Error::LaneWorkspaceMissing(name.as_str().to_owned()));
        };
        Ok(landed.commit_id.clone())
    }

    fn fan_out(
        &self,
        jj: &JjClient,
        store: &LaneStore,
        landed: &WorkspaceName,
        head_commit: &str,
    ) -> Vec<LaneFanoutEntry> {
        let mut entries = Vec::new();
        for record in store.open_lanes() {
            if record.name == *landed {
                continue;
            }
            let mut entry = LaneFanoutEntry {
                name: record.name.clone(),
                rebased: false,
                conflicts: 0,
                error: None,
            };
            let lane_sym = lane_symbol(&record.name);
            let result = (|| -> Result<()> {
                if !self.workspace_exists(&record.name)? {
                    return Ok(());
                }
                if jj.is_ancestor(head_commit, &lane_sym)? {
                    return Ok(());
                }
                jj.rebase_branch_onto(&lane_sym, head_commit)?;
                entry.rebased = true;
                entry.conflicts =
                    jj.count(&format!("conflicts() & ({head_commit}..{lane_sym})"))?;
                Ok(())
            })();
            if let Err(error) = result {
                entry.error = Some(error.to_string());
            }
            entries.push(entry);
        }
        entries
    }

    fn close_landed_lane(
        &self,
        store: &mut LaneStore,
        name: &WorkspaceName,
        head_commit: &str,
    ) -> Result<()> {
        let jj = JjClient::new(self.workspace_root());
        let lane_sym = lane_symbol(name);
        let unlanded = jj.count(&format!("(::{lane_sym} ~ ::{head_commit}) ~ empty()"))?;
        if unlanded > 0 {
            return Err(Error::LaneNotLanded {
                lane: name.as_str().to_owned(),
            });
        }
        let removable = self.resolve_removable_workspace_path(name)?;
        self.forget_workspace(name)?;
        fs::remove_dir_all(&removable).map_err(|source| {
            Error::WorkspaceDirectoryDeleteAfterForgetFailed {
                workspace: name.as_str().to_owned(),
                path: removable.display().to_string(),
                source,
            }
        })?;
        store.set_lifecycle(name, LaneLifecycle::Closed, OffsetDateTime::now_utc())?;
        store.save()?;
        Ok(())
    }

    fn resolve_trunk(&self) -> Result<TrunkContext> {
        let name = self.repo_config().lane.trunk.clone();
        if !self.workspace_exists(&name)? {
            return Err(Error::LaneTrunkMissing(name.as_str().to_owned()));
        }
        let jj = JjClient::new(self.workspace_root());
        let parents = jj.revisions(&format!("parents({}@)", name.as_str()))?;
        let [head] = parents.as_slice() else {
            return Err(Error::LaneTrunkNotReady {
                trunk: name.as_str().to_owned(),
                reason: format!(
                    "trunk working copy has {} parents; expected exactly one",
                    parents.len()
                ),
            });
        };
        Ok(TrunkContext {
            name,
            head_commit: head.commit_id.clone(),
        })
    }

    fn trunk_root(&self, trunk: &TrunkContext) -> Result<PathBuf> {
        if *self.current_workspace_name() == trunk.name {
            return Ok(self.workspace_root().to_path_buf());
        }
        let resolved = self.resolve_workspace_path(&trunk.name)?;
        if !resolved.path.is_dir() {
            return Err(Error::LaneTrunkMissing(trunk.name.as_str().to_owned()));
        }
        Ok(resolved.path)
    }

    fn existing_lane_root(&self, name: &WorkspaceName) -> Result<PathBuf> {
        if !self.workspace_exists(name)? {
            return Err(Error::LaneWorkspaceMissing(name.as_str().to_owned()));
        }
        let resolved = self.resolve_workspace_path(name)?;
        if !resolved.path.is_dir() {
            return Err(Error::LaneWorkspaceMissing(name.as_str().to_owned()));
        }
        Ok(resolved.path)
    }

    fn write_archive(&self, name: &WorkspaceName, diff: &str) -> Result<PathBuf> {
        let dir = navi_dir_path(self.repo_storage_path()).join(ARCHIVE_DIR);
        fs::create_dir_all(&dir)?;
        let stamp = OffsetDateTime::now_utc()
            .format(&Rfc3339)
            .unwrap_or_else(|_| String::from("unknown-time"))
            .replace(':', "-");
        let path = dir.join(format!("{}-{stamp}.diff", name.as_str()));
        fs::write(&path, diff)?;
        Ok(path)
    }

    fn rev(&self, revision: &str) -> Result<LaneRev> {
        let jj = JjClient::new(self.workspace_root());
        let revisions = jj.revisions(revision)?;
        revisions
            .into_iter()
            .next()
            .map(to_lane_rev)
            .ok_or_else(|| Error::JjCommandFailed {
                command: format!("jj log -r {revision}"),
                stderr: String::from("revision not found"),
            })
    }
}

fn to_lane_rev(summary: super::jj::JjRevisionSummary) -> LaneRev {
    LaneRev {
        commit_id: summary.commit_id,
        change_id: summary.change_id,
        message: summary.message,
    }
}

fn lane_symbol(name: &WorkspaceName) -> String {
    format!("{}@", name.as_str())
}

fn require_open_lane<'a>(store: &'a LaneStore, name: &WorkspaceName) -> Result<&'a LaneRecord> {
    let record = store
        .get(name)
        .ok_or_else(|| Error::LaneNotFound(name.as_str().to_owned()))?;
    if record.lifecycle != LaneLifecycle::Open {
        return Err(Error::LaneNotOpen {
            name: name.as_str().to_owned(),
            lifecycle: record.lifecycle.as_str(),
        });
    }
    Ok(record)
}

fn check_overlap_excluding(
    store: &LaneStore,
    name: &WorkspaceName,
    paths: &[LanePath],
) -> Result<()> {
    for lane in store.open_lanes() {
        if lane.name == *name {
            continue;
        }
        for path in paths {
            if let Some(other_path) = lane.paths.iter().find(|other| other.overlaps(path)) {
                return Err(Error::LaneOverlap {
                    path: path.as_str().to_owned(),
                    other: lane.name.as_str().to_owned(),
                    other_path: other_path.as_str().to_owned(),
                });
            }
        }
    }
    Ok(())
}

fn scope_base(jj: &JjClient, trunk_head: &str, lane_sym: &str) -> Result<String> {
    // The lane's own work is its diff from the fork point with trunk; a
    // synced lane's fork point is the trunk head itself.
    let bases = jj.revisions(&format!("heads(::{trunk_head} & ::{lane_sym})"))?;
    Ok(bases
        .into_iter()
        .next()
        .map_or_else(|| trunk_head.to_owned(), |base| base.commit_id))
}

fn unscoped_paths(changed: &[String], write_set: &[LanePath]) -> Vec<String> {
    changed
        .iter()
        .filter(|path| !write_set.iter().any(|lane_path| lane_path.contains(path)))
        .cloned()
        .collect()
}

fn run_gate(command: &str, lane_root: &std::path::Path) -> Result<()> {
    let status = Command::new("sh")
        .arg("-c")
        .arg(command)
        .current_dir(lane_root)
        .status()?;
    if !status.success() {
        return Err(Error::LaneGateFailed {
            command: command.to_owned(),
        });
    }
    Ok(())
}

use std::path::{Path, PathBuf};

use crate::diagnostics::{
    DoctorFinding, DoctorFindingCode, DoctorReport, DoctorScope, DoctorSeverity,
};
use crate::error::{Error, Result};
use crate::shell;
use crate::types::{
    RepoConfig, WorkspaceMetadataStatus, WorkspaceName, WorkspacePathSource, WorkspacePathState,
};

use super::config::load_repo_config;
use super::discovery::{find_workspace_root, resolve_repo_storage_path};
use super::jj::JjClient;
use super::metadata::{WorkspaceMetadataEntry, WorkspaceMetadataStore};
use super::paths::{
    derive_repo_name_for_doctor, display_path_for_list, should_report_missing_navi_metadata,
};
use super::workspace::{WorkspaceSnapshotInputs, collect_workspace_snapshots};

pub(crate) fn build_doctor_report(
    path: &Path,
    command_name: &str,
    deep: bool,
) -> Result<DoctorReport> {
    let cwd = path.canonicalize()?;
    let workspace_root = find_workspace_root(&cwd)?;
    let repo_storage_path = std::fs::canonicalize(resolve_repo_storage_path(&workspace_root)?)?;
    let mut report = DoctorReport::default();
    let current_workspace = {
        let jj = JjClient::new(&workspace_root);
        jj.ensure_supported_version()?;
        match jj.current_workspace_name() {
            Ok(current_workspace) => Some(current_workspace),
            Err(Error::OrphanedWorkspace) => {
                report.push(DoctorFinding {
                    severity: DoctorSeverity::Error,
                    code: DoctorFindingCode::OrphanedWorkspace,
                    scope: DoctorScope::Repo,
                    message: String::from(
                        "current directory is no longer a registered jj workspace",
                    ),
                    path: Some(workspace_root.display().to_string()),
                    hint: Some(String::from(
                        "cd into another workspace or recreate this workspace with jj",
                    )),
                });
                None
            }
            Err(error) => return Err(error),
        }
    };
    let repo_name = derive_repo_name_for_doctor(
        &repo_storage_path,
        &workspace_root,
        current_workspace.as_ref(),
    )?;
    let (config, config_is_valid) = match load_repo_config(&repo_storage_path) {
        Ok(config) => (config, true),
        Err(Error::InvalidRepoConfig { path, message }) => {
            report.push(DoctorFinding {
                severity: DoctorSeverity::Error,
                code: DoctorFindingCode::InvalidRepoConfig,
                scope: DoctorScope::Repo,
                message: format!("invalid repo config in {}", path.display()),
                path: Some(path.display().to_string()),
                hint: Some(message),
            });
            (RepoConfig::default(), false)
        }
        Err(error) => return Err(error),
    };
    let (metadata, metadata_is_valid) = match WorkspaceMetadataStore::load(&repo_storage_path) {
        Ok(metadata) => (metadata, true),
        Err(Error::InvalidWorkspaceMetadata { path, message }) => {
            report.push(DoctorFinding {
                severity: DoctorSeverity::Error,
                code: DoctorFindingCode::InvalidWorkspaceMetadata,
                scope: DoctorScope::Repo,
                message: format!("invalid workspace metadata in {}", path.display()),
                path: Some(path.display().to_string()),
                hint: Some(message),
            });
            (WorkspaceMetadataStore::default(), false)
        }
        Err(error) => return Err(error),
    };
    let repo = DoctorWorkspace {
        workspace_root,
        repo_storage_path,
        current_workspace,
        config,
        config_is_valid,
        metadata_is_valid,
        repo_name,
    };
    let jj = JjClient::new(&repo.workspace_root);

    report
        .findings
        .extend(repo.collect_workspace_findings(&jj, &metadata)?);
    report
        .findings
        .extend(shell::doctor_findings(command_name)?);
    if deep {
        report.findings.extend(collect_deep_findings(
            &repo.workspace_root,
            &repo.repo_storage_path,
            &repo.config,
        ));
    }
    report.sort();
    Ok(report)
}

/// Deep hygiene findings: divergence, conflicts, orphan heads, op churn,
/// and merged-then-amended landings. Failures degrade to findings instead
/// of aborting the report.
fn deep_finding(
    severity: DoctorSeverity,
    code: DoctorFindingCode,
    message: String,
    hint: Option<String>,
) -> DoctorFinding {
    DoctorFinding {
        severity,
        code,
        scope: DoctorScope::Repo,
        message,
        path: None,
        hint,
    }
}

/// Divergence and op-churn findings from the embedded engine.
fn engine_findings(workspace_root: &Path) -> Vec<DoctorFinding> {
    let mut findings = Vec::new();
    match super::jj::config_list_all(workspace_root)
        .and_then(|config| crate::engine::Engine::open(workspace_root, &config))
    {
        Ok(engine) => {
            match engine.divergent_changes() {
                Ok(changes) if changes.is_empty() => findings.push(deep_finding(
                    DoctorSeverity::Info,
                    DoctorFindingCode::DivergentChanges,
                    String::from("no divergent changes"),
                    None,
                )),
                Ok(changes) => findings.push(deep_finding(
                    DoctorSeverity::Warning,
                    DoctorFindingCode::DivergentChanges,
                    format!("{} divergent change(s)", changes.len()),
                    Some(String::from("run: navi heal")),
                )),
                Err(error) => findings.push(deep_finding(
                    DoctorSeverity::Warning,
                    DoctorFindingCode::DivergentChanges,
                    format!("could not enumerate divergent changes: {error}"),
                    None,
                )),
            }
            match engine.op_churn(86_400, 5_000) {
                Ok(churn) => findings.push(deep_finding(
                    if churn.capped {
                        DoctorSeverity::Warning
                    } else {
                        DoctorSeverity::Info
                    },
                    DoctorFindingCode::OpChurn,
                    format!(
                        "{}{} operation(s) in the last 24h",
                        churn.recent,
                        if churn.capped { "+" } else { "" }
                    ),
                    churn.capped.then(|| {
                        String::from(
                            "op log is very large and slows every navi/jj command; compact it: jj op abandon ..<old-op-id> then jj util gc",
                        )
                    }),
                )),
                Err(error) => findings.push(deep_finding(
                    DoctorSeverity::Warning,
                    DoctorFindingCode::OpChurn,
                    format!("could not walk the operation log: {error}"),
                    None,
                )),
            }
        }
        Err(error) => findings.push(deep_finding(
            DoctorSeverity::Warning,
            DoctorFindingCode::DivergentChanges,
            format!("embedded engine unavailable: {error}"),
            None,
        )),
    }
    findings
}

/// Deep hygiene findings from the jj CLI plus the lane registry.
fn collect_deep_findings(
    workspace_root: &Path,
    repo_storage_path: &Path,
    config: &RepoConfig,
) -> Vec<DoctorFinding> {
    let mut findings = engine_findings(workspace_root);
    let jj = JjClient::new(workspace_root);

    if let Some(bookmark) = &config.lane.target {
        findings.extend(target_hygiene_findings(&jj, bookmark));
    }

    match jj.count("conflicts()") {
        Ok(0) => findings.push(deep_finding(
            DoctorSeverity::Info,
            DoctorFindingCode::ConflictedCommits,
            String::from("no visible conflicted commits"),
            None,
        )),
        Ok(count) => findings.push(deep_finding(
            DoctorSeverity::Warning,
            DoctorFindingCode::ConflictedCommits,
            format!("{count} visible conflicted commit(s)"),
            Some(String::from(
                "conflicts in a bookmark's ancestry will block pushing it",
            )),
        )),
        Err(error) => findings.push(deep_finding(
            DoctorSeverity::Warning,
            DoctorFindingCode::ConflictedCommits,
            format!("could not count conflicted commits: {error}"),
            None,
        )),
    }

    match jj.revisions("heads(all()) ~ working_copies() ~ bookmarks()") {
        Ok(orphans) if orphans.is_empty() => findings.push(deep_finding(
            DoctorSeverity::Info,
            DoctorFindingCode::OrphanHeads,
            String::from("every head is a working copy or bookmarked"),
            None,
        )),
        Ok(orphans) => {
            let sample: Vec<&str> = orphans
                .iter()
                .take(5)
                .map(|revision| revision.commit_id.as_str())
                .collect();
            findings.push(deep_finding(
                DoctorSeverity::Warning,
                DoctorFindingCode::OrphanHeads,
                format!(
                    "{} orphan head(s): work not owned by any workspace or bookmark",
                    orphans.len()
                ),
                Some(format!("for example: {}", sample.join(", "))),
            ));
        }
        Err(error) => findings.push(deep_finding(
            DoctorSeverity::Warning,
            DoctorFindingCode::OrphanHeads,
            format!("could not inventory heads: {error}"),
            None,
        )),
    }

    findings.extend(merged_then_amended_findings(&jj, repo_storage_path));
    findings
}

/// Push-blockers in the landing target's ancestry: conflicts, divergence,
/// and undescribed non-empty commits anywhere below the bookmark.
fn target_hygiene_findings(jj: &JjClient<'_>, bookmark: &str) -> Vec<DoctorFinding> {
    let symbol = super::jj::quote_revset_string(bookmark);
    let checks: [(&str, String); 3] = [
        ("conflicted commit(s)", format!("conflicts() & ::{symbol}")),
        ("divergent change(s)", format!("divergent() & ::{symbol}")),
        (
            "undescribed non-empty commit(s)",
            format!("::{symbol} & description(exact:\"\") ~ empty() ~ root()"),
        ),
    ];

    let mut findings = Vec::new();
    let mut clean = true;
    for (what, revset) in checks {
        match jj.count(&revset) {
            Ok(0) => {}
            Ok(count) => {
                clean = false;
                findings.push(deep_finding(
                    DoctorSeverity::Warning,
                    DoctorFindingCode::TargetHygiene,
                    format!("{count} {what} in ::{bookmark}"),
                    Some(String::from(
                        "these block pushing the target; landing onto it is refused until clean",
                    )),
                ));
            }
            Err(error) => {
                clean = false;
                findings.push(deep_finding(
                    DoctorSeverity::Warning,
                    DoctorFindingCode::TargetHygiene,
                    format!("could not check {what} for ::{bookmark}: {error}"),
                    None,
                ));
            }
        }
    }
    if clean {
        findings.push(deep_finding(
            DoctorSeverity::Info,
            DoctorFindingCode::TargetHygiene,
            format!("target '{bookmark}' ancestry is clean and pushable"),
            None,
        ));
    }
    findings
}

/// Pinned-landing check: a landed change whose change id now resolves to a
/// different commit was amended after landing; descendant rebases will smear
/// conflicts into other people's working copies.
fn merged_then_amended_findings(
    jj: &JjClient<'_>,
    repo_storage_path: &Path,
) -> Vec<DoctorFinding> {
    let Ok(store) = super::lane_store::LaneStore::load(repo_storage_path) else {
        return Vec::new();
    };

    let mut findings = Vec::new();
    for record in store.all_lanes() {
        let Some(land) = &record.last_land else {
            continue;
        };
        let Some(change_id) = &land.change_id else {
            continue;
        };
        let message = match jj.revisions(change_id) {
            Ok(revisions) => match revisions.as_slice() {
                [revision] if revision.commit_id == land.head_commit => continue,
                [revision] => format!(
                    "lane '{}' landed change {change_id} as {} but it now points at {}",
                    record.name, land.head_commit, revision.commit_id
                ),
                [] => format!(
                    "lane '{}' landed change {change_id} but it is no longer visible",
                    record.name
                ),
                _ => format!(
                    "lane '{}' landed change {change_id} which is now divergent",
                    record.name
                ),
            },
            Err(_) => format!(
                "lane '{}' landed change {change_id} which no longer resolves cleanly",
                record.name
            ),
        };
        findings.push(DoctorFinding {
            severity: DoctorSeverity::Warning,
            code: DoctorFindingCode::MergedThenAmended,
            scope: DoctorScope::Workspace {
                workspace: record.name.as_str().to_owned(),
            },
            message,
            path: None,
            hint: Some(String::from(
                "amending after landing smears conflicts into descendants; prefer a follow-up change",
            )),
        });
    }
    findings
}

struct DoctorWorkspace {
    workspace_root: PathBuf,
    repo_storage_path: PathBuf,
    current_workspace: Option<WorkspaceName>,
    config: RepoConfig,
    config_is_valid: bool,
    metadata_is_valid: bool,
    repo_name: String,
}

impl DoctorWorkspace {
    fn collect_workspace_findings(
        &self,
        jj: &JjClient<'_>,
        metadata: &WorkspaceMetadataStore,
    ) -> Result<Vec<DoctorFinding>> {
        let snapshots = collect_workspace_snapshots(
            WorkspaceSnapshotInputs {
                workspace_root: &self.workspace_root,
                repo_storage_path: &self.repo_storage_path,
                current_workspace: self.current_workspace.as_ref(),
                config: &self.config,
                repo_name: &self.repo_name,
                metadata,
                metadata_is_valid: self.metadata_is_valid,
                allow_switchable_path: self.config_is_valid,
            },
            jj,
        )?;
        let mut findings = Vec::new();

        for snapshot in &snapshots {
            let display_path = display_path_for_list(&self.workspace_root, &snapshot.path.path)
                .display()
                .to_string();
            match snapshot.path.state {
                WorkspacePathState::Confirmed => {}
                WorkspacePathState::Inferred => {
                    let (message, hint) = match snapshot.path.source {
                        WorkspacePathSource::NaviMetadata => (
                            format!(
                                "workspace '{}' is using a validated metadata fallback path",
                                snapshot.name
                            ),
                            format!("resolved from navi metadata: {display_path}"),
                        ),
                        WorkspacePathSource::Template => (
                            format!(
                                "workspace '{}' is using a validated template path",
                                snapshot.name
                            ),
                            format!("resolved from workspace template: {display_path}"),
                        ),
                        WorkspacePathSource::CurrentWorkspace
                        | WorkspacePathSource::JjRecorded
                        | WorkspacePathSource::RepoPrimary => {
                            debug_assert!(
                                false,
                                "inferred workspace path used non-inferred source: {:?}",
                                snapshot.path.source
                            );
                            continue;
                        }
                    };
                    findings.push(inferred_path_finding(
                        &snapshot.name,
                        message,
                        hint,
                        display_path.clone(),
                    ));
                }
                WorkspacePathState::Missing => findings.push(workspace_finding(
                    DoctorSeverity::Warning,
                    DoctorFindingCode::WorkspaceDirectoryMissing,
                    &snapshot.name,
                    format!("workspace '{}' directory is missing", snapshot.name),
                    Some(format!("last known path: {display_path}")),
                    Some(display_path.clone()),
                )),
                WorkspacePathState::Stale => findings.push(workspace_finding(
                    DoctorSeverity::Warning,
                    DoctorFindingCode::WorkspaceDirectoryStale,
                    &snapshot.name,
                    format!("workspace '{}' directory is stale", snapshot.name),
                    Some(format!(
                        "best known path no longer validates: {display_path}"
                    )),
                    Some(display_path.clone()),
                )),
            }

            if self.metadata_is_valid
                && matches!(
                    snapshot.health.metadata_status,
                    WorkspaceMetadataStatus::MissingRecord
                )
                && should_report_missing_navi_metadata(&snapshot.name)
            {
                findings.push(workspace_finding(
                    DoctorSeverity::Info,
                    DoctorFindingCode::JjOnlyWorkspace,
                    &snapshot.name,
                    format!(
                        "workspace '{}' exists in jj but has no navi metadata",
                        snapshot.name
                    ),
                    None,
                    Some(display_path),
                ));
            }
        }

        if self.metadata_is_valid {
            findings.extend(self.collect_metadata_only_findings(&snapshots, metadata));
        }

        Ok(findings)
    }

    fn collect_metadata_only_findings(
        &self,
        snapshots: &[crate::types::WorkspaceSnapshot],
        metadata: &WorkspaceMetadataStore,
    ) -> Vec<DoctorFinding> {
        metadata
            .entries()
            .into_iter()
            .filter(|entry| !snapshots.iter().any(|snapshot| snapshot.name == entry.name))
            .map(|entry| self.metadata_only_finding(&entry))
            .collect()
    }

    fn metadata_only_finding(&self, entry: &WorkspaceMetadataEntry) -> DoctorFinding {
        let display_path = entry.path.as_ref().map(|path| {
            display_path_for_list(&self.workspace_root, path)
                .display()
                .to_string()
        });
        DoctorFinding {
            severity: DoctorSeverity::Warning,
            code: DoctorFindingCode::MetadataOnlyWorkspace,
            scope: DoctorScope::Workspace {
                workspace: entry.name.as_str().to_owned(),
            },
            message: format!(
                "metadata exists for workspace '{}' but jj no longer lists it",
                entry.name
            ),
            path: display_path,
            hint: Some(String::from("safe prune candidate")),
        }
    }
}

fn inferred_path_finding(
    workspace: &WorkspaceName,
    message: String,
    hint: String,
    display_path: String,
) -> DoctorFinding {
    workspace_finding(
        DoctorSeverity::Info,
        DoctorFindingCode::WorkspacePathInferred,
        workspace,
        message,
        Some(hint),
        Some(display_path),
    )
}

fn workspace_finding(
    severity: DoctorSeverity,
    code: DoctorFindingCode,
    workspace: &WorkspaceName,
    message: String,
    hint: Option<String>,
    path: Option<String>,
) -> DoctorFinding {
    DoctorFinding {
        severity,
        code,
        scope: DoctorScope::Workspace {
            workspace: workspace.as_str().to_owned(),
        },
        message,
        path,
        hint,
    }
}

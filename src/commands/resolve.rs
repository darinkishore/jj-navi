//! `navi conflicts` and `navi resolve`: conflict census and structural
//! auto-resolution.
//!
//! Conflicts propagate: one conflicted merge rebased through descendants
//! paints the same conflict across every one of them. The census ranks
//! conflict *roots* by blast radius; `resolve --union` fixes a root's
//! append-only file (both sides kept, in order) and lets jj's descendant
//! rebase dissolve the echo, looping until fixpoint.

use std::path::Path;

use crate::repo::NaviWorkspace;
use crate::{Error, Result};

/// Run `navi conflicts`: read-only census of conflict roots.
///
/// # Errors
///
/// Returns an error if the repo or engine cannot be opened.
pub fn run_conflicts(path: &Path, revisions: Option<&str>, json: bool) -> Result<()> {
    let repo = NaviWorkspace::open(path)?;
    let engine = repo.open_engine()?;

    // Optional scope: only conflicts in the ancestry of this revset.
    let scope: Option<Vec<String>> = revisions
        .map(|revset| -> Result<Vec<String>> {
            let heads = repo.main_jj_client().revisions(revset)?;
            if heads.is_empty() {
                return Err(Error::Engine {
                    message: format!("revset '{revset}' matches no commits"),
                });
            }
            Ok(heads.into_iter().map(|head| head.commit_id).collect())
        })
        .transpose()?;
    let mut roots = engine.conflict_roots(scope.as_deref())?;

    // Triage against the landing target: roots outside its ancestry are
    // stranded cleanup, not push-blockers. Best-effort — repos without a
    // resolvable target just skip the annotation.
    let target = if scope.is_none() {
        repo.landing_target_head().ok()
    } else {
        None
    };
    if let Some((label, head_commit)) = &target
        && !roots.is_empty()
    {
        let ids: Vec<String> = roots.iter().map(|root| root.commit_id.clone()).collect();
        if let Ok(members) = engine.ancestry_members(head_commit, &ids) {
            for root in &mut roots {
                root.blocks_target = Some(members.contains(&root.commit_id));
            }
        }
        let blocking = roots
            .iter()
            .filter(|root| root.blocks_target == Some(true))
            .count();
        if blocking == 0 {
            eprintln!("{label} ancestry is conflict-free; everything below is stranded cleanup");
        } else {
            eprintln!("{blocking} conflict root(s) BLOCK {label}");
        }
    }

    if roots.is_empty() {
        eprintln!("no conflicted commits");
    } else {
        let total: usize = roots.iter().map(|root| root.conflicted_descendants).sum();
        eprintln!(
            "{} conflict root(s) explain ~{total} conflicted commit(s)",
            roots.len()
        );
        for root in &roots {
            let files = if root.files.is_empty() {
                String::from("(none in tree?)")
            } else {
                root.files
                    .iter()
                    .map(|file| format!("{} ({} sides)", file.path, file.sides))
                    .collect::<Vec<_>>()
                    .join(", ")
            };
            let tag = match root.blocks_target {
                Some(true) => "  [BLOCKS TARGET]",
                Some(false) => "  [stranded]",
                None => "",
            };
            eprintln!(
                "  {}  blast {}{tag}  files: {files}",
                root.commit_id, root.conflicted_descendants,
            );
        }
        eprintln!();
        eprintln!("hint: navi resolve --union <file> heals append-only file conflicts at the roots");
    }

    if json {
        #[derive(serde::Serialize)]
        struct ConflictsResult<'a> {
            roots: &'a [crate::engine::ConflictRoot],
        }
        println!(
            "{}",
            crate::output::render_json_envelope("conflicts", &ConflictsResult { roots: &roots })?
        );
    }
    Ok(())
}

/// Run `navi resolve --union <path>`.
///
/// # Errors
///
/// Returns an error if the repo or engine cannot be opened, or if applying
/// a resolution fails.
pub fn run_resolve_union(path: &Path, target_file: &str, apply: bool, json: bool) -> Result<()> {
    let repo = NaviWorkspace::open(path)?;
    let report = resolve_union_file(&repo, target_file, apply)?;
    if json {
        println!(
            "{}",
            crate::output::render_json_envelope("resolve", &report)?
        );
    }
    Ok(())
}

/// Run `navi resolve` with no target: sweep every configured `[resolve]`
/// policy.
///
/// # Errors
///
/// Returns an error if no policies are configured or a sweep step fails.
pub fn run_resolve_policies(path: &Path, apply: bool, json: bool) -> Result<()> {
    let repo = NaviWorkspace::open(path)?;
    let policies = repo.repo_config().resolve.clone();
    if policies.is_empty() {
        return Err(Error::Engine {
            message: String::from(
                "no [resolve] policies configured\nhint: add e.g. \"CHANGELOG.md\" = \"union\" to the [resolve] table in navi config, or pass --union <file>",
            ),
        });
    }

    let mut reports = Vec::new();
    for policy in &policies {
        eprintln!("policy: '{}' -> {}", policy.path, policy.strategy.as_str());
        reports.push(resolve_union_file(&repo, &policy.path, apply)?);
    }
    if json {
        #[derive(serde::Serialize)]
        struct SweepResult {
            applied: bool,
            files: Vec<FileResolveReport>,
        }
        println!(
            "{}",
            crate::output::render_json_envelope("resolve", &SweepResult {
                applied: apply,
                files: reports,
            })?
        );
    }
    Ok(())
}

#[derive(serde::Serialize)]
struct FileResolveReport {
    file: String,
    strategy: &'static str,
    applied: bool,
    passes: usize,
    roots: Vec<ResolvedRootJson>,
    skipped: Vec<String>,
}

fn resolve_union_file(
    repo: &NaviWorkspace,
    target_file: &str,
    apply: bool,
) -> Result<FileResolveReport> {
    // One root resolves per pass (descendant rebases invalidate the other
    // roots' commit ids), so the cap must exceed any realistic root count.
    const MAX_PASSES: usize = 100;

    // Resolutions rebase descendants, which can include live workspaces'
    // working-copy commits. Snapshot them first so nothing un-snapshotted
    // can be stranded, and recover them after (same doctrine as lane
    // fan-out).
    let live_roots = if apply {
        let roots = repo.live_workspace_roots()?;
        for root in &roots {
            let _ = crate::repo::snapshot_working_copy_at(root);
        }
        roots
    } else {
        Vec::new()
    };

    let mut pass = 0;
    let mut resolved: Vec<ResolvedRootJson> = Vec::new();
    let mut skipped: Vec<String> = Vec::new();
    let mut planned: Option<Vec<ResolvedRootJson>> = None;

    let outcome = (|| -> Result<()> {
        loop {
            pass += 1;
            // Re-open the engine each pass: every applied resolution
            // rewrites history and rebases descendants.
            let engine = repo.open_engine()?;
            let roots = engine.conflict_roots(None)?;
            let targets: Vec<_> = roots
                .iter()
                .filter(|root| root.paths.iter().any(|p| p == target_file))
                .filter(|root| !skipped.iter().any(|s| s == &root.commit_id))
                .collect();

            if targets.is_empty() {
                return Ok(());
            }

            if !apply {
                render_union_plan(target_file, &targets);
                planned = Some(planned_json(&targets));
                return Ok(());
            }

            if pass > MAX_PASSES {
                eprintln!("stopped after {MAX_PASSES} passes; rerun to continue");
                return Ok(());
            }

            let mut progressed = false;
            for root in &targets {
                if let Some(union) = engine.union_merge_file(&root.commit_id, target_file)? {
                    apply_union(repo, target_file, &root.commit_id, &union)?;
                    eprintln!(
                        "resolved '{target_file}' at {} ({}-sided, pass {pass})",
                        root.commit_id, union.sides
                    );
                    resolved.push(ResolvedRootJson {
                        commit_id: root.commit_id.clone(),
                        conflicted_descendants: root.conflicted_descendants,
                        pass,
                    });
                    progressed = true;
                    // History changed; re-census before touching more roots.
                    break;
                }
                eprintln!(
                    "skip {}: '{target_file}' conflict is not union-safe (deleted side, binary, or non-file)",
                    root.commit_id
                );
                skipped.push(root.commit_id.clone());
            }
            if !progressed {
                return Ok(());
            }
        }
    })();

    // Recover live workspaces even when the loop failed part-way: an early
    // error must not strand anyone stale.
    if apply {
        for root in &live_roots {
            NaviWorkspace::recover_stale_at(root);
        }
    }
    outcome?;

    if let Some(planned) = planned {
        return Ok(FileResolveReport {
            file: target_file.to_owned(),
            strategy: "union",
            applied: false,
            passes: pass,
            roots: planned,
            skipped,
        });
    }

    if apply {
        eprintln!();
        eprintln!(
            "union-resolved '{target_file}' at {} root(s) across {pass} pass(es)",
            resolved.len()
        );
        if !skipped.is_empty() {
            eprintln!("  {} root(s) skipped as not union-safe", skipped.len());
        }
        eprintln!("  every resolution is a separate jj operation; jj op log to review");
    } else if resolved.is_empty() && skipped.is_empty() {
        eprintln!("no conflict roots carry '{target_file}' — nothing to resolve");
    }
    Ok(FileResolveReport {
        file: target_file.to_owned(),
        strategy: "union",
        applied: apply,
        passes: pass,
        roots: resolved,
        skipped,
    })
}

/// Apply a union resolution to one conflicted commit. Two-sided conflicts
/// go through `jj resolve --tool` (cheap); jj refuses more sides there, so
/// those squash the resolved file down from a scratch sparse workspace.
fn apply_union(
    repo: &NaviWorkspace,
    target_file: &str,
    commit_id: &str,
    union: &crate::engine::UnionFileMerge,
) -> Result<()> {
    if union.sides > 2 {
        return repo.with_mutation_lock(|| {
            repo.resolve_file_via_squash(commit_id, target_file, &union.content)
        });
    }
    let prepared = prepared_file_path(target_file)?;
    std::fs::write(&prepared, &union.content)?;
    let result = repo.with_mutation_lock(|| {
        repo.main_jj_client()
            .resolve_with_prepared_file(commit_id, target_file, &prepared)
    });
    let _ = std::fs::remove_file(&prepared);
    result
}

#[derive(serde::Serialize)]
struct ResolvedRootJson {
    commit_id: String,
    conflicted_descendants: usize,
    /// 0 in plan mode (nothing ran); otherwise the pass that resolved it.
    pass: usize,
}

fn planned_json(targets: &[&crate::engine::ConflictRoot]) -> Vec<ResolvedRootJson> {
    targets
        .iter()
        .map(|root| ResolvedRootJson {
            commit_id: root.commit_id.clone(),
            conflicted_descendants: root.conflicted_descendants,
            pass: 0,
        })
        .collect()
}


fn render_union_plan(target_file: &str, targets: &[&crate::engine::ConflictRoot]) {
    eprintln!(
        "would union-resolve '{target_file}' at {} conflict root(s):",
        targets.len()
    );
    for root in targets {
        eprintln!(
            "  {}  (blast {}: descendants re-merge automatically)",
            root.commit_id, root.conflicted_descendants
        );
    }
    eprintln!();
    eprintln!("plan only; rerun with --apply to resolve");
    eprintln!(
        "note: descendants that independently changed '{target_file}' re-conflict in the same shape; --apply loops until fixpoint"
    );
}

fn prepared_file_path(target_file: &str) -> Result<std::path::PathBuf> {
    let name = target_file.replace(['/', '\\'], "-");
    let dir = std::env::temp_dir();
    let path = dir.join(format!("navi-resolve-{}-{name}", std::process::id()));
    if path.to_str().is_none() {
        return Err(Error::Engine {
            message: String::from("temp path is not UTF-8"),
        });
    }
    Ok(path)
}

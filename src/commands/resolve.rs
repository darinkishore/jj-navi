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
pub fn run_conflicts(path: &Path, json: bool) -> Result<()> {
    let repo = NaviWorkspace::open(path)?;
    let engine = repo.open_engine()?;
    let roots = engine.conflict_roots()?;

    if roots.is_empty() {
        eprintln!("no conflicted commits");
    } else {
        let total: usize = roots.iter().map(|root| root.conflicted_descendants).sum();
        eprintln!(
            "{} conflict root(s) explain ~{total} conflicted commit(s)",
            roots.len()
        );
        for root in &roots {
            eprintln!(
                "  {}  blast {}  files: {}",
                root.commit_id,
                root.conflicted_descendants,
                if root.paths.is_empty() {
                    String::from("(none in tree?)")
                } else {
                    root.paths.join(", ")
                }
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
    const MAX_PASSES: usize = 10;

    let repo = NaviWorkspace::open(path)?;

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

    loop {
        pass += 1;
        // Re-open the engine each pass: every applied resolution rewrites
        // history and rebases descendants.
        let engine = repo.open_engine()?;
        let roots = engine.conflict_roots()?;
        let targets: Vec<_> = roots
            .iter()
            .filter(|root| root.paths.iter().any(|p| p == target_file))
            .filter(|root| !skipped.iter().any(|s| s == &root.commit_id))
            .collect();

        if targets.is_empty() {
            break;
        }

        if !apply {
            render_union_plan(target_file, &targets);
            if json {
                emit_resolve_envelope(target_file, false, pass, &planned_json(&targets), &skipped)?;
            }
            return Ok(());
        }

        if pass > MAX_PASSES {
            eprintln!("stopped after {MAX_PASSES} passes; rerun to continue");
            break;
        }

        let mut progressed = false;
        for root in &targets {
            if let Some(content) = engine.union_merge_file(&root.commit_id, target_file)? {
                let prepared = prepared_file_path(target_file)?;
                std::fs::write(&prepared, &content)?;
                let result = repo.with_mutation_lock(|| {
                    repo.main_jj_client().resolve_with_prepared_file(
                        &root.commit_id,
                        target_file,
                        &prepared,
                    )
                });
                let _ = std::fs::remove_file(&prepared);
                result?;
                eprintln!(
                    "resolved '{target_file}' at {} (pass {pass})",
                    root.commit_id
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
            break;
        }
    }

    if apply {
        for root in &live_roots {
            NaviWorkspace::recover_stale_at(root);
        }
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
    if json {
        emit_resolve_envelope(target_file, apply, pass, &resolved, &skipped)?;
    }
    Ok(())
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

fn emit_resolve_envelope(
    target_file: &str,
    applied: bool,
    passes: usize,
    roots: &[ResolvedRootJson],
    skipped: &[String],
) -> Result<()> {
    #[derive(serde::Serialize)]
    struct ResolveResult<'a> {
        file: &'a str,
        strategy: &'a str,
        applied: bool,
        passes: usize,
        roots: &'a [ResolvedRootJson],
        skipped: &'a [String],
    }
    println!(
        "{}",
        crate::output::render_json_envelope("resolve", &ResolveResult {
            file: target_file,
            strategy: "union",
            applied,
            passes,
            roots,
            skipped,
        })?
    );
    Ok(())
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

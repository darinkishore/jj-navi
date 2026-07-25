//! `navi tidy`: the whole repair pipeline as one idempotent verb.
//!
//! Order matters and encodes hard-won field experience: ghost workspaces
//! first (their pinned working copies block heals), then `[resolve]`
//! policies (conflicts at roots), then the guarded divergence heal. Plan
//! by default; `--apply` executes; everything is op-undoable.

use std::path::Path;

use crate::Result;
use crate::repo::NaviWorkspace;

/// Run `navi tidy`.
///
/// # Errors
///
/// Returns an error if any phase fails; completed phases stay applied
/// (each is independently undoable via the op log).
pub fn run_tidy(path: &Path, apply: bool, yes: bool, json: bool) -> Result<()> {
    let repo = NaviWorkspace::open(path)?;

    // Phase 1: ghost workspaces and orphaned lane records.
    eprintln!("== tidy: workspace gc");
    let gc_plan = repo.lane_gc_plan(false)?;
    let gc_applied = if apply && !gc_plan.is_empty() {
        if !yes {
            return Err(crate::Error::Engine {
                message: format!(
                    "tidy --apply would forget {} ghost workspace(s) and abandon {} orphaned lane(s)\nhint: rerun with --yes to confirm",
                    gc_plan.ghost_workspaces.len(),
                    gc_plan.orphaned_lanes.len()
                ),
            });
        }
        repo.lane_gc_apply(&gc_plan)?;
        true
    } else {
        false
    };
    eprint!("{}", crate::output::render_lane_gc(&gc_plan, gc_applied));

    // Phase 2: configured conflict-resolution policies.
    eprintln!("== tidy: resolve policies");
    let resolve_reports = if repo.repo_config().resolve.is_empty() {
        eprintln!("no [resolve] policies configured; skipping");
        Vec::new()
    } else {
        crate::commands::resolve::sweep_policies(&repo, apply)?
    };

    // Phase 3: guarded divergence heal.
    eprintln!("== tidy: heal divergence");
    let heal_report = crate::commands::heal::run_heal_in(
        &repo,
        &crate::commands::heal::HealOptions {
            changes: &[],
            mine: false,
            apply,
            limit: 100,
            prefer_content: false,
            json: false,
        },
    )?;

    repo.divergence_tripwire();
    if !apply {
        eprintln!();
        eprintln!("plan only; rerun with --apply (--yes to confirm the gc phase)");
    }

    if json {
        #[derive(serde::Serialize)]
        struct TidyResult<'a> {
            applied: bool,
            gc: &'a crate::types::LaneGcPlan,
            gc_applied: bool,
            resolve: &'a [crate::commands::resolve::FileResolveReport],
            heal: &'a crate::commands::heal::HealReport,
        }
        println!(
            "{}",
            crate::output::render_json_envelope(
                "tidy",
                &TidyResult {
                    applied: apply,
                    gc: &gc_plan,
                    gc_applied,
                    resolve: &resolve_reports,
                    heal: &heal_report,
                }
            )?
        );
    }
    Ok(())
}

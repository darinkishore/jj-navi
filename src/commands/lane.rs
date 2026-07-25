use std::io::{self, Write};
use std::path::Path;

use crate::Error;
use crate::Result;
use crate::output::{
    render_json_envelope, render_lane_abandon_outcome, render_lane_gc, render_lane_land_outcome,
    render_lane_list, render_lane_list_json, render_lane_open_outcome, render_lane_sync_outcomes,
};
use crate::repo::NaviWorkspace;
use crate::types::{LanePath, WorkspaceName};

/// Run `lane open`.
///
/// # Errors
///
/// Returns an error on validation failures or failing `jj` operations.
pub fn run_lane_open(
    path: &Path,
    name: &str,
    paths: &[String],
    allow_overlap: bool,
    sparse: Option<bool>,
    json: bool,
) -> Result<()> {
    let repo = NaviWorkspace::open(path)?;
    let name = WorkspaceName::new(name.to_owned())?;
    let paths = parse_lane_paths(paths)?;
    let outcome = repo.lane_open(&name, paths, allow_overlap, sparse)?;
    eprint!("{}", render_lane_open_outcome(&outcome));
    if json {
        println!("{}", render_json_envelope("lane open", &outcome)?);
    }
    Ok(())
}

/// Run `lane claim`.
///
/// # Errors
///
/// Returns an error if the lane is not open or a path overlaps another lane.
pub fn run_lane_claim(
    path: &Path,
    name: &str,
    paths: &[String],
    allow_overlap: bool,
    json: bool,
) -> Result<()> {
    let repo = NaviWorkspace::open(path)?;
    let name = WorkspaceName::new(name.to_owned())?;
    let paths = parse_lane_paths(paths)?;
    let merged = repo.lane_claim(&name, paths, allow_overlap)?;
    eprintln!(
        "lane '{name}' write-set: {}",
        merged
            .iter()
            .map(|lane_path| lane_path.as_str().to_owned())
            .collect::<Vec<_>>()
            .join(", ")
    );
    if json {
        #[derive(serde::Serialize)]
        struct ClaimResult<'a> {
            name: &'a WorkspaceName,
            write_set: &'a [LanePath],
        }
        println!(
            "{}",
            render_json_envelope(
                "lane claim",
                &ClaimResult {
                    name: &name,
                    write_set: &merged,
                }
            )?
        );
    }
    Ok(())
}

/// Run `lane list`.
///
/// # Errors
///
/// Returns an error if the registry or `jj` state cannot be read.
pub fn run_lane_list(path: &Path, json: bool, compact: bool) -> Result<()> {
    let repo = NaviWorkspace::open(path)?;
    let entries = repo.lane_list()?;
    if json {
        println!("{}", render_lane_list_json(&entries, compact)?);
    } else {
        eprint!("{}", render_lane_list(&entries));
    }
    Ok(())
}

/// Run `lane sync`.
///
/// # Errors
///
/// Returns an error if a named lane is unknown or trunk cannot be resolved.
pub fn run_lane_sync(
    path: &Path,
    name: Option<&str>,
    drop_unscoped: bool,
    json: bool,
) -> Result<()> {
    let repo = NaviWorkspace::open(path)?;
    let name = name
        .map(|value| WorkspaceName::new(value.to_owned()))
        .transpose()?;
    let outcomes = repo.lane_sync(name.as_ref(), drop_unscoped)?;
    eprint!("{}", render_lane_sync_outcomes(&outcomes));
    if json {
        #[derive(serde::Serialize)]
        struct SyncResult<'a> {
            outcomes: &'a [crate::types::LaneSyncOutcome],
        }
        println!(
            "{}",
            render_json_envelope("lane sync", &SyncResult {
                outcomes: &outcomes
            })?
        );
    }
    Ok(())
}

/// Run `lane land`.
///
/// # Errors
///
/// Returns an error if any landing precondition or the gate fails.
pub fn run_lane_land(
    path: &Path,
    name: &str,
    message: Option<&str>,
    no_gate: bool,
    close: bool,
    json: bool,
) -> Result<()> {
    let repo = NaviWorkspace::open(path)?;
    let name = WorkspaceName::new(name.to_owned())?;
    let outcome = repo.lane_land(&name, message, no_gate, close)?;
    eprint!("{}", render_lane_land_outcome(&outcome));
    if json {
        println!("{}", render_json_envelope("lane land", &outcome)?);
    }
    Ok(())
}

/// Run `lane close`.
///
/// # Errors
///
/// Returns an error if the lane still has unlanded work.
pub fn run_lane_close(path: &Path, name: &str, json: bool) -> Result<()> {
    let repo = NaviWorkspace::open(path)?;
    let name = WorkspaceName::new(name.to_owned())?;
    let removed = repo.lane_close(&name)?;
    eprintln!("closed lane '{name}'");
    eprintln!("  removed: {}", removed.display());
    if json {
        #[derive(serde::Serialize)]
        struct CloseResult<'a> {
            name: &'a WorkspaceName,
            removed_directory: &'a Path,
        }
        println!(
            "{}",
            render_json_envelope(
                "lane close",
                &CloseResult {
                    name: &name,
                    removed_directory: &removed,
                }
            )?
        );
    }
    Ok(())
}

/// Run `lane abandon`.
///
/// # Errors
///
/// Returns an error if the lane is unknown or archival fails.
pub fn run_lane_abandon(path: &Path, name: &str, yes: bool, json: bool) -> Result<()> {
    let repo = NaviWorkspace::open(path)?;
    let name = WorkspaceName::new(name.to_owned())?;
    if !yes {
        confirm(&format!(
            "This will archive and discard lane '{name}' and delete its workspace directory."
        ))?;
    }
    let outcome = repo.lane_abandon(&name)?;
    eprint!("{}", render_lane_abandon_outcome(&outcome));
    if json {
        println!("{}", render_json_envelope("lane abandon", &outcome)?);
    }
    Ok(())
}

/// Run `lane gc`.
///
/// # Errors
///
/// Returns an error if discovery, forgetting, or registry saves fail.
pub fn run_lane_gc(path: &Path, apply: bool, yes: bool, json: bool) -> Result<()> {
    let repo = NaviWorkspace::open(path)?;
    let plan = repo.lane_gc_plan()?;
    let nothing_to_do = plan.ghost_workspaces.is_empty() && plan.orphaned_lanes.is_empty();
    let applied = if apply && !nothing_to_do {
        if !yes {
            confirm(&format!(
                "This will forget {} ghost workspace(s) and abandon {} orphaned lane(s).",
                plan.ghost_workspaces.len(),
                plan.orphaned_lanes.len()
            ))?;
        }
        repo.lane_gc_apply(&plan)?;
        true
    } else {
        false
    };
    eprint!("{}", render_lane_gc(&plan, applied));
    if json {
        #[derive(serde::Serialize)]
        struct GcResult<'a> {
            plan: &'a crate::types::LaneGcPlan,
            applied: bool,
        }
        println!(
            "{}",
            render_json_envelope("lane gc", &GcResult {
                plan: &plan,
                applied,
            })?
        );
    }
    Ok(())
}

fn parse_lane_paths(paths: &[String]) -> Result<Vec<LanePath>> {
    paths
        .iter()
        .map(|path| LanePath::new(path.clone()))
        .collect()
}

/// Interactive destructive-action confirmation. The prompt goes to stderr:
/// stdout is reserved for machine output.
fn confirm(prompt: &str) -> Result<()> {
    eprintln!("{prompt}");
    eprint!("Type 'yes' to continue: ");
    io::stderr().flush()?;

    let mut answer = String::new();
    io::stdin().read_line(&mut answer)?;
    if answer.trim() == "yes" {
        Ok(())
    } else {
        Err(Error::RemoveCancelled)
    }
}

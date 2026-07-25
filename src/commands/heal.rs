//! `navi heal`: the divergence healer.
//!
//! For each divergent change, pick the sibling minted by the newest
//! operation as the winner and abandon the stale siblings — with the
//! one-writer law encoded: changes whose stale siblings are another
//! workspace's working copy, or carry stacked descendants, are skipped for
//! manual review. Plan by default; `--apply` executes the whole batch as a
//! single undoable jj operation.

use std::path::Path;

use crate::engine::{DivergentChange, DivergentSibling};
use crate::repo::NaviWorkspace;
use crate::{Error, Result};

pub struct HealOptions<'a> {
    /// Only heal changes whose id starts with one of these prefixes.
    pub changes: &'a [String],
    /// Only heal changes where every sibling was minted by my operations.
    pub mine: bool,
    /// Execute the plan instead of printing it.
    pub apply: bool,
    /// Maximum number of changes healed per run.
    pub limit: usize,
}

struct PlannedHeal<'a> {
    change: &'a DivergentChange,
    losers: Vec<&'a DivergentSibling>,
}

struct SkippedHeal<'a> {
    change: &'a DivergentChange,
    reason: String,
}

/// Run `navi heal`.
///
/// # Errors
///
/// Returns an error if the repo or engine cannot be opened, or if applying
/// the plan fails.
pub fn run_heal(path: &Path, options: &HealOptions<'_>) -> Result<()> {
    let repo = NaviWorkspace::open(path)?;
    let engine = repo.open_engine()?;
    let all = engine.divergent_changes()?;

    if all.is_empty() {
        eprintln!("no divergent changes — nothing to heal");
        return Ok(());
    }

    let me = engine.operation_username();
    let mut planned: Vec<PlannedHeal> = Vec::new();
    let mut skipped: Vec<SkippedHeal> = Vec::new();
    let mut filtered = 0usize;
    let mut over_limit = 0usize;

    for change in &all {
        if !options.changes.is_empty()
            && !options
                .changes
                .iter()
                .any(|prefix| change.change_id.starts_with(prefix.as_str()))
        {
            filtered += 1;
            continue;
        }
        if options.mine
            && !change.siblings.iter().all(|sibling| {
                sibling
                    .op
                    .as_ref()
                    .is_some_and(|op| op.username == me)
            })
        {
            filtered += 1;
            continue;
        }

        // Siblings are sorted newest-op first; the winner is siblings[0].
        let losers: Vec<&DivergentSibling> = change.siblings.iter().skip(1).collect();
        if let Some(loser) = losers.iter().find(|loser| !loser.wc_of.is_empty()) {
            skipped.push(SkippedHeal {
                change,
                reason: format!(
                    "stale sibling {} is the working copy of workspace '{}'",
                    loser.commit_id,
                    loser.wc_of.join("', '")
                ),
            });
            continue;
        }
        if let Some(loser) = losers.iter().find(|loser| loser.has_children) {
            skipped.push(SkippedHeal {
                change,
                reason: format!(
                    "stale sibling {} has stacked descendants; heal manually",
                    loser.commit_id
                ),
            });
            continue;
        }

        if planned.len() >= options.limit {
            over_limit += 1;
            continue;
        }
        planned.push(PlannedHeal { change, losers });
    }

    render_plan(&planned, &skipped, filtered, over_limit, options.apply);

    if !options.apply {
        if !planned.is_empty() {
            eprintln!();
            eprintln!("plan only; rerun with --apply to heal");
        }
        return Ok(());
    }
    if planned.is_empty() {
        return Ok(());
    }

    let loser_ids: Vec<String> = planned
        .iter()
        .flat_map(|heal| heal.losers.iter().map(|loser| loser.commit_id.clone()))
        .collect();
    repo.with_mutation_lock(|| {
        let jj = repo.main_jj_client();
        jj.abandon_commits(&loser_ids)
    })?;

    eprintln!();
    eprintln!(
        "healed {} change(s): abandoned {} stale sibling(s) in one operation",
        planned.len(),
        loser_ids.len()
    );
    eprintln!("  undo with: jj op undo");
    Ok(())
}

fn render_plan(
    planned: &[PlannedHeal<'_>],
    skipped: &[SkippedHeal<'_>],
    filtered: usize,
    over_limit: usize,
    applying: bool,
) {
    let verb = if applying { "healing" } else { "would heal" };
    for heal in planned {
        eprintln!("{verb} change {}", heal.change.change_id);
        for (index, sibling) in heal.change.siblings.iter().enumerate() {
            let role = if index == 0 { "keep   " } else { "abandon" };
            eprintln!("  {role} {}", describe_sibling(sibling));
        }
    }
    for skip in skipped {
        eprintln!("skip change {}", skip.change.change_id);
        eprintln!("  reason: {}", skip.reason);
    }
    if filtered > 0 {
        eprintln!("({filtered} divergent change(s) excluded by filters)");
    }
    if over_limit > 0 {
        eprintln!("({over_limit} healable change(s) beyond --limit; rerun to continue)");
    }
}

fn describe_sibling(sibling: &DivergentSibling) -> String {
    let provenance = sibling.op.as_ref().map_or_else(
        || String::from("op unknown"),
        |op| {
            format!(
                "op {} by {}@{} ({})",
                op.op_id, op.username, op.hostname, op.description
            )
        },
    );
    let description = if sibling.description.is_empty() {
        "(no description)"
    } else {
        sibling.description.as_str()
    };
    format!("{}  {description}  [{provenance}]", sibling.commit_id)
}

/// Parse and validate the `--limit` flag.
///
/// # Errors
///
/// Returns an error when the limit is zero.
pub fn validated_limit(limit: usize) -> Result<usize> {
    if limit == 0 {
        return Err(Error::Engine {
            message: String::from("--limit must be at least 1"),
        });
    }
    Ok(limit)
}

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
    /// Emit a machine envelope on stdout.
    pub json: bool,
}

#[derive(serde::Serialize)]
struct HealReport {
    applied: bool,
    healed: Vec<HealedChangeJson>,
    skipped: Vec<SkippedChangeJson>,
    filtered: usize,
    over_limit: usize,
    abandoned_commits: usize,
    rebased_chains: usize,
}

#[derive(serde::Serialize)]
struct HealedChangeJson {
    change_id: String,
    keep_commit: String,
    abandon_commits: Vec<String>,
}

#[derive(serde::Serialize)]
struct SkippedChangeJson {
    change_id: String,
    reason: String,
}

struct PlannedHeal<'a> {
    change: &'a DivergentChange,
    losers: Vec<&'a DivergentSibling>,
}

struct SkippedHeal<'a> {
    change: &'a DivergentChange,
    reason: String,
}

struct SelectedHeals<'a> {
    planned: Vec<PlannedHeal<'a>>,
    skipped: Vec<SkippedHeal<'a>>,
    filtered: usize,
    over_limit: usize,
}

fn select_heals<'a>(
    all: &'a [DivergentChange],
    options: &HealOptions<'_>,
    me: &str,
) -> SelectedHeals<'a> {
    let mut selected = SelectedHeals {
        planned: Vec::new(),
        skipped: Vec::new(),
        filtered: 0,
        over_limit: 0,
    };

    for change in all {
        if !options.changes.is_empty()
            && !options
                .changes
                .iter()
                .any(|prefix| change.change_id.starts_with(prefix.as_str()))
        {
            selected.filtered += 1;
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
            selected.filtered += 1;
            continue;
        }

        // Siblings are sorted newest-op first; the winner is siblings[0].
        let losers: Vec<&DivergentSibling> = change.siblings.iter().skip(1).collect();
        // One-writer law: never rewrite a chain that contains a live
        // working copy — that is how divergence gets minted, not healed.
        if let Some(loser) = losers.iter().find(|loser| loser.blocks_wc) {
            let reason = if loser.wc_of.is_empty() {
                format!(
                    "stale sibling {} carries a workspace's working copy in its descendants",
                    loser.commit_id
                )
            } else {
                format!(
                    "stale sibling {} is the working copy of workspace '{}'",
                    loser.commit_id,
                    loser.wc_of.join("', '")
                )
            };
            selected.skipped.push(SkippedHeal { change, reason });
            continue;
        }

        if selected.planned.len() >= options.limit {
            selected.over_limit += 1;
            continue;
        }
        selected.planned.push(PlannedHeal { change, losers });
    }

    selected
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
        if options.json {
            emit_heal_envelope(options.apply, &[], &[], 0, 0, 0, 0)?;
        }
        return Ok(());
    }

    let plan = select_heals(&all, options, engine.operation_username());
    let SelectedHeals {
        planned,
        skipped,
        filtered,
        over_limit,
    } = plan;

    render_plan(&planned, &skipped, filtered, over_limit, options.apply);

    let mut rebased_chains = 0usize;
    let mut abandoned = 0usize;
    if options.apply && !planned.is_empty() {
        let loser_ids: Vec<String> = planned
            .iter()
            .flat_map(|heal| heal.losers.iter().map(|loser| loser.commit_id.clone()))
            .collect();
        repo.with_mutation_lock(|| {
            let jj = repo.main_jj_client();
            // Move stacked descendants from each stale sibling onto its
            // winner (same change, newer version) before abandoning the
            // stale side.
            for heal in &planned {
                let winner = &heal.change.siblings[0];
                for loser in &heal.losers {
                    if loser.has_children {
                        jj.rebase_children_onto(&loser.commit_id, &winner.commit_id)?;
                        rebased_chains += 1;
                    }
                }
            }
            jj.abandon_commits(&loser_ids)
        })?;
        abandoned = loser_ids.len();

        eprintln!();
        eprintln!(
            "healed {} change(s): abandoned {} stale sibling(s), rebased {} descendant chain(s)",
            planned.len(),
            abandoned,
            rebased_chains
        );
        eprintln!("  each rebase and the final abandon are separate jj operations; jj op undo reverses the most recent");
    } else if !options.apply && !planned.is_empty() {
        eprintln!();
        eprintln!("plan only; rerun with --apply to heal");
    }

    if options.json {
        emit_heal_envelope(
            options.apply,
            &planned,
            &skipped,
            filtered,
            over_limit,
            abandoned,
            rebased_chains,
        )?;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)] // flat envelope inputs
fn emit_heal_envelope(
    applied: bool,
    planned: &[PlannedHeal<'_>],
    skipped: &[SkippedHeal<'_>],
    filtered: usize,
    over_limit: usize,
    abandoned_commits: usize,
    rebased_chains: usize,
) -> Result<()> {
    let report = HealReport {
        applied,
        healed: planned
            .iter()
            .map(|heal| HealedChangeJson {
                change_id: heal.change.change_id.clone(),
                keep_commit: heal.change.siblings[0].commit_id.clone(),
                abandon_commits: heal
                    .losers
                    .iter()
                    .map(|loser| loser.commit_id.clone())
                    .collect(),
            })
            .collect(),
        skipped: skipped
            .iter()
            .map(|skip| SkippedChangeJson {
                change_id: skip.change.change_id.clone(),
                reason: skip.reason.clone(),
            })
            .collect(),
        filtered,
        over_limit,
        abandoned_commits,
        rebased_chains,
    };
    println!("{}", crate::output::render_json_envelope("heal", &report)?);
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
            let role = if index == 0 {
                "keep   "
            } else if sibling.has_children {
                "abandon (rebasing its descendants onto keep)"
            } else {
                "abandon"
            };
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

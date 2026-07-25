//! `navi abandon -r <revset>`: bulk-abandon dead subtrees safely.
//!
//! Cleanup after heavy concurrent work leaves forests of stranded heads.
//! jj's abandon is view-only (commits hide, the op log keeps everything,
//! one `jj op undo` restores), so no file archive is needed — the guards
//! matter instead: never a live working copy's chain, never anything in
//! the landing target's ancestry.

use std::path::Path;

use crate::repo::NaviWorkspace;
use crate::{Error, Result};

/// Run `navi abandon`.
///
/// # Errors
///
/// Returns an error if the revset selects nothing, a guard trips, or the
/// abandon fails.
pub fn run_abandon(path: &Path, revset: &str, apply: bool, json: bool) -> Result<()> {
    let repo = NaviWorkspace::open(path)?;
    let jj = repo.main_jj_client();

    let selected = jj.revisions(revset)?;
    if selected.is_empty() {
        return Err(Error::Engine {
            message: format!("revset '{revset}' selects no commits"),
        });
    }

    // Guard: never a chain containing a live working copy.
    let wc_hits = jj.revisions(&format!("({revset}) & (working_copies() | ::working_copies())"))?;
    if !wc_hits.is_empty() {
        return Err(Error::Engine {
            message: format!(
                "revset '{revset}' includes commits in a live working copy's chain (for example {})\nhint: narrow the revset; working copies are never bulk-abandoned",
                wc_hits[0].commit_id
            ),
        });
    }
    // Guard: never published history.
    if let Ok((label, head)) = repo.landing_target_head() {
        let target_hits = jj.revisions(&format!("({revset}) & ::{head}"))?;
        if !target_hits.is_empty() {
            return Err(Error::Engine {
                message: format!(
                    "revset '{revset}' includes commits in {label}'s ancestry (for example {})\nhint: abandon only targets stranded work, never the mainline",
                    target_hits[0].commit_id
                ),
            });
        }
    }

    eprintln!(
        "{} {} commit(s) selected by '{revset}'",
        if apply { "abandoning" } else { "would abandon" },
        selected.len()
    );
    for revision in selected.iter().take(10) {
        eprintln!(
            "  {}  {}",
            revision.commit_id,
            if revision.message.is_empty() {
                "(no description)"
            } else {
                &revision.message
            }
        );
    }
    if selected.len() > 10 {
        eprintln!("  ... and {} more", selected.len() - 10);
    }

    if apply {
        let ids: Vec<String> = selected
            .iter()
            .map(|revision| revision.commit_id.clone())
            .collect();
        repo.with_mutation_lock(|| jj.abandon_commits(&ids))?;
        repo.divergence_tripwire();
        eprintln!();
        eprintln!(
            "abandoned {} commit(s) in one operation; jj op undo restores them",
            ids.len()
        );
    } else {
        eprintln!();
        eprintln!("plan only; rerun with --apply");
    }

    if json {
        #[derive(serde::Serialize)]
        struct AbandonResult<'a> {
            revset: &'a str,
            applied: bool,
            commits: Vec<&'a str>,
        }
        println!(
            "{}",
            crate::output::render_json_envelope(
                "abandon",
                &AbandonResult {
                    revset,
                    applied: apply,
                    commits: selected
                        .iter()
                        .map(|revision| revision.commit_id.as_str())
                        .collect(),
                }
            )?
        );
    }
    Ok(())
}

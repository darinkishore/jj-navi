//! Rendering for lane workflow commands.
//!
//! Lane health renders as weather. A lane fully rebased onto the trunk head
//! is clear sky; drift clouds it over; conflicts are a storm. The metaphor
//! is load-bearing: divergence, like weather, is nobody's fault and
//! everybody's problem, and the fix is always the same — sync before it
//! storms.

use std::fmt::Write as _;

use serde::Serialize;

use crate::types::{
    LaneAbandonOutcome, LaneGcPlan, LaneLandOutcome, LaneLifecycle, LaneListEntry,
    LaneOpenOutcome, LanePath, LaneSyncOutcome,
};

use super::style_meta;

/// Weather classification for a lane's live state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LaneWeather {
    /// Synced, conflict-free, in scope.
    Clear,
    /// Behind trunk but rebasing cleanly so far.
    Overcast,
    /// Carrying conflicted commits.
    Storm,
    /// Scope drift: changes outside the declared write-set.
    Fog,
    /// Workspace missing or lane terminal.
    Void,
}

impl LaneWeather {
    fn of(entry: &LaneListEntry) -> Self {
        if entry.lifecycle != LaneLifecycle::Open || !entry.workspace_exists {
            Self::Void
        } else if entry.conflicts > 0 {
            Self::Storm
        } else if !entry.unscoped.is_empty() {
            Self::Fog
        } else if entry.synced {
            Self::Clear
        } else {
            Self::Overcast
        }
    }

    const fn glyph(self) -> &'static str {
        match self {
            Self::Clear => "\u{2600}",    // ☀
            Self::Overcast => "\u{26c5}", // ⛅
            Self::Storm => "\u{26c8}",    // ⛈
            Self::Fog => "\u{1f32b}",     // 🌫
            Self::Void => "\u{00b7}",     // ·
        }
    }

    const fn label(self) -> &'static str {
        match self {
            Self::Clear => "clear",
            Self::Overcast => "overcast",
            Self::Storm => "storm",
            Self::Fog => "fog",
            Self::Void => "void",
        }
    }
}

/// Render the outcome of `lane open`.
#[must_use]
pub fn render_lane_open_outcome(outcome: &LaneOpenOutcome) -> String {
    let mut output = String::new();
    writeln!(
        output,
        "opened lane '{}'{}",
        outcome.name,
        if outcome.sparse { " (sparse)" } else { "" }
    )
    .expect("write lane open");
    writeln!(
        output,
        "  base: change {}  commit {}  {}",
        style_meta(&outcome.base.change_id),
        style_meta(&outcome.base.commit_id),
        outcome.base.message,
    )
    .expect("write lane open base");
    writeln!(
        output,
        "  write-set: {}",
        outcome
            .paths
            .iter()
            .map(|path| path.as_str().to_owned())
            .collect::<Vec<_>>()
            .join(", "),
    )
    .expect("write lane open paths");
    writeln!(output, "  next: cd {}", outcome.path.display()).expect("write lane open path");
    output
}

/// Render the lane table for `lane list`.
#[must_use]
pub fn render_lane_list(entries: &[LaneListEntry]) -> String {
    if entries.is_empty() {
        return String::from("no lanes registered\nhint: navi lane open NAME --path PATH\n");
    }

    let mut output = String::new();
    for entry in entries {
        let weather = LaneWeather::of(entry);
        let mut facts: Vec<String> = Vec::new();
        match entry.lifecycle {
            LaneLifecycle::Open if !entry.workspace_exists => {
                facts.push(String::from("workspace missing"));
            }
            LaneLifecycle::Open => {
                if entry.synced {
                    facts.push(String::from("synced"));
                } else {
                    facts.push(format!("behind {}", entry.behind));
                }
                facts.push(format!("ahead {}", entry.ahead));
                if entry.conflicts > 0 {
                    facts.push(format!("conflicts {}", entry.conflicts));
                }
                if !entry.unscoped.is_empty() {
                    facts.push(format!("unscoped {}", entry.unscoped.len()));
                }
            }
            LaneLifecycle::Closed => facts.push(String::from("closed")),
            LaneLifecycle::Abandoned => facts.push(String::from("abandoned")),
        }

        writeln!(
            output,
            "{} {}  [{}]  {}",
            weather.glyph(),
            entry.name,
            entry
                .paths
                .iter()
                .map(|path| path.as_str().to_owned())
                .collect::<Vec<_>>()
                .join(", "),
            facts.join(", "),
        )
        .expect("write lane row");
        if !entry.unscoped.is_empty() {
            for path in &entry.unscoped {
                writeln!(output, "    unscoped: {path}").expect("write unscoped row");
            }
        }
    }
    output
}

#[derive(Serialize)]
struct LaneListJson<'a> {
    lanes: Vec<LaneJsonEntry<'a>>,
}

#[derive(Serialize)]
struct LaneJsonEntry<'a> {
    name: &'a str,
    lifecycle: &'a str,
    weather: &'a str,
    paths: Vec<&'a str>,
    workspace_exists: bool,
    synced: bool,
    ahead: usize,
    behind: usize,
    conflicts: usize,
    unscoped: &'a [String],
    last_land: Option<&'a str>,
}

/// Render `lane list` as JSON.
///
/// # Errors
///
/// Returns an error if serialization fails.
pub fn render_lane_list_json(entries: &[LaneListEntry], compact: bool) -> crate::Result<String> {
    let payload = LaneListJson {
        lanes: entries
            .iter()
            .map(|entry| LaneJsonEntry {
                name: entry.name.as_str(),
                lifecycle: entry.lifecycle.as_str(),
                weather: LaneWeather::of(entry).label(),
                paths: entry.paths.iter().map(LanePath::as_str).collect(),
                workspace_exists: entry.workspace_exists,
                synced: entry.synced,
                ahead: entry.ahead,
                behind: entry.behind,
                conflicts: entry.conflicts,
                unscoped: &entry.unscoped,
                last_land: entry.last_land.as_deref(),
            })
            .collect(),
    };

    let rendered = if compact {
        serde_json::to_string(&payload)
    } else {
        serde_json::to_string_pretty(&payload)
    };
    rendered.map_err(|error| crate::Error::JsonSerialization(error.to_string()))
}

/// Render the outcomes of `lane sync`.
#[must_use]
pub fn render_lane_sync_outcomes(outcomes: &[LaneSyncOutcome]) -> String {
    if outcomes.is_empty() {
        return String::from("no open lanes to sync\n");
    }

    let mut output = String::new();
    for outcome in outcomes {
        if !outcome.workspace_exists {
            writeln!(output, "· {}  workspace missing (gc?)", outcome.name)
                .expect("write sync row");
            continue;
        }
        let mut facts: Vec<String> = Vec::new();
        if outcome.recovered_stale {
            facts.push(String::from("recovered stale working copy"));
        }
        facts.push(if outcome.rebased {
            String::from("rebased onto trunk head")
        } else {
            String::from("already synced")
        });
        if !outcome.dropped.is_empty() {
            facts.push(format!("dropped {} unscoped path(s)", outcome.dropped.len()));
        }
        let glyph = if outcome.conflicts.is_empty() {
            "\u{2600}"
        } else {
            "\u{26c8}"
        };
        writeln!(output, "{glyph} {}  {}", outcome.name, facts.join(", "))
            .expect("write sync row");
        for conflict in &outcome.conflicts {
            writeln!(
                output,
                "    conflict: change {}  {}",
                style_meta(&conflict.change_id),
                conflict.message,
            )
            .expect("write sync conflict");
        }
        for path in &outcome.dropped {
            writeln!(output, "    dropped: {path}").expect("write sync dropped");
        }
    }
    output
}

/// Render the outcome of `lane land`.
#[must_use]
pub fn render_lane_land_outcome(outcome: &LaneLandOutcome) -> String {
    let mut output = String::new();
    writeln!(
        output,
        "landed lane '{}': {} change(s) fast-forwarded onto trunk",
        outcome.name, outcome.landed_changes,
    )
    .expect("write land");
    writeln!(
        output,
        "  head: change {}  commit {}  {}",
        style_meta(&outcome.landed.change_id),
        style_meta(&outcome.landed.commit_id),
        outcome.landed.message,
    )
    .expect("write land head");
    if let Some(gate) = &outcome.gate {
        writeln!(output, "  gate: passed ({gate})").expect("write land gate");
    }

    if outcome.fanout.is_empty() {
        writeln!(output, "  ripple: no other open lanes").expect("write land ripple");
    } else {
        writeln!(output, "  ripple:").expect("write land ripple");
        for entry in &outcome.fanout {
            if let Some(error) = &entry.error {
                writeln!(output, "    \u{26a0} {}  {error}", entry.name)
                    .expect("write ripple row");
            } else if !entry.rebased {
                writeln!(output, "    \u{2600} {}  already current", entry.name)
                    .expect("write ripple row");
            } else if entry.conflicts > 0 {
                writeln!(
                    output,
                    "    \u{26c8} {}  rebased, {} conflict(s) — theirs to resolve, today not merge-day",
                    entry.name, entry.conflicts,
                )
                .expect("write ripple row");
            } else {
                writeln!(output, "    \u{2600} {}  rebased clean", entry.name)
                    .expect("write ripple row");
            }
        }
    }

    if outcome.closed {
        writeln!(output, "  lane closed and retired").expect("write land closed");
    }
    output
}

/// Render the outcome of `lane abandon`.
#[must_use]
pub fn render_lane_abandon_outcome(outcome: &LaneAbandonOutcome) -> String {
    let mut output = String::new();
    writeln!(output, "abandoned lane '{}'", outcome.name).expect("write abandon");
    if let Some(archive) = &outcome.archive {
        writeln!(output, "  archived diff: {}", archive.display()).expect("write abandon archive");
    }
    if let Some(path) = &outcome.removed_directory {
        writeln!(output, "  removed: {}", path.display()).expect("write abandon dir");
    }
    output
}

/// Render a gc plan, optionally after it was applied.
#[must_use]
pub fn render_lane_gc(plan: &LaneGcPlan, applied: bool) -> String {
    if plan.ghost_workspaces.is_empty() && plan.orphaned_lanes.is_empty() {
        return String::from("nothing to collect: no ghost workspaces, no orphaned lanes\n");
    }

    let mut output = String::new();
    let verb = if applied { "forgot" } else { "would forget" };
    for workspace in &plan.ghost_workspaces {
        writeln!(output, "{verb} ghost workspace '{workspace}' (directory missing)")
            .expect("write gc ghost");
    }
    let verb = if applied { "abandoned" } else { "would abandon" };
    for lane in &plan.orphaned_lanes {
        writeln!(output, "{verb} orphaned lane '{lane}' (no jj workspace)").expect("write gc lane");
    }
    if !applied {
        writeln!(output, "plan only; rerun with --apply").expect("write gc hint");
    }
    output
}

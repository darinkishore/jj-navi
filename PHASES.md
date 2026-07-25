# Overhaul phases — durable spec

Working doc for the reliability/power overhaul on branch `lanes`. Phases 1–2
shipped (commits `cb3691d`, `41775f1`, `b70d330`). This file specs what
remains so work can resume from a cold context.

Standing doctrine (applies to everything below):
- **Engine reads, CLI mutates.** jj-lib (pinned `=0.43.0`, lockstep with the
  system jj via nix) is read-only analysis; every mutation goes through the
  jj CLI so two jj versions can never disagree on working-copy formats.
- **Plan by default, `--apply` to execute, mutation lock held, everything
  `jj op undo`/`jj op restore`-able.**
- **Never rewrite a chain containing a live working copy from outside**
  without snapshotting it first and recovering it after (fan-out doctrine —
  now also applied by `resolve --apply`).
- Human output → stderr; stdout is reserved for machine output (JSON, cd
  directives, switch paths).

## Phase 3 — machine interface (greenlit)

Goal: every navi verb is agent-consumable without parsing English.

1. **`--json` on every mutating verb**: lane open/claim/sync/land/close/
   abandon/gc, heal, resolve, merge, remove. Envelope on stdout:
   `{"ok": true, "command": "...", "result": {...}}`; human stderr
   unchanged. Reuse the outcome structs (`LaneLandOutcome` etc.) with serde.
2. **Stable error codes**: `Error::code() -> &'static str` (kebab-case per
   variant, e.g. `lane-not-synced`, `trunk-moved`, `gate-failed`,
   `mutation-lock-timeout`). Render as `error[<code>]: ...`; with `--json`,
   errors go to stdout as `{"ok": false, "code": ..., "message": ...}`.
3. **Global `-R/--repo <path>`** replacing the hardcoded `PathBuf::from(".")`
   in `cli::try_run`.
4. **New flags/verbs**:
   - `lane open -r <revset>` (stacked lanes; base on given revision).
   - `lane land --gate <cmd>` (override configured gate) and
     `--allow-unscoped` (escape hatch matching open/claim's
     `--allow-overlap`).
   - `lane release -p <path>` (shrink a write-set; refuse emptying it).
   - `lane list --lifecycle open|closed|abandoned|all` filter.
   - `lane gc --prune` (drop closed/abandoned registry records, confirm).
   - `list`/`lane list` `--no-snapshot` (cheap read; skip
     `snapshot_working_copy_at`).
   - `config show` (print effective repo config + path); `ensure_repo_config`
     writes a commented scaffold documenting trunk/gate/sparse/context_paths.
5. **Merge rewrite on `jj duplicate --destination`** (kills the last stderr
   scrape, `parse_duplicated_change_id` in `src/repo/workspace.rs`):
   record `children(target)` before, duplicate `-r <revset> -d target@`,
   new roots = children-diff, head(s) = `heads(descendants(new_roots))`,
   `jj new` onto head(s) (multi-parent ok). Enables multi-root sources.
   Add `merge -r <revset>` and `^`/`@`/`-` alias resolution for
   `merge --from/--into` and `remove` (share one resolver with `switch`).
6. **Heal plan content-diff summary**: per divergent sibling, engine
   tree-diff stats vs the winner (files changed / insertions-ish) so
   newest-op-wins picks are eyeball-verifiable. (Motivated by a constructed
   case where the newest op carried less content.)
7. **Resolve path policies**: `[resolve]` table in repo config
   (`"CHANGELOG.md" = "union"`); `nv resolve --apply` with no args sweeps
   all policies; optionally auto-run during `lane sync`/land gate.
8. Consider: `heal --workspace <ws>` consent flag (heal the live-blocked
   class for one workspace: snapshot → rebase its chain onto winner →
   update-stale); `possession` vs `newest-op` policy flag for the
   loser-is-a-live-@ class.

## Phase 4 — land into a bookmark via integration workspace

Goal: kill the trunk-as-live-workspace constraint (the biggest design
limit found in the audits; wishlist item #1).

- Config: `[lane] target = "main"` (a bookmark) with
  `integration_workspace = "navi-integration"` (auto-created, sparse-empty,
  never a human/agent working copy). `trunk = <workspace>` remains as
  compat fallback; `TrunkContext` generalizes to
  `enum LandTarget { WorkspaceHead(name), Bookmark(name) }`.
- `resolve_trunk` → `resolve_target`: bookmark target = bookmark's commit;
  no parents(@) fragility, read-only commands stop depending on a healthy
  trunk working copy.
- Landing: same pinned-head + gate + lock + re-verify pipeline, but the
  advance is `jj bookmark move <target> --to <head>` run in the integration
  workspace — no live @ is ever fast-forwarded.
- **Law-6 hygiene gate** before advancing: `::head` must contain no
  conflicts (`conflicts() & ::head` empty), no divergent changes, no empty
  descriptions in the new range; refusal with the offending commits listed.
- Fan-out unchanged (peers rebase onto the new bookmark target).
- doctor --deep gains a `::<target>` hygiene section (push-blockers).
- Later (2c, optional): move the landing sequence onto a jj-lib Transaction
  for single-op atomic landings; only after the CLI version has soaked.

## sts_mods cleanup runbook (agreed, not yet executed)

Repo: `~/src/personal/sts_mods` — 667 divergent changes (583 auto-healable),
1073 conflicted commits from 13 roots (10 are CHANGELOG.md; root
`03ebf6b307f5` blast 995), ~237 ghost workspaces, 9 live workspace dirs.
NOTHING applied yet; every step needs Darin's explicit go.

1. Checkpoint: `jj op log -n1` → save op id; `cp -a .jj /tmp/sts_mods-jj-backup`.
2. `nv resolve --union CHANGELOG.md --apply` (now snapshots live workspaces
   first and un-stales after). Re-run `nv conflicts`.
3. Ghost GC: forget the ~237 workspaces with missing dirs
   (`jj workspace forget` each; unblocks most of the 84 heal skips, kills
   the 65 empty+conflicted phantom @s, shrinks the 241 orphan heads).
4. `nv heal --apply --limit 20`, verify, then raise limit and repeat.
5. `nv doctor --deep` re-census; remaining conflicts are the real
   `analysis/seedgen` code conflicts + whatever re-conflicted (hand work).
6. Prevention decision: generated CHANGELOG at land time (gate step) or
   write-set exception — kills the 93% conflict class permanently.

## Backlog (unscheduled)

- Divergence tripwire: every navi op (or a watcher) diffs op-heads and
  warns the moment a fresh divergence is minted.
- Conflict strategies beyond union: ours/theirs per path, lockfile-regen.
- `navi exec` capture-and-retry on stale (currently pre-emptive only).
- fish/nushell shell integration (needs directive-protocol rework).
- Op-churn/attribution: stamp agent identity into op metadata via exec.

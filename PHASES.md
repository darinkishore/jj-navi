# Overhaul phases — durable spec

Working doc for the reliability/power overhaul on branch `lanes`.
**Phases 1–4 are shipped** (commits `cb3691d`, `41775f1`, `b70d330`,
`3499104`, `eb2b703`, `00429e8`, `77e0ee9`, `c5c42c5`, `f767f4c`).
This file keeps the standing doctrine, the concurrency contract, the
sts_mods runbook (not yet executed), and the backlog.

## Standing doctrine

- **Engine reads, CLI mutates.** jj-lib (pinned `=0.43.0`, lockstep with the
  system jj via nix) is read-only analysis; every mutation goes through the
  jj CLI so two jj versions can never disagree on working-copy formats.
- **Plan by default, `--apply` to execute, mutation lock held, everything
  `jj op undo`/`jj op restore`-able.**
- **Never rewrite a chain containing a live working copy from outside**
  without snapshotting it first and recovering it after (fan-out doctrine —
  applied by land fan-out and `resolve --apply`).
- Human output → stderr; stdout is reserved for machine output (JSON
  envelopes, cd directives, switch paths).
- Machine interface: every mutating verb takes `--json` and emits
  `{"ok":true,"command":...,"result":...}` on stdout (errors:
  `{"ok":false,"code":...,"message":...}`); every error has a stable
  kebab-case code rendered as `error[<code>]:`. Codes are API; renaming one
  is a breaking change.

## Concurrency contract (what protects concurrent users)

- **One repo-wide mutation lock** (`navi/mutation.lock`, file lock in shared
  repo storage, so it spans every workspace and process). Held by every
  mutating verb: lane open/claim/release/sync/land/close/abandon/gc,
  heal --apply, resolve --apply (per resolution), exec. Timeout →
  `mutation-lock-timeout`, nothing partially applied.
- **Landings cannot clobber each other.** The head is pinned before the
  gate; the gate runs *outside* the lock (gates take minutes); the lock is
  then taken and the target re-resolved — if it moved (concurrent landing),
  the refusal is `trunk-moved` with zero mutations. The registry is
  reloaded under the lock before `record_land`, so gate-window registry
  changes are never lost.
- **Bookmark mode never touches a live working copy**: the advance is an
  atomic `jj bookmark move` in the sparse-empty integration workspace, so a
  concurrent human/agent editing any workspace cannot be disturbed by a
  landing.
- **Registry writes are atomic** (temp+rename) and always load-modify-save
  under the lock; malformed records quarantine instead of bricking.
- **Rewrites of live chains snapshot first, recover after** (fan-out,
  resolve --apply) — the divergence-minting path is gone (regression-tested).
- **Failures are loud, not silent**: heal/resolve plans computed from an
  engine snapshot re-verify at apply time through jj itself; a commit that
  was concurrently rewritten makes the jj command fail with an error, never
  silently operate on the wrong thing. Descendant sets (e.g. heal's
  rebase-children) are evaluated by jj at execution time under the lock.
- **Interrupt-safe**: a killed navi releases the mutation lock (OS file
  lock on the fd), and every mutation is a jj op — there is no half-done
  command, only clean op boundaries (`jj op log` / `op undo`). Multi-step
  sequences are ordered so interrupted states are visible and repairable:
  registry-before-workspace on open (gc catches orphans),
  archive-before-delete on remove/abandon, pin-before-gate on land
  (killed pre-advance → re-land; post-advance → `lane-nothing-to-land`,
  benign), snapshot-before-rebase in fan-out (stale at worst, never
  divergent; auto-recovered on next touch), self-cleaning scratch
  workspace in the resolve squash path. Residual: stray temp files.
- **Residual, by design**: raw `jj` run outside navi is not serialized by
  navi's lock. jj itself never corrupts under concurrency (op-log merge);
  the worst outcome is divergence, which `doctor --deep` surfaces and
  `navi heal` repairs. Agents should route raw jj through `navi exec`,
  which takes the lock and pre-snapshots.

## Shipped surface (phases 3–4 summary)

- `navi skill`: the agent usage guide ships inside the binary (load once
  per session; works outside any repo). Keep it updated with every
  feature change — it is the contract agents actually read.
- `navi init [--target <bookmark>]`: idempotent repo setup — config
  scaffold (never rewrites an existing config), `.jj/` gitignore in
  colocated repos, bookmark-mode enablement with bookmark auto-create.

- `--json` envelopes on all mutating verbs + conflicts census; stable error
  codes; global `-R/--repo`.
- `lane open -r` (stacked lanes), `lane land --gate <cmd>`/`--allow-unscoped`,
  `lane release -p`, `lane list --lifecycle`/`--no-snapshot`,
  `list --no-snapshot`, `lane gc --prune`, `config show`; the auto-written
  config scaffold documents every knob.
- Merge rewrite: `jj duplicate -r <revset> -d target@` with children-diff
  root discovery (no stderr scraping), multi-root/multi-head sources,
  `merge -r <revset>`, `@`/`-`/`^` aliases on merge/remove.
- Heal plans show per-sibling content diffs vs the winner
  (`changed_paths_vs_keep`); `[resolve]` config table maps files to
  strategies and bare `navi resolve --apply` sweeps all policies.
- **Phase 4**: `[lane] target = "<bookmark>"` lands by advancing the
  bookmark from the auto-created sparse-empty `integration_workspace`
  (default `navi-integration`); no live @ is ever fast-forwarded; the
  law-6 hygiene gate refuses to publish conflicts/divergence below the
  head or undescribed commits in the landed range (checked pre-finalize so
  refusals leave no artifacts, and again under the lock); `doctor --deep`
  reports `::<target>` push-blockers; `trunk = <workspace>` remains the
  legacy fallback.

## Prevention & repair layer (shipped 2026-07-25, post-phase-4)

- `[lane] auto_resolve` (default on): lane sync auto-applies `[resolve]`
  policies when it mints policied conflicts — the changelog class dies at
  birth now.
- Divergence tripwire: mutating verbs compare `divergent()` count to a
  baseline in navi state and warn on increase.
- `navi tidy [--apply --yes]`: gc → policy sweep → guarded heal as one
  idempotent verb (the runbook's mechanical steps).
- `navi abandon -r <revset>`: bulk dead-subtree abandon, guarded against
  working-copy chains and target ancestry; op-undoable, no archive needed.
- Heal guards: empty-shell (with `--prefer-content` override) and
  mainline (never abandon target-ancestry siblings for strays).
- Census triage: `[BLOCKS TARGET]`/`[stranded]` per root, side counts,
  change ids, `-r` scoping.
- Perf: engine commit-prefix resolution moved from full-repo revset
  sweeps to O(log n) index prefix lookups; doctor warns (with compaction
  hint) when the op-log walk hits its cap. resolve --take: vetoed.

## sts_mods cleanup runbook (agreed, not yet executed)

Repo: `~/src/personal/sts_mods` — 667 divergent changes (583 auto-healable),
1073 conflicted commits from 13 roots (10 are CHANGELOG.md; root
`03ebf6b307f5` blast 995), ~237 ghost workspaces, 9 live workspace dirs.
NOTHING applied yet; every step needs Darin's explicit go.

1. Checkpoint: `jj op log -n1` → save op id; `cp -a .jj /tmp/sts_mods-jj-backup`.
2. `nv resolve --union CHANGELOG.md --apply` (snapshots live workspaces
   first and un-stales after). Re-run `nv conflicts`.
   (Or configure `[resolve] "CHANGELOG.md" = "union"` and run
   `nv resolve --apply` — same engine, and it stays configured for later.)
3. Ghost GC: forget the ~237 workspaces with missing dirs
   (`nv lane gc --apply`; unblocks most of the 84 heal skips, kills the 65
   empty+conflicted phantom @s, shrinks the 241 orphan heads).
4. `nv heal --apply --limit 20`, verify (plans now show content diffs vs
   the keep sibling), then raise limit and repeat.
5. `nv doctor --deep` re-census; remaining conflicts are the real
   `analysis/seedgen` code conflicts + whatever re-conflicted (hand work).
6. Prevention decision: generated CHANGELOG at land time (gate step) or
   write-set exception — kills the 93% conflict class permanently.
7. Optional follow-up: create a `main` bookmark and set `[lane]
   target = "main"` to move sts_mods onto bookmark landings.

## Backlog (unscheduled)

- `heal --workspace <ws>` consent flag: heal the live-blocked class for one
  workspace (snapshot → rebase its chain onto winner → update-stale);
  `possession` vs `newest-op` policy flag for the loser-is-a-live-@ class.
- Conflict strategies beyond union: lockfile-regen. (ours/theirs-style
  `resolve --take` was vetoed.)
- CLI setter for `[resolve]` policies (needs comment-preserving TOML
  editing; hand-edit + `config show` for now).
- `navi exec` capture-and-retry on stale (currently pre-emptive only).
- fish/nushell shell integration (needs directive-protocol rework).
- Op-churn/attribution: stamp agent identity into op metadata via exec.
- Later (optional): move the landing sequence onto a jj-lib Transaction for
  single-op atomic landings; only after the CLI version has soaked.

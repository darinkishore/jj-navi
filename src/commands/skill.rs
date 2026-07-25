//! `navi skill`: print the agent-facing usage guide.
//!
//! The guide ships inside the binary so it can never drift from the
//! installed feature set. It is written for agents (and humans) to load
//! into working context once per session.

/// Run `navi skill`: print the guide to stdout.
pub fn run_skill() {
    println!("{}", SKILL.trim_end());
}

const SKILL: &str = r#"# navi — Jujutsu workspace & lane navigator (agent skill)

Load this ONCE per session (`navi skill`); do not re-run it, it is static
per binary version. Everything below is stable interface: error codes and
JSON shapes only change with a breaking release.

## What navi is

navi manages concurrent work on one Jujutsu repo: many workspaces, each
agent working in a *lane* — a workspace plus a declared write-set (path
prefixes it may touch). Lanes stay rebased onto a shared target, land by
fast-forward (never a merge commit, never a landing conflict), and navi
repairs the classic concurrent-jj failure modes (stale working copies,
divergent changes, conflict storms) instead of letting them compound.

Two landing modes, set in repo config (`navi config show`):
- **bookmark mode** (`[lane] target = "main"`, preferred): landing advances
  the bookmark from a hidden integration workspace. No live working copy is
  ever moved; a hygiene gate refuses to publish conflicted, divergent, or
  undescribed history.
- **trunk-workspace mode** (legacy default): landing fast-forwards the
  `trunk` workspace's working copy.

## Golden rules (in order of importance)

1. **Route raw jj through `navi exec -- <jj args>`** (optionally
   `-w <workspace>`). It serializes on navi's repo-wide mutation lock and
   recovers staleness first. Bare `jj` in a busy repo is how divergence
   gets minted. jj never corrupts either way — worst case is divergence,
   which `navi heal` repairs — but don't create the mess.
2. **Mutations are plan-by-default.** `heal`, `resolve`, `lane gc` print a
   plan; add `--apply` to execute. Read the plan first.
3. **Stay inside your write-set.** Landing refuses out-of-scope changes
   (`lane-unscoped-changes`). Widen with `lane claim -p <path>` (checks
   overlap with other lanes) rather than landing with `--allow-unscoped`.
4. **Sync early, sync often** (`navi lane sync <lane>`): conflicts surface
   small, early, and in *your* lane instead of at landing time.
5. **Never amend work that already landed** — make a follow-up change.
   Amending landed changes smears conflicts into everyone's descendants
   (doctor flags this as merged-then-amended).
6. **Everything navi does is jj-native and undoable**: `jj op log` to see
   what happened, `jj op undo` / `jj op restore <op>` to unwind.

## Machine interface

- Add `--json` to any mutating verb (and `conflicts`, `doctor`, `list`,
  `lane list`, `config show`). Success → stdout envelope
  `{"ok":true,"command":"lane land","result":{...}}`. Failure → exit != 0
  and `{"ok":false,"code":"<kebab-code>","message":"..."}` on stdout.
  Human commentary always goes to stderr; parse stdout only.
- Errors render as `error[<code>]:` on stderr; the codes are stable — key
  on them, not on message text.
- Global `-R/--repo <path>` targets a repo without cd-ing into it.
- Concurrency: every mutating verb holds a repo-wide lock. A concurrent
  landing cannot clobber yours (loser gets `trunk-moved`, nothing applied).
  On `mutation-lock-timeout`, wait and retry (or raise NAVI_LOCK_TIMEOUT_MS).

## Session start checklist

1. `navi skill` (once — you are reading it).
2. `navi config show --json` — landing mode, gate, resolve policies. In a
   brand-new repo, `navi init [--target main]` first: writes the config
   scaffold, gitignores `.jj/` in colocated repos, and (with `--target`)
   turns on bookmark landings, creating the bookmark if needed.
3. `navi doctor --json` — health; add `--deep` if anything smells off
   (divergence, conflict, orphan-head, target-hygiene census).
4. `navi lane list --json` — who is working where; pick a free scope.

## Lanes — the core workflow

```
navi lane open feat-auth -p src/auth -p tests/auth   # declare scope, get a workspace
cd <printed path>                                    # work normally; jj or navi exec
navi lane sync feat-auth                             # rebase onto target (repeat often)
navi lane land feat-auth -m "auth: add tokens" --close --json
```

- `lane open NAME -p PATH...` — flags: `--sparse`/`--full` (materialize
  only the write-set + context paths, or everything), `-r <revset>`
  (stacked lane: base on an unlanded chain instead of the target head),
  `--allow-overlap` (share paths with another lane, coordinate yourselves).
- `lane claim NAME -p PATH...` / `lane release NAME -p PATH...` — widen /
  shrink the write-set (release refuses to empty it).
- `lane sync [NAME] [--drop-unscoped]` — all open lanes when NAME omitted;
  `--drop-unscoped` restores out-of-scope files from the target head.
- `lane land NAME` — refusal-checked (synced? conflict-free? in scope?
  described?), then gate, then atomic advance, then automatic *fan-out*:
  every peer lane is rebased onto the new head (snapshot-first, no
  divergence). Flags: `-m <msg>` (describe the head), `--close` (retire
  after landing; not from inside the lane), `--no-gate`, `--gate <cmd>`
  (one-shot override), `--allow-unscoped` (escape hatch).
- `lane list` — weather per lane: ☀ clear ⛅ behind ⛈ conflicts 🌫 scope
  drift · void. Flags: `--json`, `--lifecycle open|closed|abandoned|all`,
  `--no-snapshot` (fast read, slightly stale).
- `lane close NAME` (fully landed only) / `lane abandon NAME` (archives
  the diff first) / `lane gc [--prune] --apply` (ghost workspaces,
  orphaned records; `--prune` drops retired records).

## Repair toolkit (when the repo is sick)

- `navi doctor --deep --json` — full census: divergent changes, conflicted
  commits, orphan heads, op churn, merged-then-amended landings, target
  push-blockers.
- `navi heal [--apply]` — divergence healer: newest-op-wins per change,
  stale siblings abandoned, stacked descendants rebased onto the winner.
  The plan shows each loser's content diff vs the keeper ("identical tree
  to keep" = abandoning loses nothing). Skips anything carrying a live
  working copy (one-writer law). Flags: `--change <prefix>` (repeatable),
  `--mine`, `--limit N`.
- `navi conflicts --json` — conflict *roots* ranked by blast radius; fix
  roots, descendants re-merge automatically.
- `navi resolve --union <FILE> [--apply]` — structurally union-merge an
  append-only file (changelogs) at every root, looping to fixpoint. A line
  survives once if any side has it (deduped across sides — rebase echoes
  duplicate entries); works for any number of conflict sides. Expect the
  conflict count to *rise* mid-run as descendants re-merge, then collapse:
  that is the fixpoint loop working, not damage. With a `[resolve]` policy
  table configured, bare `navi resolve --apply` sweeps every policy.

## Workspace basics (outside the lane workflow)

- `navi switch NAME` (alias `cd`) — aliases: `^` primary, `@` current,
  `-` previous. `-c` creates, `-r <revset>` sets the base (create only).
- `navi list --json [--no-snapshot]` — all workspaces with health/paths.
- `navi merge -f <ws> [-i <ws>]` or `navi merge -r <revset>` — duplicate
  work into a target workspace (source never rewritten; multi-root OK;
  aliases work).
- `navi remove NAME` (alias `rm`) — archives the working-copy diff before
  deleting; `-y` skips the prompt.
- `navi exec [-w ws] -- <jj args>` — locked, staleness-recovering raw jj.

## Config (`.jj/repo/navi/config.toml`, see `navi config show --json`)

```toml
workspace_template = "../{repo}.{workspace}"
[lane]
target = "main"                      # bookmark mode (preferred)
integration_workspace = "navi-integration"
# trunk = "default"                  # legacy fallback when target unset
gate = "cargo test"                  # runs via sh -c before every landing
sparse = false
context_paths = []
[resolve]
"CHANGELOG.md" = "union"
```

## Error code → recovery (the ones you will actually see)

| code                    | meaning → action                                          |
|-------------------------|-----------------------------------------------------------|
| lane-not-synced         | behind target → `lane sync <lane>`, then retry            |
| lane-conflicted         | conflicts in lane chain → resolve in lane (try `navi resolve`), retry |
| lane-unscoped-changes   | touched paths outside write-set → `lane claim -p`, or `sync --drop-unscoped` |
| lane-needs-message      | head undescribed → retry with `-m "..."`                  |
| trunk-moved             | someone landed during your gate → `lane sync`, land again |
| gate-failed             | your work failed the gate → fix and retry (`--no-gate` only if the gate itself is broken) |
| target-hygiene          | conflicts/divergence/undescribed below head → `navi resolve` / `navi heal` / `jj describe`, retry |
| target-bookmark-missing | create it: `jj bookmark create <name> -r <rev>`           |
| mutation-lock-timeout   | another navi op running → wait, retry                     |
| lane-overlap            | scope owned by another lane → coordinate, pick other paths, or `--allow-overlap` |
| lane-nothing-to-land    | no non-empty changes → nothing to do                      |
| stale working copies    | not an error: navi auto-recovers them (snapshot → update-stale) |

Anything else: read the message (stderr) — every error carries a hint line.
"#;

# jj-navi

<img width="788" height="640" alt="jj-navi" src="https://github.com/user-attachments/assets/88e8b46e-9a76-416b-9f76-b4480d6964e7" />

Workspace management for [Jujutsu](https://jj-vcs.github.io/jj/latest/), built for parallel human and AI agent workflows.

## The problem

jj workspaces are great for parallel work, but the workflow around it is quite cumbersome:

- **Paths are unmanaged.** `jj workspace add ../name` works, but paths are arbitrary and easy to forget.
- **Cross-workspace visibility is stale.** jj snapshots the current workspace when you run a command, but not the others. So `jj log` from one workspace can show outdated commits for the rest — files on disk exist, but jj hasn't recorded them yet.
- **Cleanup is awkward.** Forgetting a workspace does not delete its directory, and deleting a directory does not forget the workspace. There is also no guard against removing the one you are currently in.
- **Switching doesn't switch your shell.** `jj workspace` changes the working copy, not your terminal's current directory.

## What `jj-navi` does

`jj-navi` manages workspace lifecycle: naming, paths, switching, visibility, and cleanup.

- **`switch --create`** — go to a workspace, creating it at a deterministic path if it doesn't exist
- **`list`** — snapshot each workspace and show path health, diff stats, commit info, and age
- **`merge`** — merge work from another workspace into the current or named workspace
- **`remove`** — forget a workspace and delete its local directory; refuses current workspace

With shell integration installed, `navi switch` also changes your current directory.

```text
repo/
├── repo                 current workspace
├── repo.feature-auth    navi switch --create feature-auth
└── repo.fix-api         navi switch --create fix-api
```

## Before and after

**Without `jj-navi`**

```sh
jj workspace add ../repo.feature-auth
cd ../repo.feature-auth
# ... do work ...
cd ../repo
jj log                          # stale view of other workspaces
jj workspace list               # names only
jj workspace forget feature-auth
rm -rf ../repo.feature-auth     # directory left behind
```

**With `jj-navi`**

```sh
navi switch --create feature-auth
# ... do work ...
navi switch -
navi list                       # snapshotted, with diff stats and age
navi remove feature-auth        # asks before deleting the workspace directory
```

## Install

```sh
# npm
npm install -g jj-navi

# cargo
cargo install jj-navi --version 0.2.3
```

Binaries: `navi`, `nv`

Minimum `jj`: `0.39.0`  
Minimum Node.js (tested): `24`

## Shell integration

Install once so `navi switch` can update your shell's current directory:

```sh
navi config shell install --shell zsh
source ~/.zshrc
```

Supports `bash` and `zsh`. This adds a managed block to your shell rc file.

## Quick start

```sh
navi doctor
navi switch --create feature-auth
navi list
navi switch -
navi remove feature-auth
```

## Commands

```sh
navi switch <workspace>          # switch to a workspace
navi cd <workspace>              # alias for switch
navi switch ^                    # switch to the primary workspace
navi switch -                    # switch to previous workspace
navi switch @                    # switch to current workspace explicitly
navi switch --create <workspace> # create and switch
navi switch -c <workspace>
navi switch --create <workspace> --revision <revset> # create from a revision
navi switch -c <workspace> -r <revset>

navi list                        # human-readable workspace inventory
navi ls                          # alias for list
navi list --json
navi list -j
navi list --json --compact
navi list -j -c

navi doctor [--json] [--compact] # diagnose repo, workspace, and shell state
navi doctor [-j] [-c]

navi merge --from <workspace>     # merge a workspace into the current workspace
navi merge -f <workspace>
navi merge --from <workspace> --into <workspace>
navi merge -f <workspace> -i <workspace>

navi remove <workspace>          # forget a workspace and delete its directory
navi rm <workspace>              # alias for remove
navi remove <workspace> --yes    # skip destructive confirmation
navi remove <workspace> -y

navi config shell init <bash|zsh>
navi config shell install [--shell <bash|zsh>]
navi config shell install [-s <bash|zsh>]

navi lane open <name> --path <prefix> [...]  # declare a write-set, open a lane on the trunk head
navi lane claim <name> --path <prefix>       # extend an open lane's write-set
navi lane list [--json]                      # lane weather: ☀ synced · ⛅ behind · ⛈ conflicted · 🌫 scope drift
navi lane sync [<name>] [--drop-unscoped]    # rebase lanes onto the trunk head
navi lane land <name> [-m <msg>] [--close]   # gate, fast-forward trunk, ripple the new head to peers
navi lane close <name>                       # retire a fully landed lane
navi lane abandon <name> [--yes]             # archive the diff, then discard the lane
navi lane gc [--apply] [--yes]               # collect ghost workspaces and orphaned lanes
```

## Lanes: concurrent work without merge-day

Lanes are for repos where several agents (or humans) work the same trunk
concurrently. Each lane is a jj workspace plus a **declared write-set** —
the path prefixes it intends to touch — registered in repo storage.
Everything else follows from two rules:

1. **Divergence age is the enemy, not landing size.** Every landing rebases
   every other open lane onto the new head (the *ripple*), so conflicts
   surface in the owning lane within minutes as small, attributed diffs —
   never at integration time as a 40-file surprise. A lane can cook for a
   week and still land clean, because it ate its conflicts incrementally.
2. **Landing is a fast-forward, not a merge.** `lane land` requires the lane
   to be synced onto the trunk head, checks the diff stays inside the
   write-set, refuses if trunk's working copy is dirty *inside that
   write-set* (unrelated trunk dirt does not block), runs the configured
   gate command, then advances trunk onto the lane's chain. No merge
   commits; a landing that would conflict is structurally impossible.

Write-set overlap between open lanes is refused at `lane open` — the moment
coordination is cheapest — unless you opt in with `--allow-overlap`.
Out-of-scope changes show up as 🌫 in `lane list`, block landing with the
offending paths named, and can be mechanically dropped with
`lane sync --drop-unscoped`.

Repo-level configuration lives next to navi's other state (shared jj repo
storage, `navi/config.toml`):

```toml
workspace_template = "../{repo}.{workspace}"

[lane]
trunk = "default"          # workspace whose @-parent is the trunk head
gate = "cargo test"        # run in the lane before every landing (sh -c)
sparse = false             # open lanes as sparse workspaces by default
context_paths = ["AGENTS.md"]  # extra read-only paths for sparse lanes
```

Lifecycle is total: a lane ends `closed` (landed and retired) or
`abandoned` (diff archived under `navi/archive/`, then discarded) — and
`lane gc` sweeps up ghost workspaces whose directories vanished, so the
workspace list stays legible.

## How it works

Config and metadata live inside shared Jujutsu storage:

```text
.jj/repo/navi/config.toml
.jj/repo/navi/workspaces.toml
```

Default workspace path template: `../{repo}.{workspace}`

## Notes

- `switch` can recover from missing jj workspace-path records when it can validate a fallback path
- `switch` warns when it falls back to template-based path resolution
- `list` snapshots healthy workspaces before rendering so parallel changes are visible
- `list` reports missing, stale, or not-current workspaces instead of hiding them
- `list --json` exposes structured `freshness`, `diff`, and `age` fields
- `remove` forgets a workspace and deletes its directory after confirmation; `--yes` skips the prompt
- Supported shells: `bash`, `zsh`

## Special thanks

Inspired by:

- [Worktrunk](https://github.com/max-sixty/worktrunk) — Git worktree management for parallel AI agent workflows
- [jj-ryu](https://github.com/dmmulroy/jj-ryu) — Stacked PRs for Jujutsu

## Art credits

- [BoTW Link Pixel Art](https://www.reddit.com/r/zelda/comments/piy10r/botw_oc_hero_of_the_wild_pixel_art/)

## License

[MIT](https://github.com/eersnington/jj-navi/blob/main/LICENSE)

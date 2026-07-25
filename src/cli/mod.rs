use std::ffi::OsString;
use std::path::PathBuf;
use std::process::ExitCode;

use clap::{CommandFactory, FromArgMatches, Parser, Subcommand};

use crate::commands;
use crate::completion;
use crate::output::{render_domain_error, render_error_message, render_json_error};
use crate::types::ShellKind;

#[derive(Parser)]
#[command(about = "Workspace navigator for Jujutsu")]
#[command(arg_required_else_help = true)]
#[command(version)]
struct Cli {
    #[arg(
        long = "repo",
        short = 'R',
        global = true,
        value_name = "PATH",
        help = "Operate on the jj repo containing this path instead of the current directory"
    )]
    repo: Option<PathBuf>,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    #[command(
        about = "Switch to an existing workspace, '^' for primary, '@' for current, '-' for previous, or create one with --create",
        visible_alias = "cd"
    )]
    Switch {
        #[arg(long, short = 'c', help = "Create the workspace if it does not exist")]
        create: bool,

        #[arg(
            long,
            short = 'r',
            help = "Revision to base a newly created workspace on"
        )]
        revision: Option<String>,

        #[arg(
            help = "Workspace name, '^' for primary, '@' for current, or '-' for previous",
            add = completion::workspace_value_completer()
        )]
        workspace: String,
    },
    #[command(
        about = "List known workspaces with path and commit details",
        visible_alias = "ls"
    )]
    List {
        #[arg(long, short = 'j', help = "Render workspaces as JSON")]
        json: bool,

        #[arg(long, short = 'c', help = "Render compact JSON", requires = "json")]
        compact: bool,

        #[arg(
            long,
            help = "Cheap read: skip snapshotting each workspace's working copy first"
        )]
        no_snapshot: bool,
    },
    #[command(about = "Inspect repo, workspace, and shell health")]
    Doctor {
        #[arg(long, short = 'j', help = "Render diagnostics as JSON")]
        json: bool,

        #[arg(long, short = 'c', help = "Render compact JSON", requires = "json")]
        compact: bool,

        #[arg(
            long,
            help = "Add repo hygiene: divergence, conflicts, orphan heads, op churn, merged-then-amended"
        )]
        deep: bool,
    },
    #[command(
        about = "Heal divergent changes: newest-op wins, stale siblings abandoned (plan by default)"
    )]
    Heal {
        #[arg(
            long = "change",
            help = "Only heal changes whose id starts with this prefix (repeatable)"
        )]
        changes: Vec<String>,

        #[arg(long, help = "Only heal changes minted entirely by my operations")]
        mine: bool,

        #[arg(long, help = "Apply the plan instead of printing it")]
        apply: bool,

        #[arg(long, default_value_t = 100, help = "Maximum changes healed per run")]
        limit: usize,

        #[arg(long, short = 'j', help = "Emit a machine envelope on stdout")]
        json: bool,
    },
    #[command(about = "Census conflict roots: where conflicts begin, ranked by blast radius")]
    Conflicts {
        #[arg(long, short = 'j', help = "Emit a machine envelope on stdout")]
        json: bool,
    },
    #[command(
        about = "Auto-resolve conflicts structurally; --union keeps both sides of an append-only file"
    )]
    Resolve {
        #[arg(
            long,
            value_name = "FILE",
            help = "Union-merge this repo-relative file at every conflict root; omit to sweep configured [resolve] policies"
        )]
        union: Option<String>,

        #[arg(long, help = "Apply the resolutions instead of printing the plan")]
        apply: bool,

        #[arg(long, short = 'j', help = "Emit a machine envelope on stdout")]
        json: bool,
    },
    #[command(about = "Run a raw jj command under navi's umbrella (un-stale first, serialized)")]
    Exec {
        #[arg(
            long,
            short = 'w',
            help = "Workspace to run in; defaults to the current workspace",
            add = completion::workspace_value_completer()
        )]
        workspace: Option<String>,

        #[arg(
            trailing_var_arg = true,
            allow_hyphen_values = true,
            help = "jj arguments, e.g. navi exec -- status"
        )]
        args: Vec<OsString>,
    },
    #[command(
        about = "Forget a non-current workspace and delete its directory",
        visible_alias = "rm"
    )]
    Remove {
        #[arg(long, short = 'y', help = "Skip destructive confirmation")]
        yes: bool,

        #[arg(long, short = 'j', help = "Emit a machine envelope on stdout")]
        json: bool,

        #[arg(help = "Workspace name to remove", add = completion::workspace_value_completer())]
        workspace: String,
    },
    #[command(about = "Merge work from another JJ workspace or an explicit revset")]
    Merge {
        #[arg(
            long,
            short = 'f',
            required_unless_present = "revisions",
            help = "Source workspace to merge from ('@', '-', '^' aliases work)",
            add = completion::workspace_value_completer()
        )]
        from: Option<String>,

        #[arg(
            long,
            short = 'i',
            help = "Target workspace to merge into; defaults to current ('@', '-', '^' aliases work)",
            add = completion::workspace_value_completer()
        )]
        into: Option<String>,

        #[arg(
            long = "revisions",
            short = 'r',
            value_name = "REVSET",
            help = "Merge exactly this revset instead of a source workspace's work"
        )]
        revisions: Option<String>,

        #[arg(long, short = 'j', help = "Emit a machine envelope on stdout")]
        json: bool,
    },
    #[command(
        about = "Print the agent usage guide for navi (load once per session)"
    )]
    Skill,
    #[command(
        about = "Initialize navi in this repo: config scaffold, .jj gitignore, optional bookmark target"
    )]
    Init {
        #[arg(
            long,
            value_name = "BOOKMARK",
            help = "Enable bookmark landings into this bookmark (created at @- if missing)"
        )]
        target: Option<String>,

        #[arg(long, short = 'j', help = "Emit a machine envelope on stdout")]
        json: bool,
    },
    #[command(about = "Shell integration and future config commands")]
    #[command(arg_required_else_help = true)]
    Config {
        #[command(subcommand)]
        command: ConfigCommands,
    },
    #[command(
        about = "Concurrent-work lanes: declared write-sets, fast-forward landings, fan-out sync"
    )]
    #[command(arg_required_else_help = true)]
    Lane {
        #[command(subcommand)]
        command: LaneCommands,
    },
}

#[derive(Subcommand)]
enum LaneCommands {
    #[command(about = "Open a lane: declare a write-set and create its workspace on the trunk head")]
    Open {
        #[arg(help = "Lane name")]
        name: String,

        #[arg(
            long = "path",
            short = 'p',
            required = true,
            help = "Repo-relative write-set path prefix (repeatable)"
        )]
        paths: Vec<String>,

        #[arg(long, help = "Allow write-set overlap with another open lane")]
        allow_overlap: bool,

        #[arg(long, help = "Create the workspace sparse (write-set + context paths only)", overrides_with = "full")]
        sparse: bool,

        #[arg(long, help = "Create the workspace with the full tree", overrides_with = "sparse")]
        full: bool,

        #[arg(
            long,
            short = 'r',
            value_name = "REVSET",
            help = "Base the lane on this revision instead of the trunk head (stacked lanes)"
        )]
        revision: Option<String>,

        #[arg(long, short = 'j', help = "Emit a machine envelope on stdout")]
        json: bool,
    },
    #[command(about = "Extend an open lane's write-set")]
    Claim {
        #[arg(help = "Lane name", add = completion::workspace_value_completer())]
        name: String,

        #[arg(
            long = "path",
            short = 'p',
            required = true,
            help = "Repo-relative write-set path prefix (repeatable)"
        )]
        paths: Vec<String>,

        #[arg(long, help = "Allow write-set overlap with another open lane")]
        allow_overlap: bool,

        #[arg(long, short = 'j', help = "Emit a machine envelope on stdout")]
        json: bool,
    },
    #[command(about = "Shrink an open lane's write-set")]
    Release {
        #[arg(help = "Lane name", add = completion::workspace_value_completer())]
        name: String,

        #[arg(
            long = "path",
            short = 'p',
            required = true,
            help = "Write-set path prefix to release (repeatable)"
        )]
        paths: Vec<String>,

        #[arg(long, short = 'j', help = "Emit a machine envelope on stdout")]
        json: bool,
    },
    #[command(about = "List lanes with live weather: sync, drift, conflicts, scope", visible_alias = "ls")]
    List {
        #[arg(long, short = 'j', help = "Render lanes as JSON")]
        json: bool,

        #[arg(long, short = 'c', help = "Render compact JSON", requires = "json")]
        compact: bool,

        #[arg(
            long,
            value_name = "STATE",
            help = "Only show lanes in this lifecycle: open, closed, abandoned, or all"
        )]
        lifecycle: Option<String>,

        #[arg(
            long,
            help = "Cheap read: skip snapshotting each lane's working copy first"
        )]
        no_snapshot: bool,
    },
    #[command(about = "Rebase lanes onto the current trunk head (all open lanes by default)")]
    Sync {
        #[arg(help = "Lane name; omit to sync every open lane", add = completion::workspace_value_completer())]
        name: Option<String>,

        #[arg(long, help = "Restore out-of-scope paths from the trunk head")]
        drop_unscoped: bool,

        #[arg(long, short = 'j', help = "Emit a machine envelope on stdout")]
        json: bool,
    },
    #[command(about = "Land a lane: gate, fast-forward trunk, ripple the new head to peers")]
    Land {
        #[arg(help = "Lane name", add = completion::workspace_value_completer())]
        name: String,

        #[arg(long, short = 'm', help = "Description for the landed head if it has none")]
        message: Option<String>,

        #[arg(long, help = "Skip the configured gate command")]
        no_gate: bool,

        #[arg(
            long,
            value_name = "CMD",
            conflicts_with = "no_gate",
            help = "Run this gate command instead of the configured one"
        )]
        gate: Option<String>,

        #[arg(
            long,
            help = "Land even if the lane changed paths outside its write-set"
        )]
        allow_unscoped: bool,

        #[arg(long, help = "Close and remove the lane after landing")]
        close: bool,

        #[arg(long, short = 'j', help = "Emit a machine envelope on stdout")]
        json: bool,
    },
    #[command(about = "Close a fully landed lane and remove its workspace")]
    Close {
        #[arg(help = "Lane name", add = completion::workspace_value_completer())]
        name: String,

        #[arg(long, short = 'j', help = "Emit a machine envelope on stdout")]
        json: bool,
    },
    #[command(about = "Abandon a lane: archive its diff, then remove workspace and registration")]
    Abandon {
        #[arg(help = "Lane name", add = completion::workspace_value_completer())]
        name: String,

        #[arg(long, short = 'y', help = "Skip destructive confirmation")]
        yes: bool,

        #[arg(long, short = 'j', help = "Emit a machine envelope on stdout")]
        json: bool,
    },
    #[command(about = "Collect ghost workspaces (directory gone) and orphaned lane records")]
    Gc {
        #[arg(long, help = "Apply the plan instead of printing it")]
        apply: bool,

        #[arg(
            long,
            help = "Also drop closed and abandoned lane records from the registry"
        )]
        prune: bool,

        #[arg(long, short = 'y', help = "Skip confirmation when applying")]
        yes: bool,

        #[arg(long, short = 'j', help = "Emit a machine envelope on stdout")]
        json: bool,
    },
}

#[derive(Subcommand)]
enum ConfigCommands {
    #[command(about = "Print the effective repo config and where it lives")]
    Show {
        #[arg(long, short = 'j', help = "Emit a machine envelope on stdout")]
        json: bool,
    },
    #[command(about = "Shell integration commands")]
    #[command(arg_required_else_help = true)]
    Shell {
        #[command(subcommand)]
        command: ShellCommands,
    },
}

#[derive(Subcommand)]
enum ShellCommands {
    #[command(about = "Print shell integration script for a supported shell")]
    Init {
        #[arg(value_name = "SHELL", help = "Supported shell", value_enum)]
        shell: Option<ShellKind>,
    },
    #[command(about = "Install the managed shell integration block into your rc file")]
    Install {
        #[arg(
            long,
            short = 's',
            help = "Shell to install for; defaults to $SHELL",
            value_enum
        )]
        shell: Option<ShellKind>,
    },
}

enum CliError {
    Clap(clap::Error),
    Domain(crate::Error),
}

impl From<clap::Error> for CliError {
    fn from(value: clap::Error) -> Self {
        Self::Clap(value)
    }
}

impl From<crate::Error> for CliError {
    fn from(value: crate::Error) -> Self {
        Self::Domain(value)
    }
}

/// Run the CLI binary entrypoint with the provided binary name and argv.
#[must_use]
pub fn run(bin_name: &'static str, args: impl IntoIterator<Item = OsString>) -> ExitCode {
    if completion::maybe_handle_env_completion(bin_name) {
        return ExitCode::SUCCESS;
    }

    match try_run(bin_name, args) {
        Ok(exit_code) => exit_code,
        Err(CliError::Clap(error)) => {
            let exit_code = error.exit_code();
            if error.print().is_err() {
                eprintln!("{}", render_error_message(&error.to_string()));
            }
            ExitCode::from(u8::try_from(exit_code).unwrap_or(1))
        }
        Err(CliError::Domain(error)) => {
            eprintln!("{}", render_domain_error(&error));
            ExitCode::FAILURE
        }
    }
}

fn try_run(
    bin_name: &'static str,
    args: impl IntoIterator<Item = OsString>,
) -> Result<ExitCode, CliError> {
    let cli = parse(bin_name, args)?;
    let path = cli.repo.clone().unwrap_or_else(|| PathBuf::from("."));
    let machine = machine_context(&cli.command);

    match dispatch(cli.command, &path, bin_name) {
        Ok(exit_code) => Ok(exit_code),
        Err(error) => {
            // With --json, errors are part of the machine interface: the
            // envelope goes to stdout, the human rendering to stderr.
            if let Some(command) = machine {
                println!("{}", render_json_error(command, &error));
                eprintln!("{}", render_domain_error(&error));
                return Ok(ExitCode::FAILURE);
            }
            Err(CliError::Domain(error))
        }
    }
}

/// The command label for the machine envelope when `--json` was requested.
fn machine_context(command: &Commands) -> Option<&'static str> {
    match command {
        Commands::Heal { json: true, .. } => Some("heal"),
        Commands::Conflicts { json: true } => Some("conflicts"),
        Commands::Resolve { json: true, .. } => Some("resolve"),
        Commands::Remove { json: true, .. } => Some("remove"),
        Commands::Merge { json: true, .. } => Some("merge"),
        Commands::Init { json: true, .. } => Some("init"),
        Commands::Lane { command } => match command {
            LaneCommands::Open { json: true, .. } => Some("lane open"),
            LaneCommands::Claim { json: true, .. } => Some("lane claim"),
            LaneCommands::Release { json: true, .. } => Some("lane release"),
            LaneCommands::Sync { json: true, .. } => Some("lane sync"),
            LaneCommands::Land { json: true, .. } => Some("lane land"),
            LaneCommands::Close { json: true, .. } => Some("lane close"),
            LaneCommands::Abandon { json: true, .. } => Some("lane abandon"),
            LaneCommands::Gc { json: true, .. } => Some("lane gc"),
            _ => None,
        },
        _ => None,
    }
}

#[allow(clippy::too_many_lines)] // flat subcommand dispatcher
fn dispatch(
    command: Commands,
    path: &std::path::Path,
    bin_name: &'static str,
) -> crate::Result<ExitCode> {
    match command {
        Commands::Switch {
            create,
            revision,
            workspace,
        } => commands::switch::run_switch(path, &workspace, create, revision.as_deref())?,
        Commands::List {
            json,
            compact,
            no_snapshot,
        } => commands::list::run_list(path, json, compact, no_snapshot)?,
        Commands::Doctor {
            json,
            compact,
            deep,
        } => {
            return commands::doctor::run_doctor(path, bin_name, json, compact, deep);
        }
        Commands::Heal {
            changes,
            mine,
            apply,
            limit,
            json,
        } => {
            let limit = commands::heal::validated_limit(limit)?;
            commands::heal::run_heal(
                path,
                &commands::heal::HealOptions {
                    changes: &changes,
                    mine,
                    apply,
                    limit,
                    json,
                },
            )?;
        }
        Commands::Conflicts { json } => commands::resolve::run_conflicts(path, json)?,
        Commands::Resolve { union, apply, json } => match union {
            Some(union) => commands::resolve::run_resolve_union(path, &union, apply, json)?,
            None => commands::resolve::run_resolve_policies(path, apply, json)?,
        },
        Commands::Exec { workspace, args } => {
            return commands::exec::run_exec(path, workspace.as_deref(), &args);
        }
        Commands::Remove {
            yes,
            json,
            workspace,
        } => {
            commands::remove::run_remove(path, &workspace, yes, json)?;
        }
        Commands::Merge {
            from,
            into,
            revisions,
            json,
        } => {
            commands::merge::run_merge(
                path,
                from.as_deref(),
                into.as_deref(),
                revisions.as_deref(),
                json,
            )?;
        }
        Commands::Lane { command } => match command {
            LaneCommands::Open {
                name,
                paths,
                allow_overlap,
                sparse,
                full,
                revision,
                json,
            } => {
                let sparse = if sparse {
                    Some(true)
                } else if full {
                    Some(false)
                } else {
                    None
                };
                commands::lane::run_lane_open(
                    path,
                    &name,
                    &paths,
                    allow_overlap,
                    sparse,
                    revision.as_deref(),
                    json,
                )?;
            }
            LaneCommands::Claim {
                name,
                paths,
                allow_overlap,
                json,
            } => commands::lane::run_lane_claim(path, &name, &paths, allow_overlap, json)?,
            LaneCommands::Release { name, paths, json } => {
                commands::lane::run_lane_release(path, &name, &paths, json)?;
            }
            LaneCommands::List {
                json,
                compact,
                lifecycle,
                no_snapshot,
            } => {
                commands::lane::run_lane_list(
                    path,
                    json,
                    compact,
                    lifecycle.as_deref(),
                    no_snapshot,
                )?;
            }
            LaneCommands::Sync {
                name,
                drop_unscoped,
                json,
            } => commands::lane::run_lane_sync(path, name.as_deref(), drop_unscoped, json)?,
            LaneCommands::Land {
                name,
                message,
                no_gate,
                gate,
                allow_unscoped,
                close,
                json,
            } => commands::lane::run_lane_land(
                path,
                &name,
                &commands::lane::LandFlags {
                    message: message.as_deref(),
                    no_gate,
                    gate_override: gate.as_deref(),
                    allow_unscoped,
                    close,
                },
                json,
            )?,
            LaneCommands::Close { name, json } => commands::lane::run_lane_close(path, &name, json)?,
            LaneCommands::Abandon { name, yes, json } => {
                commands::lane::run_lane_abandon(path, &name, yes, json)?;
            }
            LaneCommands::Gc {
                apply,
                prune,
                yes,
                json,
            } => {
                commands::lane::run_lane_gc(path, apply, prune, yes, json)?;
            }
        },
        Commands::Skill => commands::skill::run_skill(),
        Commands::Init { target, json } => {
            commands::init::run_init(path, target.as_deref(), json)?;
        }
        Commands::Config { command } => match command {
            ConfigCommands::Show { json } => commands::config_show::run_config_show(path, json)?,
            ConfigCommands::Shell { command } => match command {
                ShellCommands::Init { shell } => {
                    commands::config_shell::run_shell_init(bin_name, shell)?;
                }
                ShellCommands::Install { shell } => {
                    commands::config_shell::run_shell_install(bin_name, shell)?;
                }
            },
        },
    }

    Ok(ExitCode::SUCCESS)
}

fn parse(
    bin_name: &'static str,
    args: impl IntoIterator<Item = OsString>,
) -> Result<Cli, clap::Error> {
    let mut command = build_command();
    command = command.name(bin_name);
    let matches = command.try_get_matches_from(args)?;
    Cli::from_arg_matches(&matches)
}

pub(crate) fn build_command() -> clap::Command {
    Cli::command().styles(crate::output::clap_styles())
}

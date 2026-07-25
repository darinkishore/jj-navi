use std::ffi::OsString;
use std::path::PathBuf;
use std::process::ExitCode;

use clap::{CommandFactory, FromArgMatches, Parser, Subcommand};

use crate::commands;
use crate::completion;
use crate::output::render_error_message;
use crate::types::ShellKind;

#[derive(Parser)]
#[command(about = "Workspace navigator for Jujutsu")]
#[command(arg_required_else_help = true)]
#[command(version)]
struct Cli {
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

        #[arg(help = "Workspace name to remove", add = completion::workspace_value_completer())]
        workspace: String,
    },
    #[command(about = "Merge work from another JJ workspace")]
    Merge {
        #[arg(
            long,
            short = 'f',
            help = "Source workspace to merge from",
            add = completion::workspace_value_completer()
        )]
        from: String,

        #[arg(
            long,
            short = 'i',
            help = "Target workspace to merge into; defaults to current",
            add = completion::workspace_value_completer()
        )]
        into: Option<String>,
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
    },
    #[command(about = "List lanes with live weather: sync, drift, conflicts, scope", visible_alias = "ls")]
    List {
        #[arg(long, short = 'j', help = "Render lanes as JSON")]
        json: bool,

        #[arg(long, short = 'c', help = "Render compact JSON", requires = "json")]
        compact: bool,
    },
    #[command(about = "Rebase lanes onto the current trunk head (all open lanes by default)")]
    Sync {
        #[arg(help = "Lane name; omit to sync every open lane", add = completion::workspace_value_completer())]
        name: Option<String>,

        #[arg(long, help = "Restore out-of-scope paths from the trunk head")]
        drop_unscoped: bool,
    },
    #[command(about = "Land a lane: gate, fast-forward trunk, ripple the new head to peers")]
    Land {
        #[arg(help = "Lane name", add = completion::workspace_value_completer())]
        name: String,

        #[arg(long, short = 'm', help = "Description for the landed head if it has none")]
        message: Option<String>,

        #[arg(long, help = "Skip the configured gate command")]
        no_gate: bool,

        #[arg(long, help = "Close and remove the lane after landing")]
        close: bool,
    },
    #[command(about = "Close a fully landed lane and remove its workspace")]
    Close {
        #[arg(help = "Lane name", add = completion::workspace_value_completer())]
        name: String,
    },
    #[command(about = "Abandon a lane: archive its diff, then remove workspace and registration")]
    Abandon {
        #[arg(help = "Lane name", add = completion::workspace_value_completer())]
        name: String,

        #[arg(long, short = 'y', help = "Skip destructive confirmation")]
        yes: bool,
    },
    #[command(about = "Collect ghost workspaces (directory gone) and orphaned lane records")]
    Gc {
        #[arg(long, help = "Apply the plan instead of printing it")]
        apply: bool,

        #[arg(long, short = 'y', help = "Skip confirmation when applying")]
        yes: bool,
    },
}

#[derive(Subcommand)]
enum ConfigCommands {
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
            eprintln!("{}", render_error_message(&error.to_string()));
            ExitCode::FAILURE
        }
    }
}

fn try_run(
    bin_name: &'static str,
    args: impl IntoIterator<Item = OsString>,
) -> Result<ExitCode, CliError> {
    let cli = parse(bin_name, args)?;
    let path = PathBuf::from(".");

    match cli.command {
        Commands::Switch {
            create,
            revision,
            workspace,
        } => commands::switch::run_switch(&path, &workspace, create, revision.as_deref())?,
        Commands::List { json, compact } => commands::list::run_list(&path, json, compact)?,
        Commands::Doctor {
            json,
            compact,
            deep,
        } => {
            return Ok(commands::doctor::run_doctor(
                &path, bin_name, json, compact, deep,
            )?);
        }
        Commands::Heal {
            changes,
            mine,
            apply,
            limit,
        } => {
            let limit = commands::heal::validated_limit(limit)?;
            commands::heal::run_heal(
                &path,
                &commands::heal::HealOptions {
                    changes: &changes,
                    mine,
                    apply,
                    limit,
                },
            )?;
        }
        Commands::Exec { workspace, args } => {
            return Ok(commands::exec::run_exec(&path, workspace.as_deref(), &args)?);
        }
        Commands::Remove { yes, workspace } => {
            commands::remove::run_remove(&path, &workspace, yes)?;
        }
        Commands::Merge { from, into } => {
            commands::merge::run_merge(&path, &from, into.as_deref())?;
        }
        Commands::Lane { command } => match command {
            LaneCommands::Open {
                name,
                paths,
                allow_overlap,
                sparse,
                full,
            } => {
                let sparse = if sparse {
                    Some(true)
                } else if full {
                    Some(false)
                } else {
                    None
                };
                commands::lane::run_lane_open(&path, &name, &paths, allow_overlap, sparse)?;
            }
            LaneCommands::Claim {
                name,
                paths,
                allow_overlap,
            } => commands::lane::run_lane_claim(&path, &name, &paths, allow_overlap)?,
            LaneCommands::List { json, compact } => {
                commands::lane::run_lane_list(&path, json, compact)?;
            }
            LaneCommands::Sync {
                name,
                drop_unscoped,
            } => commands::lane::run_lane_sync(&path, name.as_deref(), drop_unscoped)?,
            LaneCommands::Land {
                name,
                message,
                no_gate,
                close,
            } => commands::lane::run_lane_land(&path, &name, message.as_deref(), no_gate, close)?,
            LaneCommands::Close { name } => commands::lane::run_lane_close(&path, &name)?,
            LaneCommands::Abandon { name, yes } => {
                commands::lane::run_lane_abandon(&path, &name, yes)?;
            }
            LaneCommands::Gc { apply, yes } => commands::lane::run_lane_gc(&path, apply, yes)?,
        },
        Commands::Config { command } => match command {
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

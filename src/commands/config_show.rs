//! `navi config show`: print the effective repo configuration and where it
//! lives, so nobody has to guess which defaults are in force.

use std::path::Path;

use crate::Result;
use crate::repo::NaviWorkspace;

/// Run `config show`.
///
/// # Errors
///
/// Returns an error if the repo cannot be opened or serialization fails.
pub fn run_config_show(path: &Path, json: bool) -> Result<()> {
    let repo = NaviWorkspace::open(path)?;
    let config = repo.repo_config();
    let config_path = repo.repo_config_file_path();
    let exists = config_path.is_file();

    eprintln!(
        "config: {} ({})",
        config_path.display(),
        if exists { "exists" } else { "defaults; not written yet" }
    );
    eprintln!("workspace_template = {:?}", config.workspace_template.as_str());
    eprintln!("[lane]");
    eprintln!("trunk = {:?}", config.lane.trunk.as_str());
    match &config.lane.gate {
        Some(gate) => eprintln!("gate = {gate:?}"),
        None => eprintln!("# gate = (none configured)"),
    }
    eprintln!("sparse = {}", config.lane.sparse);
    eprintln!(
        "context_paths = [{}]",
        config
            .lane
            .context_paths
            .iter()
            .map(|path| format!("{:?}", path.as_str()))
            .collect::<Vec<_>>()
            .join(", ")
    );

    if json {
        #[derive(serde::Serialize)]
        struct LaneConfigJson<'a> {
            trunk: &'a str,
            gate: Option<&'a str>,
            sparse: bool,
            context_paths: Vec<&'a str>,
        }
        #[derive(serde::Serialize)]
        struct ConfigResult<'a> {
            path: &'a Path,
            exists: bool,
            workspace_template: &'a str,
            lane: LaneConfigJson<'a>,
        }
        println!(
            "{}",
            crate::output::render_json_envelope(
                "config show",
                &ConfigResult {
                    path: &config_path,
                    exists,
                    workspace_template: config.workspace_template.as_str(),
                    lane: LaneConfigJson {
                        trunk: config.lane.trunk.as_str(),
                        gate: config.lane.gate.as_deref(),
                        sparse: config.lane.sparse,
                        context_paths: config
                            .lane
                            .context_paths
                            .iter()
                            .map(crate::types::LanePath::as_str)
                            .collect(),
                    },
                }
            )?
        );
    }
    Ok(())
}

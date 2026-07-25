mod common;

use std::fs;
use std::path::{Path, PathBuf};

use common::cli::command_output;
use common::temp_repo::TempJjRepo;

fn lane_dir(repo: &TempJjRepo, lane: &str) -> PathBuf {
    repo.path()
        .with_file_name(format!("{}.{lane}", repo.repo_name()))
}

fn stderr_of(output: &std::process::Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn assert_success(output: &std::process::Output, context: &str) {
    assert!(
        output.status.success(),
        "{context} failed\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        stderr_of(output),
    );
}

fn open_lane(repo: &TempJjRepo, name: &str, path_prefix: &str) -> PathBuf {
    let output = command_output(
        "navi",
        repo.path(),
        &["lane", "open", name, "--path", path_prefix],
    );
    assert_success(&output, &format!("lane open {name}"));
    let dir = lane_dir(repo, name);
    assert!(dir.is_dir(), "lane workspace directory should exist");
    dir
}

fn write_lane_file(dir: &Path, relative: &str, contents: &str) {
    let path = dir.join(relative);
    let parent = path.parent().expect("lane file parent");
    fs::create_dir_all(parent).expect("create lane file dirs");
    fs::write(path, contents).expect("write lane file");
}

#[test]
fn lane_open_registers_write_set_and_reports_clear_weather() {
    let repo = TempJjRepo::new();
    open_lane(&repo, "alpha", "src");

    let registry = fs::read_to_string(
        repo.path()
            .join(".jj")
            .join("repo")
            .join("navi")
            .join("lanes.toml"),
    )
    .expect("read lane registry");
    assert!(registry.contains("name = \"alpha\""));
    assert!(registry.contains("src"));
    assert!(registry.contains("lifecycle = \"open\""));

    let output = command_output("navi", repo.path(), &["lane", "list"]);
    assert_success(&output, "lane list");
    let listing = stderr_of(&output);
    assert!(listing.contains("alpha"), "listing: {listing}");
    assert!(listing.contains("synced"), "listing: {listing}");
}

#[test]
fn lane_open_refuses_write_set_overlap() {
    let repo = TempJjRepo::new();
    open_lane(&repo, "alpha", "src");

    let output = command_output(
        "navi",
        repo.path(),
        &["lane", "open", "beta", "--path", "src/nested"],
    );
    assert!(!output.status.success());
    let stderr = stderr_of(&output);
    assert!(stderr.contains("overlaps open lane 'alpha'"), "stderr: {stderr}");

    let output = command_output(
        "navi",
        repo.path(),
        &[
            "lane",
            "open",
            "beta",
            "--path",
            "src/nested",
            "--allow-overlap",
        ],
    );
    assert_success(&output, "lane open with --allow-overlap");
}

#[test]
fn lane_land_fast_forwards_trunk_and_ripples_to_peers() {
    let repo = TempJjRepo::new();
    let alpha = open_lane(&repo, "alpha", "src");
    open_lane(&repo, "beta", "docs");

    write_lane_file(&alpha, "src/lib.rs", "pub fn hello() {}\n");

    let output = command_output(
        "navi",
        repo.path(),
        &["lane", "land", "alpha", "-m", "add hello"],
    );
    assert_success(&output, "lane land alpha");
    let landed = stderr_of(&output);
    assert!(landed.contains("landed lane 'alpha'"), "output: {landed}");
    assert!(landed.contains("ripple"), "output: {landed}");
    assert!(landed.contains("beta"), "output: {landed}");

    // Trunk fast-forwarded: its working-copy parent is the landed head.
    let trunk_head = repo.rev_id("default@-");
    let alpha_parent = repo.rev_id("alpha@-");
    assert_eq!(trunk_head, alpha_parent, "trunk head should be the landed head");
    let head_message = repo.run(&[
        "log",
        "-r",
        "default@-",
        "--no-graph",
        "-T",
        "description.first_line()",
    ]);
    assert_eq!(head_message.trim(), "add hello");

    // Fan-out: beta was rebased onto the new head.
    let beta_parent = repo.rev_id("beta@-");
    assert_eq!(trunk_head, beta_parent, "beta should ride the new head");
}

#[test]
fn lane_land_refuses_unscoped_changes_and_sync_can_drop_them() {
    let repo = TempJjRepo::new();
    let alpha = open_lane(&repo, "alpha", "src");

    write_lane_file(&alpha, "src/lib.rs", "in scope\n");
    write_lane_file(&alpha, "rogue.txt", "out of scope\n");

    let output = command_output(
        "navi",
        repo.path(),
        &["lane", "land", "alpha", "-m", "scoped work"],
    );
    assert!(!output.status.success());
    let stderr = stderr_of(&output);
    assert!(stderr.contains("outside its write-set"), "stderr: {stderr}");
    assert!(stderr.contains("rogue.txt"), "stderr: {stderr}");

    let output = command_output(
        "navi",
        repo.path(),
        &["lane", "sync", "alpha", "--drop-unscoped"],
    );
    assert_success(&output, "lane sync --drop-unscoped");
    let synced = stderr_of(&output);
    assert!(synced.contains("dropped"), "output: {synced}");

    let output = command_output(
        "navi",
        repo.path(),
        &["lane", "land", "alpha", "-m", "scoped work"],
    );
    assert_success(&output, "lane land after drop");
}

#[test]
fn lane_land_requires_sync_after_trunk_advances() {
    let repo = TempJjRepo::new();
    let alpha = open_lane(&repo, "alpha", "src");
    write_lane_file(&alpha, "src/lib.rs", "lane work\n");

    // Trunk advances directly (out-of-band commit).
    fs::write(repo.path().join("trunk.txt"), "trunk work\n").expect("write trunk file");
    repo.run(&["commit", "-m", "trunk work"]);

    let output = command_output(
        "navi",
        repo.path(),
        &["lane", "land", "alpha", "-m", "lane work"],
    );
    assert!(!output.status.success());
    let stderr = stderr_of(&output);
    assert!(stderr.contains("not synced"), "stderr: {stderr}");

    let output = command_output("navi", repo.path(), &["lane", "sync", "alpha"]);
    assert_success(&output, "lane sync alpha");
    assert!(stderr_of(&output).contains("rebased onto trunk head"));

    let output = command_output(
        "navi",
        repo.path(),
        &["lane", "land", "alpha", "-m", "lane work"],
    );
    assert_success(&output, "lane land after sync");
}

#[test]
fn lane_sync_reports_conflicts_and_land_refuses_them() {
    let repo = TempJjRepo::new();
    let alpha = open_lane(&repo, "alpha", "src");
    write_lane_file(&alpha, "src/lib.rs", "lane version\n");

    fs::create_dir_all(repo.path().join("src")).expect("create trunk src");
    fs::write(repo.path().join("src/lib.rs"), "trunk version\n").expect("write trunk file");
    repo.run(&["commit", "-m", "trunk touches src"]);

    let output = command_output("navi", repo.path(), &["lane", "sync", "alpha"]);
    assert!(output.status.success(), "sync itself should not fail");
    let synced = stderr_of(&output);
    assert!(synced.contains("conflict"), "output: {synced}");

    let output = command_output(
        "navi",
        repo.path(),
        &["lane", "land", "alpha", "-m", "lane version"],
    );
    assert!(!output.status.success());
    let stderr = stderr_of(&output);
    assert!(stderr.contains("conflicted"), "stderr: {stderr}");
}

#[test]
fn lane_land_runs_gate_and_gate_failure_blocks_landing() {
    let repo = TempJjRepo::new();
    // The gate rejects any tree containing docs/readme.md: alpha's landing
    // passes, beta's (which adds that file) fails.
    repo.write_navi_config(
        "workspace_template = \"../{repo}.{workspace}\"\n\n[lane]\ngate = \"test ! -f docs/readme.md\"\n",
    );
    let alpha = open_lane(&repo, "alpha", "src");
    write_lane_file(&alpha, "src/lib.rs", "gated\n");

    let output = command_output(
        "navi",
        repo.path(),
        &["lane", "land", "alpha", "-m", "gated work"],
    );
    assert_success(&output, "lane land with passing gate");
    assert!(stderr_of(&output).contains("gate: passed"));

    let beta = open_lane(&repo, "beta", "docs");
    write_lane_file(&beta, "docs/readme.md", "docs\n");
    let before = repo.rev_id("default@-");

    let output = command_output(
        "navi",
        repo.path(),
        &["lane", "land", "beta", "-m", "docs work"],
    );
    assert!(!output.status.success(), "gate should fail for beta");
    let stderr = stderr_of(&output);
    assert!(stderr.contains("gate command failed"), "stderr: {stderr}");
    assert_eq!(before, repo.rev_id("default@-"), "trunk must not move on gate failure");
}

#[test]
fn lane_land_close_retires_the_lane() {
    let repo = TempJjRepo::new();
    let alpha = open_lane(&repo, "alpha", "src");
    write_lane_file(&alpha, "src/lib.rs", "done\n");

    let output = command_output(
        "navi",
        repo.path(),
        &["lane", "land", "alpha", "-m", "final work", "--close"],
    );
    assert_success(&output, "lane land --close");
    assert!(stderr_of(&output).contains("closed and retired"));
    assert!(!lane_dir(&repo, "alpha").exists(), "workspace dir should be gone");

    let workspaces = repo.run(&["workspace", "list", "-T", "name ++ \"\\n\""]);
    assert!(!workspaces.contains("alpha"), "workspaces: {workspaces}");

    let output = command_output("navi", repo.path(), &["lane", "list"]);
    assert_success(&output, "lane list after close");
    assert!(stderr_of(&output).contains("closed"));
}

#[test]
fn lane_abandon_archives_the_diff() {
    let repo = TempJjRepo::new();
    let alpha = open_lane(&repo, "alpha", "src");
    write_lane_file(&alpha, "src/lib.rs", "doomed work\n");
    // Snapshot the lane so the abandoned work is visible to jj.
    TempJjRepo::run_at(&alpha, &["status"]);

    let output = command_output("navi", repo.path(), &["lane", "abandon", "alpha", "--yes"]);
    assert_success(&output, "lane abandon");
    let stderr = stderr_of(&output);
    assert!(stderr.contains("archived diff"), "stderr: {stderr}");
    assert!(!lane_dir(&repo, "alpha").exists());

    let archive_dir = repo
        .path()
        .join(".jj")
        .join("repo")
        .join("navi")
        .join("archive");
    let entries: Vec<_> = fs::read_dir(&archive_dir)
        .expect("archive dir")
        .collect::<std::io::Result<Vec<_>>>()
        .expect("archive entries");
    assert_eq!(entries.len(), 1, "one archived diff expected");
    let contents = fs::read_to_string(entries[0].path()).expect("read archive");
    assert!(contents.contains("doomed work"), "archive: {contents}");
}

#[test]
fn lane_gc_collects_ghost_workspaces() {
    let repo = TempJjRepo::new();
    let ghost = repo.create_workspace("ghost");
    fs::remove_dir_all(&ghost).expect("remove ghost directory");

    let output = command_output("navi", repo.path(), &["lane", "gc"]);
    assert_success(&output, "lane gc plan");
    let plan = stderr_of(&output);
    assert!(plan.contains("would forget ghost workspace 'ghost'"), "plan: {plan}");

    let output = command_output("navi", repo.path(), &["lane", "gc", "--apply", "--yes"]);
    assert_success(&output, "lane gc apply");
    assert!(stderr_of(&output).contains("forgot ghost workspace 'ghost'"));

    let workspaces = repo.run(&["workspace", "list", "-T", "name ++ \"\\n\""]);
    assert!(!workspaces.contains("ghost"), "workspaces: {workspaces}");
}

#[test]
fn sparse_lane_materializes_only_the_write_set() {
    let repo = TempJjRepo::new();
    // Seed trunk with files inside and outside the future write-set.
    fs::create_dir_all(repo.path().join("src")).expect("create src");
    fs::write(repo.path().join("src/lib.rs"), "seed\n").expect("write src seed");
    fs::create_dir_all(repo.path().join("big")).expect("create big");
    fs::write(repo.path().join("big/artifact.bin"), "heavy\n").expect("write big seed");
    repo.run(&["commit", "-m", "seed tree"]);

    let output = command_output(
        "navi",
        repo.path(),
        &["lane", "open", "alpha", "--path", "src", "--sparse"],
    );
    assert_success(&output, "lane open --sparse");

    let dir = lane_dir(&repo, "alpha");
    assert!(dir.join("src/lib.rs").is_file(), "write-set should materialize");
    assert!(
        !dir.join("big").exists(),
        "paths outside the write-set should not materialize"
    );

    write_lane_file(&dir, "src/lib.rs", "sparse work\n");
    let output = command_output(
        "navi",
        repo.path(),
        &["lane", "land", "alpha", "-m", "sparse landing"],
    );
    assert_success(&output, "sparse lane land");
}

#[test]
fn lane_list_json_reports_weather() {
    let repo = TempJjRepo::new();
    open_lane(&repo, "alpha", "src");

    let output = command_output("navi", repo.path(), &["lane", "list", "--json"]);
    assert_success(&output, "lane list --json");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value = serde_json::from_str(&stdout).expect("valid json");
    let lanes = parsed["lanes"].as_array().expect("lanes array");
    assert_eq!(lanes.len(), 1);
    assert_eq!(lanes[0]["name"], "alpha");
    assert_eq!(lanes[0]["weather"], "clear");
    assert_eq!(lanes[0]["synced"], true);
}

#[test]
fn lane_land_gate_side_effects_do_not_land() {
    let repo = TempJjRepo::new();
    // The gate writes an out-of-scope artifact, simulating a build byproduct.
    repo.write_navi_config(
        "workspace_template = \"../{repo}.{workspace}\"\n\n[lane]\ngate = \"echo artifact > build-output.bin\"\n",
    );
    let alpha = open_lane(&repo, "alpha", "src");
    write_lane_file(&alpha, "src/lib.rs", "pub fn gated() {}\n");

    let output = command_output(
        "navi",
        repo.path(),
        &["lane", "land", "alpha", "-m", "gated work"],
    );
    assert_success(&output, "lane land with artifact-writing gate");

    let landed_paths = TempJjRepo::run_at(
        repo.path(),
        &[
            "diff",
            "--summary",
            "-r",
            "default@-",
            "--ignore-working-copy",
        ],
    );
    assert!(
        landed_paths.contains("src/lib.rs"),
        "landed diff should contain the lane's work: {landed_paths}"
    );
    assert!(
        !landed_paths.contains("build-output.bin"),
        "gate artifacts must not land: {landed_paths}"
    );
}

#[test]
fn lane_sync_drop_unscoped_handles_fileset_metacharacters() {
    let repo = TempJjRepo::new();
    let alpha = open_lane(&repo, "alpha", "src");
    write_lane_file(&alpha, "src/lib.rs", "in scope\n");
    write_lane_file(&alpha, "docs/a&b.txt", "out of scope\n");

    let output = command_output(
        "navi",
        repo.path(),
        &["lane", "sync", "alpha", "--drop-unscoped"],
    );
    assert_success(&output, "lane sync --drop-unscoped");
    assert!(
        stderr_of(&output).contains("docs/a&b.txt"),
        "drop should report the metacharacter path"
    );
    assert!(
        !alpha.join("docs/a&b.txt").exists(),
        "dropped file must actually be restored from trunk (removed)"
    );
    assert!(
        alpha.join("src/lib.rs").exists(),
        "in-scope work must survive the drop"
    );
}

#[test]
fn lane_land_leaves_peers_fresh_without_divergence() {
    let repo = TempJjRepo::new();
    let alpha = open_lane(&repo, "alpha", "src");
    let beta = open_lane(&repo, "beta", "docs");
    write_lane_file(&alpha, "src/lib.rs", "alpha work\n");
    // Beta has on-disk work that jj has not snapshotted yet.
    write_lane_file(&beta, "docs/notes.md", "beta work\n");

    let output = command_output(
        "navi",
        repo.path(),
        &["lane", "land", "alpha", "-m", "land alpha"],
    );
    assert_success(&output, "lane land alpha");

    // The peer must be immediately usable: not stale, work intact, and no
    // divergent change minted by the fan-out rebase.
    let status = TempJjRepo::run_at(&beta, &["status"]);
    assert!(
        status.contains("docs/notes.md"),
        "peer's on-disk work must survive the landing: {status}"
    );
    let divergent = TempJjRepo::run_at(
        repo.path(),
        &[
            "log",
            "-r",
            "divergent()",
            "--no-graph",
            "-T",
            "change_id",
            "--ignore-working-copy",
        ],
    );
    assert!(
        divergent.trim().is_empty(),
        "fan-out must not mint divergent changes: {divergent}"
    );
}

#[test]
fn lane_open_json_emits_machine_envelope_on_stdout() {
    let repo = TempJjRepo::new();
    let output = command_output(
        "navi",
        repo.path(),
        &["lane", "open", "alpha", "--path", "src", "--json"],
    );
    assert_success(&output, "lane open --json");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let envelope: serde_json::Value = serde_json::from_str(&stdout).expect("stdout is one JSON envelope");
    assert_eq!(envelope["ok"], serde_json::Value::Bool(true));
    assert_eq!(envelope["command"], "lane open");
    assert_eq!(envelope["result"]["name"], "alpha");
    assert_eq!(envelope["result"]["paths"][0], "src");
}

#[test]
fn lane_land_json_emits_machine_envelope_and_errors_carry_codes() {
    let repo = TempJjRepo::new();
    let lane = open_lane(&repo, "alpha", "src");
    write_lane_file(&lane, "src/alpha.txt", "alpha work\n");

    let output = command_output(
        "navi",
        repo.path(),
        &["lane", "land", "alpha", "-m", "alpha work", "--json"],
    );
    assert_success(&output, "lane land --json");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let envelope: serde_json::Value = serde_json::from_str(&stdout).expect("land envelope parses");
    assert_eq!(envelope["ok"], serde_json::Value::Bool(true));
    assert_eq!(envelope["command"], "lane land");
    assert_eq!(envelope["result"]["landed_changes"], 1);

    // Landing again with nothing new must fail with a stable error code on
    // stdout while the human message stays on stderr.
    let output = command_output(
        "navi",
        repo.path(),
        &["lane", "land", "alpha", "--json"],
    );
    assert!(!output.status.success(), "second land should fail");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let envelope: serde_json::Value = serde_json::from_str(&stdout).expect("error envelope parses");
    assert_eq!(envelope["ok"], serde_json::Value::Bool(false));
    assert_eq!(envelope["code"], "lane-nothing-to-land");
    assert!(stderr_of(&output).contains("error[lane-nothing-to-land]:"));
}

#[test]
fn repo_flag_targets_a_repo_from_outside_it() {
    let repo = TempJjRepo::new();
    let outside = tempfile::tempdir().expect("outside dir");
    let repo_path = repo.path().display().to_string();

    let output = command_output(
        "navi",
        outside.path(),
        &["-R", &repo_path, "lane", "open", "alpha", "--path", "src", "--json"],
    );
    assert_success(&output, "lane open via -R");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let envelope: serde_json::Value = serde_json::from_str(&stdout).expect("envelope parses");
    assert_eq!(envelope["result"]["name"], "alpha");
    assert!(lane_dir(&repo, "alpha").is_dir());
}

#[test]
fn lane_open_with_revision_bases_on_that_revision() {
    let repo = TempJjRepo::new();
    let alpha = open_lane(&repo, "alpha", "src");
    write_lane_file(&alpha, "src/alpha.txt", "alpha work\n");
    let output = command_output("navi", repo.path(), &["exec", "-w", "alpha", "--", "describe", "-m", "alpha work"]);
    assert_success(&output, "describe alpha work");

    // Stack beta on alpha's unlanded working-copy commit.
    let output = command_output(
        "navi",
        repo.path(),
        &["lane", "open", "beta", "--path", "docs", "-r", "alpha@", "--json"],
    );
    assert_success(&output, "lane open beta -r alpha@");
    let beta = lane_dir(&repo, "beta");
    assert!(
        beta.join("src").join("alpha.txt").is_file(),
        "stacked lane should contain the base lane's unlanded work"
    );
}

#[test]
fn lane_release_shrinks_write_set_and_refuses_emptying() {
    let repo = TempJjRepo::new();
    let output = command_output(
        "navi",
        repo.path(),
        &["lane", "open", "alpha", "--path", "src", "--path", "docs"],
    );
    assert_success(&output, "lane open with two paths");

    let output = command_output(
        "navi",
        repo.path(),
        &["lane", "release", "alpha", "--path", "docs", "--json"],
    );
    assert_success(&output, "lane release docs");
    let envelope: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&output.stdout)).expect("release envelope");
    assert_eq!(envelope["result"]["write_set"], serde_json::json!(["src"]));

    let output = command_output(
        "navi",
        repo.path(),
        &["lane", "release", "alpha", "--path", "src", "--json"],
    );
    assert!(!output.status.success(), "emptying release must fail");
    let envelope: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&output.stdout)).expect("error envelope");
    assert_eq!(envelope["code"], "lane-release-would-empty");
}

#[test]
fn lane_gc_prune_drops_retired_records() {
    let repo = TempJjRepo::new();
    open_lane(&repo, "alpha", "src");
    let output = command_output(
        "navi",
        repo.path(),
        &["lane", "abandon", "alpha", "--yes"],
    );
    assert_success(&output, "lane abandon alpha");

    let output = command_output(
        "navi",
        repo.path(),
        &["lane", "gc", "--prune", "--apply", "-y", "--json"],
    );
    assert_success(&output, "lane gc --prune --apply");
    let envelope: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&output.stdout)).expect("gc envelope");
    assert_eq!(envelope["result"]["plan"]["prunable_lanes"], serde_json::json!(["alpha"]));

    let registry = fs::read_to_string(
        repo.path()
            .join(".jj")
            .join("repo")
            .join("navi")
            .join("lanes.toml"),
    )
    .expect("read lane registry");
    assert!(
        !registry.contains("name = \"alpha\""),
        "pruned record should be gone from the registry"
    );
}

#[test]
fn lane_list_lifecycle_filters_rows() {
    let repo = TempJjRepo::new();
    open_lane(&repo, "alpha", "src");
    open_lane(&repo, "beta", "docs");
    let output = command_output("navi", repo.path(), &["lane", "abandon", "beta", "--yes"]);
    assert_success(&output, "abandon beta");

    let output = command_output(
        "navi",
        repo.path(),
        &["lane", "list", "--lifecycle", "open", "--json", "--no-snapshot"],
    );
    assert_success(&output, "lane list --lifecycle open");
    let payload: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&output.stdout)).expect("lane list json");
    let lanes = payload["lanes"].as_array().expect("lanes array");
    assert_eq!(lanes.len(), 1);
    assert_eq!(lanes[0]["name"], "alpha");
}

#[test]
fn lane_land_allow_unscoped_and_gate_override() {
    let repo = TempJjRepo::new();
    let lane = open_lane(&repo, "alpha", "src");
    write_lane_file(&lane, "src/alpha.txt", "in scope\n");
    write_lane_file(&lane, "docs/out.txt", "out of scope\n");

    // Overriding the gate with a failing command must block the landing.
    let output = command_output(
        "navi",
        repo.path(),
        &[
            "lane", "land", "alpha", "-m", "alpha work", "--allow-unscoped", "--gate", "false",
            "--json",
        ],
    );
    assert!(!output.status.success(), "failing gate override must block");
    let envelope: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&output.stdout)).expect("gate error envelope");
    assert_eq!(envelope["code"], "gate-failed");

    // Without --allow-unscoped the out-of-scope path blocks the landing.
    let output = command_output(
        "navi",
        repo.path(),
        &["lane", "land", "alpha", "-m", "alpha work"],
    );
    assert!(!output.status.success(), "unscoped landing must fail");
    assert!(stderr_of(&output).contains("error[lane-unscoped-changes]"));

    // --allow-unscoped with a passing gate lands everything.
    let output = command_output(
        "navi",
        repo.path(),
        &[
            "lane", "land", "alpha", "-m", "alpha work", "--allow-unscoped", "--gate", "true",
        ],
    );
    assert_success(&output, "land --allow-unscoped");
}

#[test]
fn config_show_reports_effective_config() {
    let repo = TempJjRepo::new();
    let output = command_output("navi", repo.path(), &["config", "show", "--json"]);
    assert_success(&output, "config show --json");
    let envelope: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&output.stdout)).expect("config envelope");
    assert_eq!(envelope["command"], "config show");
    assert_eq!(envelope["result"]["lane"]["trunk"], "default");
}

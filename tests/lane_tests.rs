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

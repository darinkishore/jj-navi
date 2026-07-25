//! Phase 4: landing into a bookmark via the integration workspace.

mod common;

use std::fs;
use std::path::{Path, PathBuf};

use common::cli::command_output;
use common::temp_repo::TempJjRepo;

fn assert_success(output: &std::process::Output, context: &str) {
    assert!(
        output.status.success(),
        "{context} failed\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
}

fn lane_dir(repo: &TempJjRepo, lane: &str) -> PathBuf {
    repo.path()
        .with_file_name(format!("{}.{lane}", repo.repo_name()))
}

fn commit_file(path: &Path, name: &str, contents: &str, message: &str) {
    fs::write(path.join(name), contents).expect("write committed file");
    TempJjRepo::run_at(path, &["commit", "-m", message]);
}

fn configure_bookmark_target(repo: &TempJjRepo) {
    TempJjRepo::run_at(repo.path(), &["bookmark", "create", "main", "-r", "@-"]);
    repo.write_navi_config(
        "workspace_template = \"../{repo}.{workspace}\"\n\n[lane]\ntarget = \"main\"\n",
    );
}

fn commit_of(repo: &TempJjRepo, revset: &str) -> String {
    TempJjRepo::run_at(
        repo.path(),
        &[
            "--ignore-working-copy",
            "log",
            "-r",
            revset,
            "--no-graph",
            "-T",
            "commit_id.short(12)",
        ],
    )
    .trim()
    .to_owned()
}

#[test]
fn landing_advances_bookmark_without_touching_any_working_copy() {
    let repo = TempJjRepo::new();
    commit_file(repo.path(), "base.txt", "base\n", "Base commit");
    configure_bookmark_target(&repo);
    let default_wc_before = commit_of(&repo, "@");

    let output = command_output(
        "navi",
        repo.path(),
        &["lane", "open", "alpha", "--path", "src"],
    );
    assert_success(&output, "lane open alpha");
    let alpha = lane_dir(&repo, "alpha");
    fs::create_dir_all(alpha.join("src")).expect("mkdir src");
    fs::write(alpha.join("src/alpha.txt"), "alpha work\n").expect("write alpha work");

    // A peer lane, to prove fan-out still ripples in bookmark mode.
    let output = command_output(
        "navi",
        repo.path(),
        &["lane", "open", "beta", "--path", "docs"],
    );
    assert_success(&output, "lane open beta");

    let output = command_output(
        "navi",
        repo.path(),
        &["lane", "land", "alpha", "-m", "alpha work", "--json"],
    );
    assert_success(&output, "lane land alpha");
    let envelope: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&output.stdout)).expect("land envelope");
    let landed_commit = envelope["result"]["landed"]["commit_id"]
        .as_str()
        .expect("landed commit id");

    // The bookmark moved to the landed head...
    let main_now = commit_of(&repo, "main");
    assert!(
        main_now.starts_with(landed_commit) || landed_commit.starts_with(&main_now),
        "bookmark 'main' ({main_now}) should be at the landed head ({landed_commit})"
    );
    // ...and no live working copy was fast-forwarded.
    assert_eq!(
        commit_of(&repo, "@"),
        default_wc_before,
        "the default workspace's working copy must not move on a bookmark landing"
    );

    // The integration workspace was auto-created sparse-empty.
    let integration = lane_dir(&repo, "navi-integration");
    assert!(integration.is_dir(), "integration workspace directory exists");
    assert!(
        !integration.join("base.txt").exists(),
        "integration workspace stays sparse-empty"
    );

    // The peer lane was rebased onto the new bookmark target.
    let output = command_output(
        "navi",
        repo.path(),
        &["lane", "list", "--json", "--lifecycle", "open"],
    );
    assert_success(&output, "lane list");
    let payload: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&output.stdout)).expect("lane list json");
    let beta = payload["lanes"]
        .as_array()
        .expect("lanes")
        .iter()
        .find(|lane| lane["name"] == "beta")
        .expect("beta row");
    assert_eq!(beta["synced"], serde_json::Value::Bool(true));
}

#[test]
fn landing_refuses_undescribed_work_in_bookmark_mode() {
    let repo = TempJjRepo::new();
    commit_file(repo.path(), "base.txt", "base\n", "Base commit");
    configure_bookmark_target(&repo);

    let output = command_output(
        "navi",
        repo.path(),
        &["lane", "open", "alpha", "--path", "src"],
    );
    assert_success(&output, "lane open alpha");
    let alpha = lane_dir(&repo, "alpha");
    fs::create_dir_all(alpha.join("src")).expect("mkdir src");

    // Park a non-empty commit with no description under the head.
    fs::write(alpha.join("src/undescribed.txt"), "no message\n").expect("write undescribed");
    TempJjRepo::run_at(&alpha, &["new"]);
    fs::write(alpha.join("src/described.txt"), "top\n").expect("write described");

    let output = command_output(
        "navi",
        repo.path(),
        &["lane", "land", "alpha", "-m", "top work", "--json"],
    );
    assert!(
        !output.status.success(),
        "undescribed work must block a bookmark landing"
    );
    let envelope: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&output.stdout)).expect("error envelope");
    assert_eq!(envelope["code"], "target-hygiene");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("undescribed commit(s)"),
        "refusal should name the violation"
    );

    // Describing the parked commit unblocks the landing.
    TempJjRepo::run_at(&alpha, &["describe", "@-", "-m", "bottom work"]);
    let output = command_output(
        "navi",
        repo.path(),
        &["lane", "land", "alpha", "-m", "top work"],
    );
    assert_success(&output, "land after describing");
}

#[test]
fn missing_target_bookmark_is_a_coded_error() {
    let repo = TempJjRepo::new();
    commit_file(repo.path(), "base.txt", "base\n", "Base commit");
    repo.write_navi_config(
        "workspace_template = \"../{repo}.{workspace}\"\n\n[lane]\ntarget = \"main\"\n",
    );

    let output = command_output(
        "navi",
        repo.path(),
        &["lane", "open", "alpha", "--path", "src", "--json"],
    );
    assert!(!output.status.success(), "open must fail without the bookmark");
    let envelope: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&output.stdout)).expect("error envelope");
    assert_eq!(envelope["code"], "target-bookmark-missing");
}

#[test]
fn doctor_deep_reports_target_hygiene() {
    let repo = TempJjRepo::new();
    commit_file(repo.path(), "base.txt", "base\n", "Base commit");
    configure_bookmark_target(&repo);

    let output = command_output("navi", repo.path(), &["doctor", "--deep", "--json"]);
    let payload: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&output.stdout)).expect("doctor json");
    let findings = payload["findings"].as_array().expect("findings");
    assert!(
        findings.iter().any(|finding| finding["code"] == "target_hygiene"
            && finding["message"]
                .as_str()
                .is_some_and(|message| message.contains("clean and pushable"))),
        "doctor --deep should report the target as clean: {payload}"
    );
}

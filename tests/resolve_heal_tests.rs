//! End-to-end tests for the conflict-resolution policies and the heal
//! content-diff surface.

mod common;

use std::fs;
use std::path::Path;

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

fn commit_file(path: &Path, name: &str, contents: &str, message: &str) {
    fs::write(path.join(name), contents).expect("write committed file");
    TempJjRepo::run_at(path, &["commit", "-m", message]);
}

/// Build a lane whose CHANGELOG.md conflicts with trunk, then sweep the
/// configured `[resolve]` policy and verify the union lands both sides.
#[test]
fn resolve_policy_sweep_union_resolves_changelog_conflicts() {
    let repo = TempJjRepo::new();
    commit_file(repo.path(), "CHANGELOG.md", "# log\n- base\n", "Add changelog");

    let output = command_output(
        "navi",
        repo.path(),
        &["lane", "open", "alpha", "--path", "CHANGELOG.md"],
    );
    assert_success(&output, "lane open alpha");
    let lane = repo
        .path()
        .with_file_name(format!("{}.alpha", repo.repo_name()));

    // Lane and trunk both append to the changelog: classic 2-sided append
    // conflict once the lane syncs.
    fs::write(lane.join("CHANGELOG.md"), "# log\n- base\n- alpha entry\n")
        .expect("write lane changelog");
    TempJjRepo::run_at(&lane, &["describe", "-m", "alpha entry"]);
    commit_file(
        repo.path(),
        "CHANGELOG.md",
        "# log\n- base\n- trunk entry\n",
        "Trunk entry",
    );

    let output = command_output("navi", repo.path(), &["lane", "sync", "alpha"]);
    assert_success(&output, "lane sync alpha");

    let output = command_output("navi", repo.path(), &["conflicts", "--json"]);
    assert_success(&output, "conflicts census");
    let census: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&output.stdout)).expect("census envelope");
    assert!(
        !census["result"]["roots"].as_array().expect("roots").is_empty(),
        "sync should have produced a conflict"
    );

    // Sweeping with no policies configured is a coded error.
    let output = command_output("navi", repo.path(), &["resolve", "--apply", "--json"]);
    assert!(!output.status.success(), "sweep without policies must fail");
    let envelope: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&output.stdout)).expect("error envelope");
    assert_eq!(envelope["code"], "engine");

    // Configure the policy and sweep for real.
    repo.write_navi_config(
        "workspace_template = \"../{repo}.{workspace}\"\n\n[resolve]\n\"CHANGELOG.md\" = \"union\"\n",
    );
    let output = command_output("navi", repo.path(), &["resolve", "--apply", "--json"]);
    assert_success(&output, "resolve policy sweep");
    let envelope: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&output.stdout)).expect("sweep envelope");
    assert_eq!(envelope["command"], "resolve");
    let files = envelope["result"]["files"].as_array().expect("files");
    assert_eq!(files[0]["file"], "CHANGELOG.md");
    assert!(
        !files[0]["roots"].as_array().expect("roots").is_empty(),
        "sweep should have resolved at least one root"
    );

    let output = command_output("navi", repo.path(), &["conflicts", "--json"]);
    assert_success(&output, "post-sweep census");
    let census: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&output.stdout)).expect("census envelope");
    assert!(
        census["result"]["roots"].as_array().expect("roots").is_empty(),
        "no conflicts should remain after the sweep"
    );

    let changelog =
        fs::read_to_string(lane.join("CHANGELOG.md")).expect("read resolved changelog");
    assert!(changelog.contains("alpha entry"), "union keeps the lane side");
    assert!(changelog.contains("trunk entry"), "union keeps the trunk side");
    assert!(
        !changelog.contains("<<<<<<<"),
        "no conflict markers may survive"
    );
}

/// Mint a real divergent change (two ops rewriting the same commit) and
/// check the heal plan carries the content-diff summary, then heals it.
#[test]
fn heal_plan_reports_content_diff_and_heals_divergence() {
    let repo = TempJjRepo::new();
    commit_file(repo.path(), "base.txt", "base\n", "Base commit");

    // Mint a head with no working copy on it, then rewrite it from two
    // operation-log branches: describe it, and describe it again from the
    // pre-describe operation. The op merge leaves one change with two
    // visible commits and no live @ in either chain.
    let base = repo.rev_id("@-");
    commit_file(repo.path(), "detached.txt", "detached\n", "Detached work");
    let target = repo.rev_id("@-");
    TempJjRepo::run_at(repo.path(), &["new", &base]);

    let op_before = TempJjRepo::run_at(
        repo.path(),
        &["op", "log", "-n1", "--no-graph", "-T", "id.short()"],
    )
    .trim()
    .to_owned();
    TempJjRepo::run_at(repo.path(), &["describe", &target, "-m", "first rewrite"]);
    TempJjRepo::run_at(
        repo.path(),
        &[
            "--at-operation",
            &op_before,
            "describe",
            &target,
            "-m",
            "divergent rewrite",
        ],
    );

    let output = command_output("navi", repo.path(), &["heal", "--json"]);
    assert_success(&output, "heal plan");
    let envelope: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&output.stdout)).expect("heal envelope");
    assert_eq!(envelope["command"], "heal");
    let healed = envelope["result"]["healed"].as_array().expect("healed");
    assert!(
        !healed.is_empty(),
        "the divergence should be planned as healable: {envelope}"
    );
    let abandon = healed[0]["abandon"].as_array().expect("abandon");
    assert!(
        abandon[0]["changed_paths_vs_keep"].is_array(),
        "plan should carry a content-diff summary: {envelope}"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("differs from keep") || stderr.contains("identical tree"),
        "human plan should describe the content diff: {stderr}"
    );

    let output = command_output("navi", repo.path(), &["heal", "--apply", "--json"]);
    assert_success(&output, "heal apply");

    let output = command_output("navi", repo.path(), &["heal", "--json"]);
    assert_success(&output, "post-heal plan");
    let envelope: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&output.stdout)).expect("heal envelope");
    assert!(
        envelope["result"]["healed"].as_array().expect("healed").is_empty()
            && envelope["result"]["skipped"].as_array().expect("skipped").is_empty(),
        "no divergence should remain after heal --apply: {envelope}"
    );
}

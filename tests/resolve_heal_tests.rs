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

/// A three-parent merge produces a 3-sided conflict, which jj's
/// `resolve --tool` refuses; navi must route it through the squash path.
/// The sides share a line, which must survive exactly once (deduped union).
#[test]
fn resolve_union_handles_many_sided_conflicts_with_dedup() {
    let repo = TempJjRepo::new();
    commit_file(repo.path(), "log.md", "# log\n", "Add log");
    let base = repo.rev_id("@-");

    let mut entries = Vec::new();
    for name in ["alpha", "beta", "gamma"] {
        TempJjRepo::run_at(repo.path(), &["new", &base]);
        fs::write(
            repo.path().join("log.md"),
            format!("# log\n- shared entry\n- {name} entry\n"),
        )
        .expect("write side");
        TempJjRepo::run_at(repo.path(), &["commit", "-m", &format!("{name} entry")]);
        entries.push(repo.rev_id("@-"));
    }
    // Three-parent merge working copy -> 3-sided conflict; park it behind
    // a child so the conflict root is not @ itself.
    TempJjRepo::run_at(
        repo.path(),
        &["new", &entries[0], &entries[1], &entries[2]],
    );
    TempJjRepo::run_at(repo.path(), &["new"]);

    // The census must report the side count so callers can route.
    let output = command_output("navi", repo.path(), &["conflicts", "--json"]);
    assert_success(&output, "pre-resolve census");
    let census: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&output.stdout)).expect("census envelope");
    let root = &census["result"]["roots"][0];
    assert_eq!(root["files"][0]["path"], "log.md");
    assert_eq!(root["files"][0]["sides"], 3);
    assert!(root["change_id"].is_string(), "census carries change ids");

    let output = command_output(
        "navi",
        repo.path(),
        &["resolve", "--union", "log.md", "--apply", "--json"],
    );
    assert_success(&output, "resolve 3-sided union");
    let envelope: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&output.stdout)).expect("resolve envelope");
    assert!(
        !envelope["result"]["roots"].as_array().expect("roots").is_empty(),
        "the 3-sided root should be resolved: {envelope}"
    );

    let output = command_output("navi", repo.path(), &["conflicts", "--json"]);
    assert_success(&output, "post-resolve census");
    let census: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&output.stdout)).expect("census envelope");
    assert!(
        census["result"]["roots"].as_array().expect("roots").is_empty(),
        "no conflicts should remain: {census}"
    );

    let resolved = TempJjRepo::run_at(
        repo.path(),
        &["--ignore-working-copy", "file", "show", "-r", "@-", "log.md"],
    );
    for entry in ["- alpha entry", "- beta entry", "- gamma entry"] {
        assert!(resolved.contains(entry), "union keeps '{entry}': {resolved}");
    }
    assert_eq!(
        resolved.matches("- shared entry").count(),
        1,
        "shared line must survive exactly once: {resolved}"
    );
    assert!(!resolved.contains("<<<<<<<"), "no markers: {resolved}");

    // The scratch workspace must not linger.
    let workspaces = repo.run(&["workspace", "list"]);
    assert!(
        !workspaces.contains("navi-resolve"),
        "scratch workspace cleaned up: {workspaces}"
    );
}

/// The newest sibling being an empty shell must not beat a sibling with
/// content: default skips, --prefer-content keeps the content.
#[test]
fn heal_guards_empty_shell_winners() {
    let repo = TempJjRepo::new();
    commit_file(repo.path(), "base.txt", "base\n", "Base commit");
    let base = repo.rev_id("@-");
    // Park a described-but-empty commit K off to the side.
    TempJjRepo::run_at(repo.path(), &["new", &base]);
    TempJjRepo::run_at(repo.path(), &["describe", "-m", "content sibling"]);
    let target = repo.rev_id("@");
    TempJjRepo::run_at(repo.path(), &["new", &base]);

    let op_before = TempJjRepo::run_at(
        repo.path(),
        &["op", "log", "-n1", "--no-graph", "-T", "id.short()"],
    )
    .trim()
    .to_owned();
    // Older op branch: squash real content into K.
    fs::write(repo.path().join("f.txt"), "content\n").expect("write content");
    TempJjRepo::run_at(repo.path(), &["squash", "--into", &target]);
    // Newest op branch (at-op always runs later): re-describe the old
    // empty version of K — a shell with a message and no content.
    TempJjRepo::run_at(
        repo.path(),
        &[
            "--ignore-working-copy",
            "--at-operation",
            &op_before,
            "describe",
            &target,
            "-m",
            "empty shell",
        ],
    );

    let output = command_output("navi", repo.path(), &["heal", "--json"]);
    assert_success(&output, "heal plan");
    let envelope: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&output.stdout)).expect("heal envelope");
    let skipped = envelope["result"]["skipped"].as_array().expect("skipped");
    assert!(
        skipped
            .iter()
            .any(|skip| skip["reason"].as_str().is_some_and(|reason| reason.contains("empty shell"))),
        "empty-shell winner must be skipped by default: {envelope}"
    );

    let output = command_output("navi", repo.path(), &["heal", "--prefer-content", "--json"]);
    assert_success(&output, "heal --prefer-content plan");
    let envelope: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&output.stdout)).expect("heal envelope");
    let healed = envelope["result"]["healed"].as_array().expect("healed");
    assert!(
        !healed.is_empty(),
        "--prefer-content should plan the heal: {envelope}"
    );

    let kept = healed[0]["keep_commit"].as_str().expect("keep commit").to_owned();

    let output = command_output(
        "navi",
        repo.path(),
        &["heal", "--prefer-content", "--apply", "--json"],
    );
    assert_success(&output, "heal --prefer-content apply");
    // The surviving sibling must be the content-carrying one.
    let survivor = TempJjRepo::run_at(
        repo.path(),
        &[
            "--ignore-working-copy",
            "file",
            "show",
            "-r",
            &kept,
            "f.txt",
        ],
    );
    assert!(
        survivor.contains("content"),
        "the kept sibling must carry the content: {survivor}"
    );
}

/// With a policy configured, `lane sync` heals policied conflicts itself:
/// the conflict dies at birth instead of propagating.
#[test]
fn lane_sync_auto_applies_resolve_policies() {
    let repo = TempJjRepo::new();
    commit_file(repo.path(), "CHANGELOG.md", "# log\n- base\n", "Add changelog");
    let output = command_output(
        "navi",
        repo.path(),
        &["lane", "open", "alpha", "--path", "CHANGELOG.md"],
    );
    assert_success(&output, "lane open alpha");
    repo.write_navi_config(
        "workspace_template = \"../{repo}.{workspace}\"\n\n[resolve]\n\"CHANGELOG.md\" = \"union\"\n",
    );
    let lane = repo
        .path()
        .with_file_name(format!("{}.alpha", repo.repo_name()));
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
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("applying [resolve] policies"),
        "sync should announce the auto-resolve"
    );

    let output = command_output("navi", repo.path(), &["conflicts", "--json"]);
    assert_success(&output, "post-sync census");
    let census: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&output.stdout)).expect("census envelope");
    assert!(
        census["result"]["roots"].as_array().expect("roots").is_empty(),
        "sync should have auto-resolved the conflict: {census}"
    );
    let changelog = fs::read_to_string(lane.join("CHANGELOG.md")).expect("read changelog");
    assert!(changelog.contains("alpha entry") && changelog.contains("trunk entry"));
}

/// `tidy --apply --yes` runs gc + policies + heal in one shot.
#[test]
fn tidy_runs_the_repair_pipeline() {
    let repo = TempJjRepo::new();
    commit_file(repo.path(), "base.txt", "base\n", "Base commit");
    // Mint one healable divergence (same recipe as the heal test).
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
        &["--at-operation", &op_before, "describe", &target, "-m", "divergent rewrite"],
    );

    // Plan first: no mutations without --apply.
    let output = command_output("navi", repo.path(), &["tidy", "--json"]);
    assert_success(&output, "tidy plan");
    let envelope: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&output.stdout)).expect("tidy envelope");
    assert_eq!(envelope["command"], "tidy");
    assert_eq!(envelope["result"]["applied"], serde_json::Value::Bool(false));
    assert!(
        !envelope["result"]["heal"]["healed"].as_array().expect("healed").is_empty(),
        "plan should include the divergence: {envelope}"
    );

    let output = command_output("navi", repo.path(), &["tidy", "--apply", "--yes", "--json"]);
    assert_success(&output, "tidy apply");

    let output = command_output("navi", repo.path(), &["heal", "--json"]);
    assert_success(&output, "post-tidy heal check");
    let envelope: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&output.stdout)).expect("heal envelope");
    assert!(
        envelope["result"]["healed"].as_array().expect("healed").is_empty(),
        "tidy should have healed the divergence: {envelope}"
    );
}

/// The tripwire warns when the divergent count rises past the baseline.
#[test]
fn divergence_tripwire_warns_on_new_divergence() {
    let repo = TempJjRepo::new();
    commit_file(repo.path(), "base.txt", "base\n", "Base commit");
    // Establish a zero baseline.
    let output = command_output("navi", repo.path(), &["exec", "--", "status"]);
    assert_success(&output, "baseline exec");
    assert!(
        !String::from_utf8_lossy(&output.stderr).contains("divergent commits rose"),
        "no warning at zero divergence"
    );

    // Mint a divergence outside navi (raw jj, the exact hazard).
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
        &["--at-operation", &op_before, "describe", &target, "-m", "divergent rewrite"],
    );

    let output = command_output("navi", repo.path(), &["exec", "--", "status"]);
    assert_success(&output, "post-divergence exec");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("divergent commits rose from 0"),
        "tripwire should fire: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Bulk abandon takes dead subtrees but refuses working-copy chains.
#[test]
fn abandon_bulk_removes_dead_subtrees_with_guards() {
    let repo = TempJjRepo::new();
    commit_file(repo.path(), "base.txt", "base\n", "Base commit");
    let base = repo.rev_id("@-");
    // A dead subtree: two stacked commits off base, no workspace on them.
    TempJjRepo::run_at(repo.path(), &["new", &base]);
    commit_file(repo.path(), "dead1.txt", "dead\n", "Dead one");
    commit_file(repo.path(), "dead2.txt", "dead\n", "Dead two");
    let dead_head = repo.rev_id("@-");
    TempJjRepo::run_at(repo.path(), &["new", &base]);

    // Refusal: the working copy's chain.
    let output = command_output(
        "navi",
        repo.path(),
        &["abandon", "-r", "::@", "--apply", "--json"],
    );
    assert!(!output.status.success(), "wc chain must be refused");

    // The dead subtree goes, in one op.
    let revset = format!("{base}..{dead_head}");
    let output = command_output(
        "navi",
        repo.path(),
        &["abandon", "-r", &revset, "--apply", "--json"],
    );
    assert_success(&output, "abandon dead subtree");
    let envelope: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&output.stdout)).expect("abandon envelope");
    assert_eq!(envelope["result"]["commits"].as_array().expect("commits").len(), 2);

    let remaining = TempJjRepo::run_at(
        repo.path(),
        &["--ignore-working-copy", "log", "-r", "all()", "--no-graph", "-T", "description.first_line() ++ \"\\n\""],
    );
    assert!(!remaining.contains("Dead one"), "subtree gone: {remaining}");
}

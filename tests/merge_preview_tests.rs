mod common;

use predicates::prelude::*;
use std::path::Path;

use common::{TempJjRepo, command};

fn change_id_at(path: &Path, revset: &str) -> String {
    TempJjRepo::run_at(
        path,
        &[
            "--ignore-working-copy",
            "log",
            "-r",
            revset,
            "--no-graph",
            "-T",
            "change_id.short(12)",
        ],
    )
    .trim()
    .to_owned()
}

fn commit_file(path: &Path, name: &str, contents: &str, message: &str) {
    std::fs::write(path.join(name), contents).expect("write committed file");
    TempJjRepo::run_at(path, &["commit", "-m", message]);
}

#[test]
fn merge_preview_defaults_target_to_current_workspace() {
    let repo = TempJjRepo::new();
    let feature_path = repo.create_workspace("feature-a");
    commit_file(
        &feature_path,
        "feature.txt",
        "feature\n",
        "Add feature file",
    );

    command("navi")
        .current_dir(repo.path())
        .args(["merge", "--from", "feature-a"])
        .assert()
        .success()
        .stdout(predicate::str::is_empty())
        .stderr(predicate::str::contains(
            "Merged 1 change from feature-a into default",
        ))
        .stderr(predicate::str::contains("Duplicated "))
        .stderr(predicate::str::contains("Working copy  (@) now at:"));

    assert!(repo.path().join("feature.txt").exists());
}

#[test]
fn merge_preview_resolves_explicit_target_workspace() {
    let repo = TempJjRepo::new();
    let feature_path = repo.create_workspace("feature-a");
    let integration_path = repo.create_workspace("integration");
    commit_file(
        &feature_path,
        "feature.txt",
        "feature\n",
        "Add feature file",
    );

    command("navi")
        .current_dir(repo.path())
        .args(["merge", "-f", "feature-a", "-i", "integration"])
        .assert()
        .success()
        .stdout(predicate::str::is_empty())
        .stderr(predicate::str::contains(
            "Merged 1 change from feature-a into integration",
        ));

    assert!(integration_path.join("feature.txt").exists());
}

#[test]
fn merge_json_errors_emit_machine_envelope_on_stdout() {
    let repo = TempJjRepo::new();
    repo.create_workspace("feature-a");

    command("navi")
        .current_dir(repo.path())
        .args(["merge", "--from", "feature-a", "--json"])
        .assert()
        .failure()
        .stdout(predicate::str::contains("\"ok\": false"))
        .stdout(predicate::str::contains("\"code\": \"merge-source-empty\""))
        .stderr(predicate::str::contains("error[merge-source-empty]:"));
}

#[test]
fn merge_preview_fails_for_missing_source_workspace() {
    let repo = TempJjRepo::new();

    command("navi")
        .current_dir(repo.path())
        .args(["merge", "--from", "missing"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "error[merge-workspace-missing]: merge source workspace 'missing' does not exist",
        ));
}

#[test]
fn merge_preview_fails_for_missing_target_workspace() {
    let repo = TempJjRepo::new();
    repo.create_workspace("feature-a");

    command("navi")
        .current_dir(repo.path())
        .args(["merge", "--from", "feature-a", "--into", "missing"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "error[merge-workspace-missing]: merge target workspace 'missing' does not exist",
        ));
}

#[test]
fn merge_preview_fails_for_same_source_and_target() {
    let repo = TempJjRepo::new();

    command("navi")
        .current_dir(repo.path())
        .args(["merge", "--from", "default"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "error[merge-same-workspace]: cannot merge workspace 'default' into itself",
        ));
}

#[test]
fn merge_preview_refuses_stale_source_workspace_path() {
    let repo = TempJjRepo::new();
    let feature_path = repo.create_workspace("feature-a");
    let moved_path = feature_path.with_file_name(format!(
        "{}.moved",
        feature_path
            .file_name()
            .expect("workspace dir name")
            .to_string_lossy()
    ));
    std::fs::rename(&feature_path, &moved_path).expect("move workspace dir");
    std::fs::create_dir(&feature_path).expect("create stale workspace dir");

    command("navi")
        .current_dir(repo.path())
        .args(["merge", "--from", "feature-a"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "error[merge-workspace-unavailable]: merge source workspace 'feature-a' is not ready: workspace path is stale",
        ));
}

#[test]
fn merge_preview_refuses_stale_target_workspace_path() {
    let repo = TempJjRepo::new();
    repo.create_workspace("feature-a");
    let target_path = repo.create_workspace("integration");
    let moved_path = target_path.with_file_name(format!(
        "{}.moved",
        target_path
            .file_name()
            .expect("workspace dir name")
            .to_string_lossy()
    ));
    std::fs::rename(&target_path, &moved_path).expect("move workspace dir");
    std::fs::create_dir(&target_path).expect("create stale workspace dir");

    command("navi")
        .current_dir(repo.path())
        .args(["merge", "--from", "feature-a", "--into", "integration"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "error[merge-workspace-unavailable]: merge target workspace 'integration' is not ready: workspace path is stale",
        ));
}

#[test]
fn merge_preview_does_not_rewrite_source_workspace() {
    let repo = TempJjRepo::new();
    let feature_path = repo.create_workspace("feature-a");
    commit_file(
        &feature_path,
        "feature.txt",
        "feature\n",
        "Add feature file",
    );
    let before = change_id_at(&feature_path, "feature-a@");

    command("navi")
        .current_dir(repo.path())
        .args(["merge", "--from", "feature-a"])
        .assert()
        .success();

    let after = change_id_at(&feature_path, "feature-a@");
    assert_eq!(after, before);
}

#[test]
fn merge_preview_snapshots_and_merges_dirty_source_workspace() {
    let repo = TempJjRepo::new();
    let feature_path = repo.create_workspace("feature-a");
    std::fs::write(feature_path.join("dirty.txt"), "unsnapshotted\n").expect("write dirty file");

    command("navi")
        .current_dir(repo.path())
        .args(["merge", "--from", "feature-a"])
        .assert()
        .success();

    assert!(repo.path().join("dirty.txt").exists());
}

#[test]
fn merge_preview_reports_rebase_conflict_with_recovery_hint() {
    let repo = TempJjRepo::new();
    commit_file(repo.path(), "shared.txt", "initial\n", "Add shared file");
    let feature_path = repo.create_workspace("feature-a");

    commit_file(repo.path(), "shared.txt", "target\n", "Update target file");
    commit_file(
        &feature_path,
        "shared.txt",
        "source\n",
        "Update source file",
    );

    command("navi")
        .current_dir(repo.path())
        .args(["merge", "--from", "feature-a"])
        .assert()
        .failure()
        .stdout(predicate::str::is_empty())
        .stderr(predicate::str::contains(
            "error[merge-rebase-failed]: merge stopped during rebase",
        ))
        .stderr(predicate::str::contains(
            "duplicated work remains in the repo and source workspace was not rewritten",
        ))
        .stderr(predicate::str::contains("jj resolve --list"));
}

#[test]
fn merge_handles_multi_root_sources() {
    let repo = TempJjRepo::new();
    let feature_path = repo.create_workspace("feature-a");
    // Two sibling roots relative to the target, joined by a merge working
    // copy so both sit inside the source cone.
    commit_file(&feature_path, "one.txt", "one\n", "Add one");
    let one = change_id_at(&feature_path, "@-");
    let base = change_id_at(&feature_path, "@--");
    TempJjRepo::run_at(&feature_path, &["new", &base]);
    commit_file(&feature_path, "two.txt", "two\n", "Add two");
    let two = change_id_at(&feature_path, "@-");
    TempJjRepo::run_at(&feature_path, &["new", &one, &two]);

    command("navi")
        .current_dir(repo.path())
        .args(["merge", "--from", "feature-a", "--json"])
        .assert()
        .success()
        .stderr(predicate::str::contains("Merged 2 changes from feature-a into default"));

    assert!(repo.path().join("one.txt").exists());
    assert!(repo.path().join("two.txt").exists());
}

#[test]
fn merge_accepts_explicit_revset() {
    let repo = TempJjRepo::new();
    let feature_path = repo.create_workspace("feature-a");
    commit_file(&feature_path, "wanted.txt", "wanted\n", "Add wanted");
    let wanted = change_id_at(&feature_path, "@-");
    commit_file(&feature_path, "unwanted.txt", "unwanted\n", "Add unwanted");

    command("navi")
        .current_dir(repo.path())
        .args(["merge", "-r", &wanted])
        .assert()
        .success()
        .stderr(predicate::str::contains("into default"));

    assert!(repo.path().join("wanted.txt").exists());
    assert!(
        !repo.path().join("unwanted.txt").exists(),
        "revset merge must not bring unselected changes"
    );
}

#[test]
fn merge_resolves_current_workspace_alias_for_target() {
    let repo = TempJjRepo::new();
    let feature_path = repo.create_workspace("feature-a");
    commit_file(&feature_path, "feature.txt", "feature\n", "Add feature");

    command("navi")
        .current_dir(repo.path())
        .args(["merge", "--from", "feature-a", "--into", "@"])
        .assert()
        .success()
        .stderr(predicate::str::contains("into default"));

    assert!(repo.path().join("feature.txt").exists());
}

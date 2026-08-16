use std::sync::atomic::AtomicBool;

use herdr_reviewr::forge::{GhError, PendingReviewBinding, ReviewEvent, submit_pending_review};

fn binding() -> PendingReviewBinding {
    PendingReviewBinding {
        host: "github.com".into(),
        owner: "owner".into(),
        repository: "repo".into(),
        number: 7,
        head_oid: "head".into(),
        review_id: "review-1".into(),
        comment_url: None,
    }
}

/// Run one assertion in a child whose `PATH` starts with a fake production-named `gh` binary.
/// This preserves the production adapter contract: tests inject `gh` only through the child PATH.
fn fake_gh_child(test_name: &str, response: &str, log: Option<&std::path::Path>) -> bool {
    if std::env::var_os("GITHUB_SUBMIT_CHILD").is_some() {
        return false;
    }

    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("bin");
    std::fs::create_dir(&bin).unwrap();
    let gh = bin.join("gh");
    std::fs::write(
        &gh,
        r#"#!/bin/sh
if [ -n "$GH_LOG" ]; then
  { for arg in "$@"; do printf '<%s>' "$arg"; done; printf '\n'; } >> "$GH_LOG"
fi
printf '%s\n' "$GH_RESPONSE"
"#,
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    let mut command = std::process::Command::new(std::env::current_exe().unwrap());
    command
        .args(["--exact", test_name])
        .env("GITHUB_SUBMIT_CHILD", "1")
        .env("GH_RESPONSE", response)
        .env("PATH", format!("{}:{}", bin.display(), std::env::var("PATH").unwrap_or_default()));
    if let Some(log) = log {
        command.env("GH_LOG", log).env("GITHUB_SUBMIT_LOG", log);
    }
    let status = command.status().unwrap();
    assert!(status.success(), "fake-gh child {test_name} failed");
    true
}

fn submit() -> Result<Option<String>, GhError> {
    let dir = tempfile::tempdir().unwrap();
    submit_pending_review(
        dir.path(),
        &binding(),
        ReviewEvent::RequestChanges,
        &AtomicBool::new(false),
    )
}

#[test]
fn submit_mutation_uses_only_the_explicit_event_and_review_id() {
    let dir = tempfile::tempdir().unwrap();
    let log = std::env::var_os("GITHUB_SUBMIT_LOG")
        .map_or_else(|| dir.path().join("gh.log"), std::path::PathBuf::from);
    if fake_gh_child(
        "submit_mutation_uses_only_the_explicit_event_and_review_id",
        r#"{"data":{"submitPullRequestReview":{"pullRequestReview":{"url":"https://github.example/review/1"}}}}"#,
        Some(&log),
    ) {
        return;
    }

    let url = submit().unwrap();
    assert_eq!(url.as_deref(), Some("https://github.example/review/1"));
    let calls = std::fs::read_to_string(log).unwrap();
    assert!(calls.contains("submitPullRequestReview"));
    assert!(calls.contains("<-f><id=review-1><-f><event=REQUEST_CHANGES>"));
    assert!(!calls.contains("addPullRequestReview"));
}

fn assert_submit_response_is_retryable_failure(test_name: &str, response: &str) {
    if fake_gh_child(test_name, response, None) {
        return;
    }
    assert!(matches!(submit(), Err(GhError::Other(_))));
}

#[test]
fn submit_rejects_zero_exit_graphql_errors() {
    assert_submit_response_is_retryable_failure(
        "submit_rejects_zero_exit_graphql_errors",
        r#"{"errors":[{"message":"review is no longer pending"}]}"#,
    );
}

#[test]
fn submit_rejects_zero_exit_malformed_json() {
    assert_submit_response_is_retryable_failure(
        "submit_rejects_zero_exit_malformed_json",
        "this is not JSON",
    );
}

#[test]
fn submit_rejects_zero_exit_missing_review_url() {
    assert_submit_response_is_retryable_failure(
        "submit_rejects_zero_exit_missing_review_url",
        r#"{"data":{"submitPullRequestReview":{"pullRequestReview":null}}}"#,
    );
}

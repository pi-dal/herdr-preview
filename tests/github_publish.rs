use std::sync::atomic::AtomicBool;

mod common;

use herdr_reviewr::forge::{
    PendingReviewComment, canonical_patch_position, publish_pending_comment,
};

#[test]
fn canonical_position_uses_the_base_to_head_patch_and_resets_for_a_second_hunk() {
    let repo = common::Repo::init();
    let mut base = String::new();
    for line in 1..=20 {
        use std::fmt::Write as _;
        writeln!(base, "line {line}").unwrap();
    }
    repo.write("src/example.rs", &base);
    repo.commit_all("base");
    let base_oid = repo.git(&["rev-parse", "HEAD"]).trim().to_owned();

    let mut head = String::new();
    for line in 1..=20 {
        use std::fmt::Write as _;
        match line {
            2 => head.push_str("line two changed\n"),
            15 => head.push_str("line fifteen changed\n"),
            _ => writeln!(head, "line {line}").unwrap(),
        }
    }
    repo.write("src/example.rs", &head);
    repo.commit_all("head");
    let head_oid = repo.git(&["rev-parse", "HEAD"]).trim().to_owned();

    // With three lines of context, line 15 belongs to the second real hunk. Its hunk-local
    // position counts three context lines, the deleted old row, then this addition.
    assert_eq!(
        canonical_patch_position(
            repo.path(),
            &base_oid,
            &head_oid,
            "src/example.rs",
            15,
            "+line fifteen changed",
        ),
        Some(5)
    );
    // An absent anchor and an option-like path never produce a guessed position.
    assert_eq!(
        canonical_patch_position(
            repo.path(),
            &base_oid,
            &head_oid,
            "src/example.rs",
            15,
            "+not the committed line",
        ),
        None
    );
    assert_eq!(
        canonical_patch_position(
            repo.path(),
            &base_oid,
            &head_oid,
            "--output=/tmp/untrusted",
            15,
            "+line fifteen changed",
        ),
        None
    );
}

/// The publish adapter is intentionally tested through a fake `gh`: this asserts the exact
/// external contract rather than mocking the GraphQL builder in-process.
#[test]
fn creates_then_appends_a_preview_owned_pending_review_without_submitting() {
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("bin");
    std::fs::create_dir(&bin).unwrap();
    let log = std::env::var_os("GITHUB_PUBLISH_LOG")
        .map_or_else(|| dir.path().join("gh.log"), std::path::PathBuf::from);
    let gh = bin.join("gh");
    std::fs::write(&gh, r#"#!/bin/sh
{ for arg in "$@"; do printf '<%s>' "$arg"; done; printf '\n'; } >> "$GH_LOG"
n=$(wc -l < "$GH_LOG" | tr -d ' ')
case "$n" in
1) echo '{"data":{"repository":{"pullRequest":{"id":"123"}}}}' ;;
2) echo '{"data":{"addPullRequestReview":{"pullRequestReview":{"id":"123","comments":{"nodes":[{"url":"https://example/one"}]}}}}}' ;;
3) echo '{"data":{"addPullRequestReviewComment":{"comment":{"url":"https://example/two"}}}}' ;;
esac
"#).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    if std::env::var_os("GITHUB_PUBLISH_CHILD").is_none() {
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "creates_then_appends_a_preview_owned_pending_review_without_submitting",
            ])
            .env("GITHUB_PUBLISH_CHILD", "1")
            .env("GH_LOG", &log)
            .env("GITHUB_PUBLISH_LOG", &log)
            .env("PATH", format!("{}:{}", bin.display(), std::env::var("PATH").unwrap_or_default()))
            .status()
            .unwrap();
        assert!(status.success());
        return;
    }
    let cancel = AtomicBool::new(false);
    // These scalar-looking values must remain GraphQL strings/IDs rather than `gh -F`-coerced
    // booleans, nulls, or integers.
    let comment = PendingReviewComment { path: "123", position: 7, body: "true" };
    let first = publish_pending_comment(
        dir.path(),
        "github.com",
        "o",
        "r",
        4,
        "abc",
        None,
        comment,
        &cancel,
    )
    .unwrap();
    let second = publish_pending_comment(
        dir.path(),
        "github.com",
        "o",
        "r",
        4,
        "abc",
        Some(&first),
        comment,
        &cancel,
    )
    .unwrap();
    assert_eq!(second.review_id, "123");
    let calls = std::fs::read_to_string(log).unwrap();
    assert!(
        calls.contains("addPullRequestReview(input:{pullRequestId:$pr,body:\"\",commitOID:$commit")
    );
    assert!(calls.contains("comments:[{body:$body,path:$path,position:$position}]"));
    assert!(calls.contains("addPullRequestReviewComment(input:{pullRequestReviewId:$id,body:$body,path:$path,position:$position,commitOID:$commit})"));
    // Only GraphQL Int! values use `-F`; scalar-looking strings/IDs/SHA values use raw `-f`.
    assert!(calls.contains("<-f><owner=o><-f><repo=r><-F><number=4>"));
    assert!(
        calls.contains("<-f><pr=123><-f><body=true><-f><path=123><-F><position=7><-f><commit=abc>")
    );
    assert!(calls.contains(
        "<-f><id=123><-f><body=true><-f><path=123><-F><position=7><-f><commit=abc><-F><number=4>"
    ));
    assert!(!calls.contains("<-F><body=true>"));
    assert!(!calls.contains("<-F><path=123>"));
    assert!(!calls.contains("<-F><id=123>"));
    assert!(!calls.contains("line:"));
    assert!(!calls.contains("side:"));
    assert!(!calls.contains("submitPullRequestReview"));
}

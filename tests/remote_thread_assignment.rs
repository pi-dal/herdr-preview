//! Remote GitHub thread assignment is delivered only through an explicit picker and confirmation.
#![cfg(unix)]

mod common;

use common::{Repo, app_on, comment, pr_snapshot};
use herdr_reviewr::{
    app::{Mode, Tab},
    forge::{CommentKind, PrView},
    handle_key,
    keymap::Keymap,
    ui,
};
use ratatui::{
    Terminal,
    backend::TestBackend,
    crossterm::event::{KeyCode, KeyEvent, KeyModifiers},
    layout::Rect,
};
use std::{env, fs, os::unix::fs::PermissionsExt, path::Path, process::Command};

fn fake(dir: &Path) {
    let script = dir.join("herdr");
    fs::write(
        &script,
        r#"#!/bin/sh
dir=$(dirname "$0")
printf '%s\0' "$@" >> "$dir/args"
case "$1 $2" in
  'agent list') cat "$dir/agents.json" ;;
  'tab list') echo '{"result":{"tabs":[{"tab_id":"t1","label":"Review"}]}}' ;;
  'pane send-text') [ "${REMOTE_ASSIGN_FAIL:-}" != 1 ] || exit 1 ;;
  *) : ;;
esac
"#,
    )
    .unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
}

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::from(code)
}
fn modified_enter() -> KeyEvent {
    KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT)
}

fn remote_app(repo: &Repo) -> herdr_reviewr::app::App {
    let mut app = app_on(repo);
    let mut pr = pr_snapshot();
    let mut finding = comment();
    finding.kind = CommentKind::Finding;
    finding.url = "https://github.com/o/r/pull/1#discussion_r17".into();
    finding.anchor = "tracked.txt:7".into();
    finding.author = "eve".into();
    finding.body = "Please preserve this exact body.".into();
    finding.snippet = Some("+ unsafe input".into());
    pr.comments = vec![finding];
    app.pr = PrView::Pr(Box::new(pr));
    app.tab = Tab::Pr;
    app
}

fn drive_to_confirmation(app: &mut herdr_reviewr::app::App) {
    let area = Rect::new(0, 0, 100, 30);
    let keymap = Keymap::default();
    handle_key(app, key(KeyCode::Char('A')), area, &keymap).unwrap();
    assert!(matches!(app.mode, Mode::RemoteAssignPicker { .. }));
    handle_key(app, key(KeyCode::Enter), area, &keymap).unwrap();
    assert!(matches!(app.mode, Mode::ConfirmRemoteAssign { .. }));
}

fn command_groups(bytes: &[u8]) -> Vec<Vec<String>> {
    let args: Vec<String> = bytes
        .split(|byte| *byte == 0)
        .filter(|arg| !arg.is_empty())
        .map(|arg| String::from_utf8(arg.to_vec()).unwrap())
        .collect();
    let mut commands = Vec::new();
    let mut index = 0;
    while index < args.len() {
        let count =
            match (args.get(index).map(String::as_str), args.get(index + 1).map(String::as_str)) {
                (Some("agent"), Some("focus")) => 3,
                (Some("agent" | "tab"), Some("list")) => 2,
                (Some("pane"), Some("send-text")) => 4,
                _ => panic!("unexpected fake Herdr command at {index}: {:?}", args[index]),
            };
        commands.push(args[index..index + count].to_vec());
        index += count;
    }
    commands
}

#[test]
fn remote_assignment_is_framed_frozen_and_retryable() {
    if env::var("REMOTE_ASSIGN_CHILD").is_err() {
        for failed in [false, true] {
            let temp = tempfile::TempDir::new().unwrap();
            fake(temp.path());
            fs::write(temp.path().join("agents.json"), r#"{"result":{"agents":[{"agent":"codex","agent_status":"idle","pane_id":"p1","tab_id":"t1","workspace_id":"w","cwd":"/w"}]}}"#).unwrap();
            let status = Command::new(env::current_exe().unwrap())
                .args(["--exact", "remote_assignment_is_framed_frozen_and_retryable"])
                .env("REMOTE_ASSIGN_CHILD", "1")
                .env("REMOTE_ASSIGN_FAIL", if failed { "1" } else { "0" })
                .env("HERDR_BIN_PATH", temp.path().join("herdr"))
                .env("HERDR_WORKSPACE_ID", "w")
                .env("HERDR_PANE_ID", "p9")
                .env("REMOTE_ASSIGN_DIR", temp.path())
                .status()
                .unwrap();
            assert!(status.success());
            let commands = command_groups(&fs::read(temp.path().join("args")).unwrap());
            let send =
                commands.iter().find(|command| command[0..2] == ["pane", "send-text"]).unwrap();
            assert_eq!(send[2], "p1");
            let payload = &send[3];
            assert!(payload.starts_with("\x1b[200~") && payload.ends_with("\x1b[201~"));
            assert!(payload.contains("https://github.com/o/r/pull/1#discussion_r17"));
            assert!(payload.contains("**Author:** @eve"));
            assert!(payload.contains("**Location:** tracked.txt:7"));
            assert!(payload.contains("Please preserve this exact body."));
            assert!(payload.contains("+ unsafe input"));
            assert!(!commands.iter().any(|command| {
                command.iter().any(|arg| arg.contains("send-keys") || arg == "Enter")
            }));
            assert!(!commands.iter().any(|command| command.first().is_some_and(|arg| arg == "gh")));
            if failed {
                assert!(!commands.iter().any(|command| command[0..2] == ["agent", "focus"]));
            } else {
                assert!(commands.iter().any(|command| command[0..2] == ["agent", "focus"]));
            }
        }
        return;
    }

    let repo = Repo::init();
    repo.write("tracked.txt", "before\n");
    let mut app = remote_app(&repo);
    let area = Rect::new(0, 0, 100, 30);
    let keymap = Keymap::default();

    // A missing immutable URL and a non-GitHub provider never enter the assignment flow.
    let PrView::Pr(pr) = &mut app.pr else { unreachable!() };
    pr.comments[0].url.clear();
    handle_key(&mut app, key(KeyCode::Char('A')), area, &keymap).unwrap();
    assert_eq!(app.mode, Mode::Normal);
    let PrView::Pr(pr) = &mut app.pr else { unreachable!() };
    pr.comments[0].url = "https://github.com/o/r/pull/1#discussion_r17".into();
    app.pr_forge = herdr_reviewr::git::Forge::GitLab;
    handle_key(&mut app, key(KeyCode::Char('A')), area, &keymap).unwrap();
    assert_eq!(app.mode, Mode::Normal);
    app.pr_forge = herdr_reviewr::git::Forge::GitHub;

    // Unsafe, blank-after-trim, control, and bidi URLs cannot even enter the confirmation.
    for unsafe_url in [
        "   ",
        "javascript:alert(1)",
        "file:///etc/passwd",
        "https://example.test/\x1b[31m",
        "https://example.test/\u{202e}hidden",
    ] {
        let PrView::Pr(pr) = &mut app.pr else { unreachable!() };
        pr.comments[0].url = unsafe_url.into();
        handle_key(&mut app, key(KeyCode::Char('O')), area, &keymap).unwrap();
        assert_eq!(app.mode, Mode::Normal, "{unsafe_url:?} must not open a confirmation");
    }
    let PrView::Pr(pr) = &mut app.pr else { unreachable!() };
    pr.comments[0].url = "https://github.com/o/r/pull/1#discussion_r17".into();

    // Opening a direct thread is separately confirmed; Esc and modified Enter cannot launch it.
    handle_key(&mut app, key(KeyCode::Char('O')), area, &keymap).unwrap();
    assert!(matches!(app.mode, Mode::ConfirmOpenRemoteThread { .. }));
    handle_key(&mut app, modified_enter(), area, &keymap).unwrap();
    assert!(matches!(app.mode, Mode::ConfirmOpenRemoteThread { .. }));
    handle_key(&mut app, key(KeyCode::Esc), area, &keymap).unwrap();
    assert_eq!(app.mode, Mode::Normal);

    drive_to_confirmation(&mut app);
    // Modified Enter and Esc cannot deliver. Esc restores the pre-picker state.
    handle_key(&mut app, modified_enter(), area, &keymap).unwrap();
    assert!(matches!(app.mode, Mode::ConfirmRemoteAssign { .. }));
    handle_key(&mut app, key(KeyCode::Esc), area, &keymap).unwrap();
    assert_eq!(app.mode, Mode::Normal);
    drive_to_confirmation(&mut app);
    handle_key(&mut app, key(KeyCode::Enter), area, &keymap).unwrap();

    let receipt = app.remote_thread_receipt(&app.pr_snapshot().unwrap().comments[0]);
    let failed = env::var("REMOTE_ASSIGN_FAIL").as_deref() == Ok("1");
    if failed {
        assert!(matches!(receipt, Some(herdr_reviewr::app::RemoteThreadReceipt::Failed { .. })));
        assert!(app.status.contains("kept"));
    } else {
        assert!(matches!(receipt, Some(herdr_reviewr::app::RemoteThreadReceipt::Delivered { .. })));
        assert!(app.status.contains("GitHub unchanged"));
        assert!(matches!(
            app.remote_thread_worktree_state(&app.pr_snapshot().unwrap().comments[0]),
            Some(herdr_reviewr::app::RemoteWorktreeState::Unchanged)
        ));
        repo.write("tracked.txt", "after\n");
        // The read pane never performs filesystem I/O: a landed read-only PR refresh reconciles
        // the cached baseline instead.
        let refreshed = app.pr.clone();
        app.apply_pr(refreshed);
        assert!(matches!(
            app.remote_thread_worktree_state(&app.pr_snapshot().unwrap().comments[0]),
            Some(herdr_reviewr::app::RemoteWorktreeState::Changed)
        ));
        // A refresh may move the anchor, but this same immutable discussion URL retains its receipt.
        let PrView::Pr(pr) = &mut app.pr else { unreachable!() };
        pr.comments[0].anchor = "renamed/module.rs:99".into();
        let moved_comment = pr.comments[0].clone();
        assert!(matches!(
            app.remote_thread_receipt(&moved_comment),
            Some(herdr_reviewr::app::RemoteThreadReceipt::Delivered { .. })
        ));
    }

    // Every receipt state keeps the provider lifecycle visible. Forge-derived text is sanitized
    // only for paint; the raw task payload above was frozen before selection.
    for (resolved, outdated, lifecycle) in
        [(false, false, "open"), (true, false, "resolved"), (false, true, "outdated")]
    {
        let PrView::Pr(pr) = &mut app.pr else { unreachable!() };
        pr.comments[0].is_resolved = resolved;
        pr.comments[0].is_outdated = outdated;
        pr.comments[0].author = "eve\x1b[31m".into();
        let mut terminal = Terminal::new(TestBackend::new(300, 40)).unwrap();
        terminal.draw(|frame| ui::render(frame, &app)).unwrap();
        let painted = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(ratatui::buffer::Cell::symbol)
            .collect::<String>();
        let receipt = if failed { "assign failed" } else { "assigned" };
        assert!(painted.contains(&format!("{receipt} · {lifecycle}")), "{painted}");
        assert!(!painted.contains('\x1b'));
    }
}

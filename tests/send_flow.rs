//! End-to-end send dispatch through a fake herdr binary (`specs/herdr-host.md`,
//! `specs/input.md`). This file is its own test process, so the HERDR_* environment it
//! sets can never leak into another test binary, and no real herdr pane is ever addressed.
#![cfg(unix)]

mod common;

use std::env;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use common::{Repo, app_on};
use herdr_reviewr::app::{App, Focus, Mode, RepositoryMode};
use herdr_reviewr::keymap::{Action, Key, Keymap};
use herdr_reviewr::model::DeliveryReceipt;
use herdr_reviewr::ui;
use herdr_reviewr::{handle_key, handle_mouse};
use ratatui::crossterm::event::{
    KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::layout::Rect;
use ratatui::{Terminal, backend::TestBackend};

// `cwd` rides every real `agent list` entry (api notes). Send ignores it and resolves from
// the workspace, so it is here to keep the fixture honest rather than to steer the send.
const TWO_AGENTS: &str = r#"{"result":{"agents":[
  {"agent":"claude","agent_status":"idle","pane_id":"w8:p1","tab_id":"w8:t1","workspace_id":"w8","cwd":"/w/one"},
  {"agent":"codex","agent_status":"working","pane_id":"w8:p2","tab_id":"w8:t1","workspace_id":"w8","cwd":"/w/two"}
]}}"#;
const ONE_AGENT: &str = r#"{"result":{"agents":[
  {"agent":"claude","agent_status":"idle","pane_id":"w8:p1","tab_id":"w8:t1","workspace_id":"w8","cwd":"/w/one"}
]}}"#;

/// A fake herdr: answers `agent list` from `agents.json`, `tab list` with one label, logs
/// every invocation, and succeeds at everything else (`pane send-text`, `pane focus`). It
/// fails whatever `fail` holds, so a dead pane and a broken enumeration both have a shape.
fn write_fake_herdr(dir: &Path) -> PathBuf {
    let script = dir.join("herdr");
    fs::write(
        &script,
        "#!/bin/sh\n\
         dir=$(dirname \"$0\")\n\
         echo \"$@\" >> \"$dir/log\"\n\
         case \"$*\" in\n\
           $(cat \"$dir/fail\" 2>/dev/null || echo __none__)*)\n\
             echo '{\"error\":{\"code\":\"pane_not_found\",\"message\":\"pane w8:p1 not found\"},\"id\":\"cli:request\"}' >&2\n\
             exit 1 ;;\n\
         esac\n\
         case \"$1 $2\" in\n\
           \"agent list\") cat \"$dir/agents.json\" ;;\n\
           \"tab list\") echo '{\"result\":{\"tabs\":[{\"tab_id\":\"w8:t1\",\"label\":\"Grip\"}]}}' ;;\n\
           *) : ;;\n\
         esac\n",
    )
    .unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
    script
}

/// Make the fake herdr exit non-zero for every invocation starting with `prefix`.
fn fail_on(dir: &Path, prefix: &str) {
    fs::write(dir.join("fail"), prefix).unwrap();
}

fn fail_on_nothing(dir: &Path) {
    let _ = fs::remove_file(dir.join("fail"));
}

fn log(dir: &Path) -> String {
    fs::read_to_string(dir.join("log")).unwrap_or_default()
}

/// Save one comment on the first added line, so `Send` has something to deliver.
fn write_comment(app: &mut App, text: &str) {
    app.focus = Focus::Diff;
    app.diff_cursor = app.visible.iter().position(|r| r.marker() == '+').unwrap();
    app.start_comment();
    app.input = text.to_string();
    app.submit_comment();
}

fn press(app: &mut App, code: KeyCode, area: Rect, keymap: &Keymap) {
    handle_key(app, KeyEvent::from(code), area, keymap).unwrap();
}

/// The crate forbids `unsafe`, which rules out in-process `env::set_var`, so the parent
/// run re-executes this same test in a child process with the HERDR_* seam applied at
/// spawn — env applied to a child is safe, and the child alone runs the body.
#[test]
fn send_dispatches_one_agent_directly_and_several_through_the_picker() {
    if env::var("SEND_FLOW_CHILD").is_err() {
        let staging = tempfile::TempDir::new().expect("tempdir");
        let script = write_fake_herdr(staging.path());
        let out = std::process::Command::new(env::current_exe().unwrap())
            .args([
                "--exact",
                "send_dispatches_one_agent_directly_and_several_through_the_picker",
                "--nocapture",
            ])
            .env("SEND_FLOW_CHILD", "1")
            .env("FAKE_HERDR_DIR", staging.path())
            .env("HERDR_BIN_PATH", &script)
            .env("HERDR_WORKSPACE_ID", "w8")
            .env("HERDR_PANE_ID", "w8:p9")
            .output()
            .expect("re-exec the test with the fake herdr env");
        assert!(
            out.status.success(),
            "child run failed:\n{}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr),
        );
        // libtest exits 0 when `--exact` matches nothing, so the status alone cannot tell a
        // passing body from a filter that selected no test. The fake herdr's log is the proof
        // the body actually ran and delivered.
        assert!(
            log(staging.path()).contains("pane send-text"),
            "the child ran no send — did the test name and the `--exact` filter drift apart?\n{}",
            String::from_utf8_lossy(&out.stdout),
        );
        return;
    }

    let r = Repo::init();
    r.write("a.rs", "alpha\n");
    r.commit_all("init");
    r.write("a.rs", "alpha\nbeta\n");

    let fake_dir = PathBuf::from(env::var("FAKE_HERDR_DIR").expect("set by the parent run"));
    let keymap = Keymap::default();
    let area = Rect::new(0, 0, 80, 24);
    let mut app = app_on(&r);

    // Several agents: `s` opens the picker over both rows, labelled from `tab list`, and with
    // nothing sent yet the highlight arms on the first row (`specs/herdr-host.md`).
    fs::write(fake_dir.join("agents.json"), TWO_AGENTS).unwrap();
    write_comment(&mut app, "one");
    press(&mut app, KeyCode::Char('s'), area, &keymap);
    assert_eq!(app.mode, Mode::Picker, "several agents open the picker");
    assert_eq!(app.picker_rows.len(), 2);
    assert_eq!(app.picker_rows[0].tab, "Grip", "the tab label joins on tab_id");
    assert_eq!(app.picker_cursor, 0, "nothing sent this session arms the first row");

    // A chosen pane that closed while the picker was open fails the send, and every comment
    // stays. Nothing arms, since nothing was delivered (specs/herdr-host.md).
    fail_on(&fake_dir, "pane send-text w8:p1");
    press(&mut app, KeyCode::Enter, area, &keymap);
    assert_eq!(app.mode, Mode::Normal, "the picker closes whatever the outcome");
    assert_eq!(app.store.len(), 1, "a failed send keeps every comment");
    // One short sentence a reviewer can read. herdr's own wording is a JSON envelope around a
    // pane id, and the argv it came from carries the whole review in its last argument — both
    // would fill a 40-column footer without naming anything (`specs/herdr-host.md`).
    assert_eq!(app.status, "Agent not found — 1 comments kept");
    assert_eq!(app.last_sent_pane, None, "a failed send arms nothing");
    fail_on_nothing(&fake_dir);

    // One agent still opens the confirmation sheet. Only its explicit Enter writes directly to
    // Herdr's pane input and arms the agent.
    fs::write(fake_dir.join("agents.json"), ONE_AGENT).unwrap();
    press(&mut app, KeyCode::Char('s'), area, &keymap);
    assert_eq!(app.mode, Mode::Picker, "one agent requires confirmation");
    assert_eq!(app.store.len(), 1, "opening confirmation does not consume");
    press(&mut app, KeyCode::Enter, area, &keymap);
    assert_eq!(app.mode, Mode::Normal);
    assert!(app.store.is_empty(), "a successful send consumes the whole set");
    assert_eq!(app.status, "Added 1 comment to claude, not submitted");
    assert_eq!(app.last_sent_pane.as_deref(), Some("w8:p1"));

    // `enter` sends to the digit-selected agent and consumes the set.
    fs::write(fake_dir.join("agents.json"), TWO_AGENTS).unwrap();
    write_comment(&mut app, "two");
    press(&mut app, KeyCode::Char('s'), area, &keymap);
    press(&mut app, KeyCode::Char('2'), area, &keymap);
    press(&mut app, KeyCode::Enter, area, &keymap);
    assert_eq!(app.mode, Mode::Normal);
    assert!(app.store.is_empty(), "a successful send consumes the whole set");
    assert_eq!(app.status, "Added 1 comment to codex, not submitted");
    assert_eq!(app.last_sent_pane.as_deref(), Some("w8:p2"));
    assert!(log(&fake_dir).contains("pane send-text w8:p2"), "log: {}", log(&fake_dir));
    // The start marker opens the payload at the CLI boundary; `pasted()` owns the rationale.
    assert!(
        log(&fake_dir).contains("pane send-text w8:p2 \u{1b}[200~"),
        "the send is framed as a bracketed paste: {}",
        log(&fake_dir)
    );
    // The batch's last bytes are the comment text "two", so this pins the terminator to the
    // end of a delivered payload.
    assert!(
        log(&fake_dir).contains("two\u{1b}[201~"),
        "the frame terminator closes the batch: {}",
        log(&fake_dir)
    );
    assert!(log(&fake_dir).contains("agent focus w8:p2"), "a send focuses its pane");

    // Several again: the last-sent agent outranks the first row, and a first click on that
    // armed row sends immediately (specs/input.md).
    write_comment(&mut app, "three");
    press(&mut app, KeyCode::Char('s'), area, &keymap);
    assert_eq!(app.mode, Mode::Picker);
    assert_eq!(app.picker_cursor, 1, "the last-sent agent outranks the first row");
    let (col, row) = (0..area.height)
        .flat_map(|y| (0..area.width).map(move |x| (x, y)))
        .find(|&(x, y)| ui::hit_picker_row(area, &app, x, y) == Some(1))
        .expect("the armed row is clickable");
    handle_mouse(
        &mut app,
        MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: col,
            row,
            modifiers: KeyModifiers::NONE,
        },
        area,
        &[],
        &keymap,
    )
    .unwrap();
    assert_eq!(app.mode, Mode::Normal, "a first click on the armed row sends");
    assert!(app.store.is_empty());
    let sends = log(&fake_dir).matches("pane send-text w8:p2").count();
    assert_eq!(
        sends,
        2,
        "the digit-selected send and the armed-row click addressed the same pane: {}",
        log(&fake_dir)
    );

    // No agent and unavailable Herdr both keep drafts in a cancellable confirmation sheet.
    fs::write(fake_dir.join("agents.json"), r#"{"result":{"agents":[]}}"#).unwrap();
    write_comment(&mut app, "four");
    press(&mut app, KeyCode::Char('s'), area, &keymap);
    assert_eq!(app.mode, Mode::Picker, "an empty workspace explains retained drafts in the sheet");
    assert_eq!(app.store.len(), 1, "a refusal keeps every comment");
    assert_eq!(app.picker_notice.as_deref(), Some("No agent in this workspace — comments kept"));
    press(&mut app, KeyCode::Esc, area, &keymap);

    fail_on(&fake_dir, "agent list");
    press(&mut app, KeyCode::Char('s'), area, &keymap);
    assert_eq!(
        app.mode,
        Mode::Picker,
        "a failed enumeration still opens the retained-drafts sheet"
    );
    assert_eq!(app.store.len(), 1, "a refusal keeps every comment");
    assert_eq!(app.picker_notice.as_deref(), Some("Herdr unavailable — comments kept"));
}

/// An in-memory clipboard seam for picker-fallback dispatch tests. It never invokes a platform
/// clipboard program, and records the payload only after the key reaches the fallback path.
struct TestClipboard {
    ok: bool,
    captured: std::cell::RefCell<Vec<String>>,
}

impl TestClipboard {
    fn succeeding() -> Self {
        Self { ok: true, captured: std::cell::RefCell::new(Vec::new()) }
    }

    fn failing() -> Self {
        Self { ok: false, captured: std::cell::RefCell::new(Vec::new()) }
    }
}

impl herdr_reviewr::export::ExportTarget for TestClipboard {
    fn label(&self) -> &'static str {
        "clipboard"
    }

    fn success_message(&self, count: usize) -> String {
        format!("copied {count} comment{}", if count == 1 { "" } else { "s" })
    }

    fn failure_message(&self) -> String {
        "clipboard failed".to_string()
    }

    fn export(&self, text: &str) -> anyhow::Result<()> {
        self.captured.borrow_mut().push(text.to_string());
        if self.ok { Ok(()) } else { anyhow::bail!("test clipboard failure") }
    }
}

#[test]
fn no_target_and_unavailable_picker_copy_uses_only_the_clipboard_seam() {
    if env::var("PICKER_COPY_CHILD").is_err() {
        let staging = tempfile::TempDir::new().expect("tempdir");
        let script = write_fake_herdr(staging.path());
        let out = std::process::Command::new(env::current_exe().unwrap())
            .args([
                "--exact",
                "no_target_and_unavailable_picker_copy_uses_only_the_clipboard_seam",
                "--nocapture",
            ])
            .env("PICKER_COPY_CHILD", "1")
            .env("FAKE_HERDR_DIR", staging.path())
            .env("HERDR_BIN_PATH", &script)
            .env("HERDR_WORKSPACE_ID", "w8")
            .env("HERDR_PANE_ID", "w8:p9")
            .output()
            .expect("re-exec the test with the fake herdr env");
        assert!(
            out.status.success(),
            "child run failed:\n{}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr),
        );
        assert!(
            !log(staging.path()).contains("pane send-text"),
            "picker fallback must never address an agent: {}",
            log(staging.path())
        );
        return;
    }

    let r = Repo::init();
    r.write("a.rs", "alpha\n");
    r.commit_all("init");
    r.write("a.rs", "alpha\nbeta\n");
    let fake_dir = PathBuf::from(env::var("FAKE_HERDR_DIR").expect("set by the parent run"));
    let keymap = Keymap::resolve(&[(Action::Copy, vec![Key::plain('x')])]).unwrap();
    let area = Rect::new(0, 0, 80, 24);
    let mut app = app_on(&r);

    // No agent: only the resolved copy key reaches the injected clipboard, whose confirmed
    // success consumes drafts. The old default stays inert in the notice picker.
    fs::write(fake_dir.join("agents.json"), r#"{"result":{"agents":[]}}"#).unwrap();
    write_comment(&mut app, "no-target success");
    press(&mut app, KeyCode::Char('s'), area, &keymap);
    assert_eq!(app.mode, Mode::Picker);
    let copied = TestClipboard::succeeding();
    herdr_reviewr::handle_key_with_clipboard(
        &mut app,
        KeyEvent::from(KeyCode::Char('y')),
        area,
        &keymap,
        &copied,
    )
    .unwrap();
    assert_eq!(app.mode, Mode::Picker, "the old copy default is inert in a notice picker");
    assert!(app.store.len() == 1 && copied.captured.borrow().is_empty());
    herdr_reviewr::handle_key_with_clipboard(
        &mut app,
        KeyEvent::from(KeyCode::Char('x')),
        area,
        &keymap,
        &copied,
    )
    .unwrap();
    assert_eq!(app.mode, Mode::Normal);
    assert!(app.store.is_empty(), "only a confirmed clipboard success consumes drafts");
    assert_eq!(copied.captured.borrow().len(), 1, "the fallback handed the review to clipboard");

    // A clipboard error closes the notice but retains every draft for retry.
    write_comment(&mut app, "no-target failure");
    press(&mut app, KeyCode::Char('s'), area, &keymap);
    let failed = TestClipboard::failing();
    herdr_reviewr::handle_key_with_clipboard(
        &mut app,
        KeyEvent::from(KeyCode::Char('x')),
        area,
        &keymap,
        &failed,
    )
    .unwrap();
    assert_eq!(app.mode, Mode::Normal);
    assert_eq!(app.store.len(), 1, "a failed clipboard export retains drafts");

    // Cancel is equally non-destructive for the no-target notice.
    press(&mut app, KeyCode::Char('s'), area, &keymap);
    press(&mut app, KeyCode::Esc, area, &keymap);
    assert_eq!(app.store.len(), 1, "cancelling a no-target picker retains drafts");

    // A failed Herdr enumeration has the same clipboard-only fallback behavior.
    fail_on(&fake_dir, "agent list");
    press(&mut app, KeyCode::Char('s'), area, &keymap);
    assert_eq!(app.picker_notice.as_deref(), Some("Herdr unavailable — comments kept"));
    let unavailable_copied = TestClipboard::succeeding();
    herdr_reviewr::handle_key_with_clipboard(
        &mut app,
        KeyEvent::from(KeyCode::Char('x')),
        area,
        &keymap,
        &unavailable_copied,
    )
    .unwrap();
    assert!(app.store.is_empty(), "the unavailable fallback consumes only after clipboard success");

    write_comment(&mut app, "unavailable cancel");
    press(&mut app, KeyCode::Char('s'), area, &keymap);
    press(&mut app, KeyCode::Esc, area, &keymap);
    assert_eq!(app.store.len(), 1, "cancelling an unavailable picker retains drafts");
}

/// Assignment is a non-consuming, per-comment delivery. Drive the real key dispatch and the
/// fake Herdr command seam so the test proves both the selected tab and the absence of submit.
#[test]
fn assignment_delivers_one_comment_without_consuming_or_submitting() {
    if env::var("ASSIGNMENT_FLOW_CHILD").is_err() {
        let staging = tempfile::TempDir::new().expect("tempdir");
        let script = write_fake_herdr(staging.path());
        let out = std::process::Command::new(env::current_exe().unwrap())
            .args([
                "--exact",
                "assignment_delivers_one_comment_without_consuming_or_submitting",
                "--nocapture",
            ])
            .env("ASSIGNMENT_FLOW_CHILD", "1")
            .env("FAKE_HERDR_DIR", staging.path())
            .env("HERDR_BIN_PATH", &script)
            .env("HERDR_WORKSPACE_ID", "w8")
            .env("HERDR_PANE_ID", "w8:p9")
            .output()
            .expect("re-exec assignment test with fake herdr");
        assert!(
            out.status.success(),
            "child run failed:\n{}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(log(staging.path()).contains("pane send-text"), "assignment child sent nothing");
        return;
    }

    let r = Repo::init();
    r.write("a.rs", "alpha\n");
    r.commit_all("init");
    r.write("a.rs", "alpha\nbeta\n");
    let fake_dir = PathBuf::from(env::var("FAKE_HERDR_DIR").unwrap());
    fs::write(fake_dir.join("agents.json"), TWO_AGENTS).unwrap();
    let keymap = Keymap::default();
    let area = Rect::new(0, 0, 80, 24);
    let mut app = app_on(&r);
    write_comment(&mut app, "fix the beta regression");
    let id = app.store.id_at(0).expect("comment id");

    // A normal-mode `a` must not act on an off-screen diff cursor while Files owns focus.
    app.focus = Focus::Files;
    press(&mut app, KeyCode::Char('a'), area, &keymap);
    assert_eq!(app.mode, Mode::Normal);
    assert!(log(&fake_dir).is_empty(), "unfocused a must not enumerate or send an agent");

    // A diff cursor that has no attached comment must refuse without asking Herdr for agents.
    app.focus = Focus::Diff;
    app.comment_focus = None;
    app.diff_cursor = app.visible.iter().position(|r| r.marker() != '+').unwrap();
    press(&mut app, KeyCode::Char('a'), area, &keymap);
    assert_eq!(app.mode, Mode::Normal);
    assert_eq!(app.status, "select a comment to assign");
    assert!(log(&fake_dir).is_empty(), "a cursor without a comment must not enumerate agents");

    // Image and Files-only views have no assignable review anchor, even if a local draft exists.
    app.diff_cursor = app.visible.iter().position(|r| r.marker() == '+').unwrap();
    app.image_preview_note = Some("SVG preview unavailable");
    press(&mut app, KeyCode::Char('a'), area, &keymap);
    assert_eq!(app.mode, Mode::Normal);
    assert_eq!(app.status, "select a comment to assign");
    assert!(log(&fake_dir).is_empty(), "image assignment must not enumerate agents");
    app.image_preview_note = None;
    app.repository_mode = RepositoryMode::FilesOnly;
    press(&mut app, KeyCode::Char('a'), area, &keymap);
    assert_eq!(app.mode, Mode::Normal);
    assert!(log(&fake_dir).is_empty(), "Files-only assignment must not enumerate agents");
    app.repository_mode = RepositoryMode::GitReview;

    // Zero agents and an unavailable Herdr leave the comment untouched and open no modal.
    fs::write(fake_dir.join("agents.json"), r#"{"result":{"agents":[]}}"#).unwrap();
    press(&mut app, KeyCode::Char('a'), area, &keymap);
    assert_eq!(app.mode, Mode::Normal);
    assert!(app.status.starts_with("agent assignment unavailable:"));
    assert_eq!(app.store.get(id).unwrap().assignment, None);
    fail_on(&fake_dir, "agent list");
    press(&mut app, KeyCode::Char('a'), area, &keymap);
    assert_eq!(app.mode, Mode::Normal);
    assert!(app.status.starts_with("agent assignment unavailable:"));
    assert_eq!(app.store.get(id).unwrap().assignment, None);
    fail_on_nothing(&fake_dir);
    fs::write(fake_dir.join("agents.json"), TWO_AGENTS).unwrap();

    press(&mut app, KeyCode::Char('a'), area, &keymap);
    assert!(matches!(app.mode, Mode::AssignPicker { id: picked } if picked == id));
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
    terminal.draw(|frame| ui::render(frame, &app)).unwrap();
    let painted = terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(ratatui::buffer::Cell::symbol)
        .collect::<String>();
    assert!(painted.contains("Enter assign"), "{painted}");
    assert!(!painted.contains("Enter send"), "{painted}");
    press(&mut app, KeyCode::Down, area, &keymap);
    assert_eq!(app.picker_cursor, 1, "arrow keys move the assignment highlight");
    press(&mut app, KeyCode::Char('2'), area, &keymap);
    assert_eq!(app.picker_cursor, 1, "digit picks the exact second agent");
    press(&mut app, KeyCode::Enter, area, &keymap);

    assert_eq!(app.mode, Mode::Normal);
    assert_eq!(app.store.len(), 1, "assignment never consumes its comment");
    assert_eq!(
        app.store.get(id).unwrap().assignment,
        Some(DeliveryReceipt::Delivered { agent: "codex".into(), tab: "Grip".into() })
    );
    let sent = log(&fake_dir);
    assert!(sent.contains("pane send-text w8:p2"), "assignment addressed selected tab: {sent}");
    assert!(
        sent.contains("\u{1b}[200~"),
        "assignment must use bracketed-paste start framing: {sent:?}"
    );
    assert!(
        sent.contains("\u{1b}[201~"),
        "assignment must use bracketed-paste end framing: {sent:?}"
    );
    assert!(sent.contains("## Review task from Herdr Preview"));
    assert!(
        sent.contains("**Target:** a.rs:2"),
        "task envelope needs the anchored location: {sent}"
    );
    assert!(sent.contains("**Review note:** fix the beta regression"));
    assert!(
        sent.contains("```diff\n+beta\n```"),
        "task envelope needs the fenced authoritative diff: {sent}"
    );
    assert!(
        sent.contains("Please inspect this exact code, implement a fix if appropriate, and report validation. Do not submit a GitHub review on my behalf."),
        "task envelope needs its non-publishing closing instruction: {sent}"
    );
    assert!(!sent.contains("send-keys"), "assignment must write text, never submit a key: {sent}");

    // A failed re-delivery remains attached to the same local comment and updates its receipt.
    fail_on(&fake_dir, "pane send-text w8:p2");
    press(&mut app, KeyCode::Char('a'), area, &keymap);
    press(&mut app, KeyCode::Char('2'), area, &keymap);
    press(&mut app, KeyCode::Enter, area, &keymap);
    assert_eq!(app.store.len(), 1, "a failed assignment also keeps its comment");
    assert_eq!(
        app.store.get(id).unwrap().assignment,
        Some(DeliveryReceipt::Failed { agent: "codex".into() })
    );

    // The comments list is the sole non-diff `a` exception. Its chosen card becomes the stable
    // target, and click-to-pick follows the same picker contract as the keyboard.
    fail_on_nothing(&fake_dir);
    app.open_list();
    press(&mut app, KeyCode::Char('a'), area, &keymap);
    assert!(matches!(app.mode, Mode::AssignPicker { id: picked } if picked == id));
    let (col, row) = (0..area.height)
        .flat_map(|y| (0..area.width).map(move |x| (x, y)))
        .find(|&(x, y)| ui::hit_picker_row(area, &app, x, y) == Some(0))
        .expect("assignment picker row is clickable");
    handle_mouse(
        &mut app,
        MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: col,
            row,
            modifiers: KeyModifiers::NONE,
        },
        area,
        &[],
        &keymap,
    )
    .unwrap();
    assert_eq!(app.mode, Mode::List, "delivery returns to its comments-list origin");
    assert_eq!(app.store.len(), 1, "list-origin assignment remains non-consuming");
    assert_eq!(
        app.store.get(id).unwrap().assignment,
        Some(DeliveryReceipt::Delivered { agent: "claude".into(), tab: "Grip".into() })
    );
}

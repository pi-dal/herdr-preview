//! Application state and transitions for the Changes review TUI.
//!
//! See `specs/tui.md` and `specs/review-model.md`. This module is terminal-free:
//! every method is a pure state transition or a read-only git/export call, so the
//! whole interaction model is testable without a backend. `src/main.rs` owns the
//! terminal and maps input events onto these methods.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::PathBuf;
use std::process::Command;

use anyhow::Result;

use crate::diff::{DiffCache, FileDiff, Row, View};
use crate::export::{Agent, ExportTarget, format_all};
use crate::file_list::{self, Annotation, Entry, RowKind};
use crate::forge;
use crate::git;
use crate::herdr::{self, AgentChoice, SendTarget};
use crate::highlight::Highlighter;
use crate::image_preview::{self, ImagePreview, ImagePreviewError};
use crate::logln;
use crate::model::{Comment, CommentId, CommentStore, DeliveryReceipt, GitHubReceipt, Scope, Side};
use crate::theme::{self, Palette};

/// Navigator shares and bounds, as percentages of the body's split axis.
const DEFAULT_SIDE_PCT: u16 = 32;
const DEFAULT_STACK_PCT: u16 = 25;
const MIN_NAVIGATOR_PCT: u16 = 15;
const MAX_SIDE_PCT: u16 = 60;
const MAX_STACK_PCT: u16 = 50;
/// The search screen's results-pane share: half the body by default, dragged within
/// wide bounds — the geometry's minimum pane sizes clamp the rest (specs/search.md).
const DEFAULT_SEARCH_PCT: u16 = 50;
const MIN_SEARCH_PCT: u16 = 10;
const MAX_SEARCH_PCT: u16 = 90;
/// Bounded one-level Files-only directory listings per world job.
const RAW_DIR_BATCH_CAP: usize = 32;
const RAW_DIR_STATUS_LIMIT: usize = 48;

fn raw_dir_label(path: &str) -> String {
    if path.is_empty() {
        return "root".to_string();
    }
    let mut chars = path.chars();
    let label: String = chars.by_ref().take(RAW_DIR_STATUS_LIMIT).collect();
    if chars.next().is_some() { format!("{label}…") } else { label }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum DividerDrag {
    #[default]
    Idle,
    Active {
        position: crate::config::NavigatorPosition,
    },
    Cancelled,
}

/// Which pane has the keyboard.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Focus {
    Files,
    Diff,
}

/// What the file-list cursor points at, by path, so it can be restored to the same target
/// after the tree rebuilds on a poll.
enum Anchor {
    File(String),
    Dir(String),
}

/// Which top-level tab is active: the changes reviewer, the whole-repo browser, or the
/// read-only PR mirror.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tab {
    Changes,
    AllFiles,
    Pr,
}

/// The domain the pane is rooted in. Git review owns diffs, scopes, PRs, comments, and
/// agent export. Files-only owns exactly a readable directory and never substitutes another
/// pane or repository for it (`specs/overview.md`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RepositoryMode {
    GitReview,
    FilesOnly,
}

/// What a pending PR refresh may do to a fetch already in flight: an ambient trigger —
/// tab entry, a turn end, the fallback timer — rides it, the user's `refresh` key
/// supersedes it (specs/forge-host.md). `Ord` so merging pending requests keeps the
/// stronger kind.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum RefreshKind {
    Ambient,
    Forced,
}

impl Tab {
    /// Whether this tab uses the file-tree / diff machinery (and so the per-tab stash). The
    /// `PR` tab does not — it holds its own state and never swaps into the diff fields.
    pub(crate) fn is_file_tab(self) -> bool {
        matches!(self, Tab::Changes | Tab::AllFiles)
    }
}

/// The inactive tab's saved navigator and read-pane state, swapped in on a tab switch so
/// each tab keeps its own selection and scroll (specs/tui.md).
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Default)]
struct TabStash {
    entries: Vec<Entry>,
    raw_tree: RawTree,
    file_rows: Vec<file_list::Row>,
    file_cursor: usize,
    file_scroll: usize,
    toggled_dirs: HashSet<String>,
    diff: FileDiff,
    visible: Vec<Row>,
    expanded_folds: HashSet<u32>,
    diff_path: Option<String>,
    diff_cursor: usize,
    diff_scroll: usize,
    h_scroll: usize,
    select_anchor: Option<usize>,
    hide_unchanged: bool,
    preview: bool,
    preview_scroll: usize,
    preview_scrolled: bool,
    preview_text: String,
    /// Raster/fallback image presentation belongs to the file-tab identity too: an image from
    /// another tab must never paint while this tab waits for a refresh (Continuity).
    image_preview: Option<ImagePreview>,
    image_preview_note: Option<&'static str>,
    /// Whether this tab has ever completed a reload. A never-visited tab has nothing worth
    /// painting, so its first entry loads before the frame instead of deferring.
    visited: bool,
}

/// The Files-only tree's per-tab cache. Each listing contains direct children only. The app
/// materializes entries from reachable expanded listings, keeping collapsed or orphaned cache
/// data out of the navigator (specs/file-list.md).
#[derive(Debug, Default)]
struct RawTree {
    listings: BTreeMap<String, Vec<Entry>>,
    loading: HashSet<String>,
    failed: HashMap<String, String>,
    epoch: u64,
}

/// A file crossing offered by the footer, waiting for the hunk step that armed it to repeat: the
/// direction it crosses in, and the file it resolved to open. Holding the file spares the second
/// press the walk the first one already paid for (specs/input.md).
#[derive(Clone, Debug)]
struct ArmedCross {
    forward: bool,
    path: String,
}

/// The base picker's state while it is open (`specs/input.md` Base picker). The rows freeze
/// at open; the filter and highlight are the reviewer's own place state.
#[derive(Clone, Debug)]
pub struct BasePicker {
    /// Every pickable branch name: the open PR's target starred first, the default branch
    /// next, the rest by commit recency, the checked-out branch excluded.
    pub rows: Vec<BaseChoice>,
    /// The highlighted row, an index into the filtered view.
    pub cursor: usize,
    /// The typed filter, matching anywhere in the name.
    pub query: String,
    /// The caret in `query`, a char index — the filter edits with the comment editor's
    /// controls, like every other text field (`specs/input.md`).
    pub caret: usize,
}

/// One base picker row (`specs/input.md` Base picker).
#[derive(Clone, Debug)]
pub struct BaseChoice {
    pub name: String,
    /// The open PR's target, shown starred.
    pub starred: bool,
    /// The default branch, marked `default`. Choosing it clears the pick
    /// (`specs/review-model.md`).
    pub is_default: bool,
}

impl BasePicker {
    /// The filtered view: indices into `rows` whose name contains the query, matched
    /// case-insensitively and anywhere in the name (`specs/input.md` Base picker).
    pub fn filtered(&self) -> Vec<usize> {
        let q = self.query.to_lowercase();
        (0..self.rows.len()).filter(|&i| self.rows[i].name.to_lowercase().contains(&q)).collect()
    }
}

/// A stable in-memory identity for one remote finding selected from the current PR snapshot.
/// It intentionally retains raw forge values; only its display copy is sanitized by the UI.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct RemoteThread {
    pub url: String,
    pub author: String,
    pub body: String,
    pub anchor: String,
    pub snippet: Option<String>,
    pub created_at: String,
}

impl RemoteThread {
    fn from_comment(comment: &forge::Comment) -> Self {
        Self {
            url: comment.url.clone(),
            author: comment.author.clone(),
            body: comment.body.clone(),
            anchor: comment.anchor.clone(),
            snippet: comment.snippet.clone(),
            created_at: comment.created_at.clone(),
        }
    }

    /// GitHub's direct review-comment URL is the immutable identity supplied by the provider.
    /// Location, author, and timestamps are presentation data that can change after a force-push.
    fn identity(&self) -> String {
        self.url.clone()
    }
}

/// Local delivery metadata for a remote thread. This never mutates forge state.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum RemoteThreadReceipt {
    Delivered { agent: String, tab: String },
    Failed { agent: String },
}

/// The interaction mode the UI is in.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Mode {
    Normal,
    /// Writing a comment; `editing` is a stable session comment ID when editing an existing one.
    Composing {
        editing: Option<CommentId>,
    },
    /// A deliberate, exact-comment delete confirmation.
    ConfirmDelete {
        id: CommentId,
    },
    /// Explicitly confirm publishing one eligible local comment into a GitHub pending review.
    ConfirmPublish {
        id: CommentId,
    },
    /// Explicitly submit this pane session's exact GitHub pending review.
    SubmitReview {
        key: (String, String, String, u64, String),
        event: forge::ReviewEvent,
    },
    /// Browsing the comments-list overlay.
    List,
    /// Choosing which agent a `Send` goes to (`specs/herdr-host.md`). Its rows and highlight
    /// live in [`App::picker_rows`] and [`App::picker_cursor`].
    Picker,
    /// Choosing an agent for one exact comment.
    AssignPicker {
        id: CommentId,
    },
    /// Choosing a Herdr agent for the exact remote GitHub finding selected in the PR tab.
    RemoteAssignPicker {
        thread: RemoteThread,
    },
    /// A final explicit confirmation before a remote-thread task is pasted into an agent tab.
    ConfirmRemoteAssign {
        thread: RemoteThread,
        agent: AgentChoice,
    },
    /// Choosing the `branch` scope's base (`specs/input.md` Base picker). Its state lives in
    /// [`App::base_picker`].
    BasePick,
    /// The search screen, replacing the body from any tab (specs/search.md). Its state
    /// lives in [`App::search`].
    Search,
    /// The in-file find band over the read pane (specs/find-in-file.md). Its state lives in
    /// [`App::find`].
    Find,
}

impl Mode {
    /// Whether this mode is a modal hold: the reviewer is mid-gesture over the body, with keys
    /// and a mouse of its own. A modal freezes the open diff, so the world can never move the
    /// anchor, the scroll, or the selection out from under the gesture (`specs/overview.md`
    /// Continuity), and it captures the mouse so no click reaches the view behind
    /// (`specs/input.md`).
    ///
    /// `Search` replaces the body rather than holding a place in it, and `Find` is a band the
    /// reviewer navigates the live diff with. Neither freezes anything, so neither is modal here.
    pub fn is_modal(&self) -> bool {
        matches!(
            self,
            Mode::Composing { .. }
                | Mode::ConfirmDelete { .. }
                | Mode::ConfirmPublish { .. }
                | Mode::SubmitReview { .. }
                | Mode::List
                | Mode::Picker
                | Mode::AssignPicker { .. }
                | Mode::RemoteAssignPicker { .. }
                | Mode::ConfirmRemoteAssign { .. }
                | Mode::BasePick
        )
    }
}

/// The search screen's mode: which result set the list shows (specs/search.md).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SearchMode {
    /// The engine's path matches, one row per file.
    Files,
    /// The engine's content matches, grouped by file.
    Code,
}

/// Where the search overlay stands with the engine (specs/search.md).
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum SearchPhase {
    /// The engine's first scan is still running: the overlay shows `indexing…`.
    Indexing,
    /// Results are painted; stale ones stay up while a newer query is in flight.
    Ready,
    /// The engine failed; the message shows inside the overlay.
    Error(String),
}

/// The picked result's file rendered as the read pane's File view, hit-centered
/// (specs/search.md Preview).
#[derive(Debug)]
pub struct SearchPreview {
    pub path: String,
    pub diff: crate::diff::FileDiff,
    /// A `Code` pick's hit: the 1-based line and its matched byte spans, banded and
    /// emphasized by the renderer. A `Files` pick previews from the top.
    pub hit: Option<(u64, Vec<(u32, u32)>)>,
    /// Top visible row. The renderer centers the hit here once per build, then
    /// `PageUp`/`PageDown` move it freely.
    pub scroll: std::cell::Cell<usize>,
    /// Cleared by the renderer after it centers the hit for this build.
    pub center: std::cell::Cell<bool>,
}

/// The search screen's state: the query as typed, the mode, the pick, the last landed
/// results, and the settled preview. Dropped whole on close — a query is cheap, unlike a
/// comment draft.
#[derive(Debug)]
pub struct SearchOverlay {
    pub query: String,
    /// The caret into `query`: a char index, edited by the shared caret ops (`input.md`).
    pub caret: usize,
    pub search_mode: SearchMode,
    /// The picked row, indexed into the active mode's result set.
    pub pick: usize,
    /// Top visible result row, kept by the renderer so the pick stays in view.
    pub scroll: std::cell::Cell<usize>,
    pub results: crate::search::SearchResults,
    pub phase: SearchPhase,
    /// The settled preview of the picked result. `None` until the first build, or while
    /// nothing is pickable. The event loop rebuilds it once input settles, whenever it no
    /// longer matches the pick — a sweep never waits on a build (specs/search.md).
    pub preview: Option<SearchPreview>,
}

impl SearchOverlay {
    fn new() -> Self {
        Self {
            query: String::new(),
            caret: 0,
            search_mode: SearchMode::Files,
            pick: 0,
            scroll: std::cell::Cell::new(0),
            results: crate::search::SearchResults::default(),
            phase: SearchPhase::Indexing,
            preview: None,
        }
    }

    /// How many rows the pick can land on in the active mode.
    pub fn picks(&self) -> usize {
        match self.search_mode {
            SearchMode::Files => self.results.files.len(),
            SearchMode::Code => self.results.code.len(),
        }
    }

    /// The picked result in the active mode.
    pub fn picked(&self) -> Option<PickedResult<'_>> {
        match self.search_mode {
            SearchMode::Files => self.results.files.get(self.pick).map(PickedResult::File),
            SearchMode::Code => self.results.code.get(self.pick).map(PickedResult::Code),
        }
    }
}

/// One picked search result, borrowed from the overlay's results.
#[derive(Debug)]
pub enum PickedResult<'a> {
    File(&'a crate::search::FileHit),
    Code(&'a crate::search::CodeHit),
}

/// The in-file find band's state while `mode == Mode::Find` (specs/find-in-file.md). The current
/// match is the read-pane cursor when its row matches, so only the query is stored — the matches,
/// count, and highlight all derive from the query against the open file each frame.
#[derive(Clone, Debug, Default)]
pub struct Find {
    pub query: String,
    /// The caret into `query`: a char index, edited by the shared caret ops (`input.md`).
    pub caret: usize,
}

/// A found match in file order: how the cursor moves onto it (specs/find-in-file.md).
enum FindHit {
    /// A visible row, at this `visible` index.
    Visible(usize),
    /// A row hidden in the collapsed fold `anchor`; `new_no` is its context line, unique within
    /// the fold, so it is found again once the fold expands.
    Folded { anchor: u32, new_no: u32 },
}

/// The char-index ranges of every non-overlapping occurrence of `query` in `text`, honoring
/// `case_sensitive` (pass [`find_case_sensitive`]'s result for smart-case). Char indices, so the
/// diff renderer overlays the highlight the same way it does word emphasis (specs/find-in-file.md).
pub fn find_match_ranges(text: &str, query: &str, case_sensitive: bool) -> Vec<(u32, u32)> {
    if query.is_empty() {
        return Vec::new();
    }
    let q: Vec<char> = query.chars().collect();
    let eq = |a: char, b: char| if case_sensitive { a == b } else { a.eq_ignore_ascii_case(&b) };
    let chars: Vec<char> = text.chars().collect();
    let mut ranges = Vec::new();
    let mut i = 0;
    while i + q.len() <= chars.len() {
        if (0..q.len()).all(|j| eq(chars[i + j], q[j])) {
            ranges.push((i as u32, (i + q.len()) as u32));
            i += q.len();
        } else {
            i += 1;
        }
    }
    ranges
}

/// Whether `query` is case-sensitive under smart-case: any uppercase character makes it so.
pub fn find_case_sensitive(query: &str) -> bool {
    query.chars().any(char::is_uppercase)
}

/// A footer action — what the bar offers for the current context. Semantic only: the renderer
/// maps each to its key glyph and label and styles it by [`Band`] (`specs/input.md`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FooterAction {
    Comment,
    Select,
    ClearSelection,
    EditComment,
    DeleteComment,
    JumpComment,
    ExpandFold,
    HideUnchanged,
    /// Take the armed crossing: the hunk step that armed it leaves the file when pressed again.
    /// The direction names the destination and picks the key (`] next file`, `[ prev file`).
    CrossFile {
        forward: bool,
    },
    /// The `move` band's cursor-movement pairs, each rendered as its two keys (`specs/input.md`).
    /// `MovePage` names the fixed page keys, which are not rebindable.
    MoveLine,
    MoveHunk,
    MoveChange,
    MoveFile,
    MovePage,
    ExpandDir,
    CollapseDir,
    /// Open the search screen — offered in every context, on every tab (specs/search.md).
    Search,
    /// Open the in-file find band — offered wherever the read pane has content
    /// (specs/find-in-file.md).
    Find,
    /// The search screen's own bar: flip, pick, open, close (specs/search.md). The flip
    /// label names the destination mode, derived from the current mode at render time.
    FlipSearchMode,
    PickResult,
    OpenResult,
    CloseSearch,
    /// The find band's own bar: step between matches, and close (specs/find-in-file.md).
    FindStep,
    CloseFind,
    /// Switch focus between the file list and the diff; the label names the destination pane.
    TogglePane,
    /// Toggle the markdown preview; the label names the destination view (`m preview`
    /// on source, `m source` in the preview).
    Preview,
    NavigatorPosition,
    /// Hide the navigator or show it back; the label names the direction (`z hide` / `z show`).
    /// Visible, it waits in the `go` band; hidden, it joins row 1 (specs/input.md).
    NavigatorHide,
    Wrap,
    Scope,
    Send,
    /// Publish one eligible local comment to Preview's pending GitHub review.
    Publish,
    /// Confirm the selected event and submit Preview's pending GitHub review.
    SubmitReview,
    /// Assign the selected remote GitHub finding to a Herdr coding agent.
    AssignRemote,
    List,
    Copy,
    Save,
    Newline,
    Cancel,
    CloseList,
    /// The agent picker's own bar: send to the highlight, move it, and cancel
    /// (`specs/input.md`). The digits are literal here, so the move hint names them.
    PickAgent,
    MovePickerRow,
    ClosePicker,
    /// Open the base picker (`specs/input.md` Base picker).
    BasePick,
    /// The base picker's own bar: pick the highlight and move it. Every printable is
    /// filter text there, so the move hint names the arrows alone.
    PickBaseRow,
    MoveBaseRow,
    /// The two scopes to switch away to, from the branch-scope no-base row — `b` is the
    /// scope already showing, so its hint would offer a no-op (`specs/input.md`).
    ScopeOther,
    OpenPr,
    Refresh,
    Tabs,
    Quit,
}

/// Where a footer action sits: on row 1 (`Primary`, `Send`, `Submit`, or a `Do` cursor action),
/// or in one of the `?`-expansion bands (`Do` overflow, `Go`, `Move`). Row 1 keeps the primary,
/// send actions, and the `?`, trimming trailing `Do` actions to fit and spilling them into the
/// `do` band (`specs/input.md`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Band {
    Primary,
    Send,
    /// Preview-owned GitHub review submission. Kept distinct from agent send so the expanded
    /// footer cannot hide `S` behind `s send`.
    Submit,
    Do,
    Go,
    Move,
}

/// The full state of the review session.
// The several bools (wrap, reveal_files, reveal_diff, should_quit, and refresh flags) are independent
// toggles, not a state machine in disguise, so the excessive-bools lint does not apply.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug)]
pub struct App {
    /// The exact Git top level in review mode, or the exact launch directory in Files-only mode.
    pub repo: PathBuf,
    pub repository_mode: RepositoryMode,
    pub base: Option<String>,
    /// The `branch` scope's base outcome, carried by the latest landed snapshot — the
    /// header names its winner (or the skip) and the diff builds against the winner's OID
    /// (`specs/review-model.md`).
    pub branch_base: git::BaseStatus,
    /// Bumped by each pick made in this pane, so an in-flight build that read the old pick
    /// fails the landing's input match instead of reverting the pick (`crate::world::WorldInput`).
    base_epoch: u64,
    pub scope: Scope,
    /// The active tab; it drives both panes and selects the per-tab state in play.
    pub tab: Tab,
    /// Which file tab (`Changes`/`AllFiles`) currently occupies the diff/file fields. Tracked
    /// apart from `tab` so the `PR` tab can be active while a file tab's state stays frozen in
    /// place, with the other file tab in the stash.
    active_file_tab: Tab,
    pub focus: Focus,
    /// The navigator's source for the active tab: changed files in `Changes`, the whole
    /// worktree in `All files`.
    pub entries: Vec<Entry>,
    /// The flattened directory tree over `entries` — the rows the navigator paints. The
    /// `file_cursor` indexes this, not `entries`.
    pub file_rows: Vec<file_list::Row>,
    pub file_cursor: usize,
    /// Top visible row of the file list, kept so `file_cursor` stays on screen when the
    /// changeset is taller than the pane.
    pub file_scroll: usize,
    /// Set by a navigation that moves `file_cursor`; consumed once per frame to scroll the
    /// cursor into view. The wheel never sets it, so wheel-scrolling moves the viewport alone.
    pub reveal_files: bool,
    /// Set by a navigation that moves `diff_cursor`; consumed once per frame to scroll the
    /// cursor into view. The wheel never sets it.
    pub reveal_diff: bool,
    /// The file crossing a hunk step armed when it found no further hunk in the open file. The
    /// next step the same way takes it, and any other input drops it (specs/input.md).
    armed_cross: Option<ArmedCross>,
    /// Whether the current compose was opened from the comments-list overlay, so finishing it
    /// returns there rather than dropping to the diff.
    resume_list: bool,
    /// Directory paths toggled away from the tab's resting state — collapsed in `Changes`
    /// (expanded by default), expanded in `All files` (collapsed by default). Keyed by path,
    /// so it survives a poll that rebuilds the tree.
    toggled_dirs: HashSet<String>,
    /// Cached direct Files-only directory listings, loading/error state, and stale-completion
    /// epoch. This is tab state just like `toggled_dirs`.
    raw_tree: RawTree,
    /// Retained descriptor authority for the selected Files-only root. `repo` remains display
    /// identity only in this mode; directory listings and file reads go through this capability.
    files_root: Option<crate::world::FilesRoot>,
    /// The inactive tab's saved state, swapped in on a tab switch.
    stash: TabStash,
    /// The active scope's changed files, keyed by repo-relative path and recomputed every
    /// reload regardless of tab. Keys back the header count and diff-comment staleness; values
    /// annotate `All files` entries with their marker and stats. Stays correct while `All
    /// files` lists the whole worktree.
    changed: HashMap<String, Annotation>,
    pub diff: FileDiff,
    /// The rows actually shown: `diff.rows` with each fold collapsed to a marker or
    /// expanded to its lines. The cursor, scroll, selection, and hit-testing index this.
    pub visible: Vec<Row>,
    /// Fold anchors (first-hidden-line numbers) currently expanded; survives a poll.
    expanded_folds: HashSet<u32>,
    /// The file the open diff belongs to — the diff title, frozen with the diff
    /// while composing even if `file_cursor` drifts as the file list updates.
    pub diff_path: Option<String>,
    pub diff_cursor: usize,
    /// Top visible diff line. Sticky: only moves to keep the cursor in view, so the
    /// diff does not jump on every cursor step and drag-selection stays stable.
    pub diff_scroll: usize,
    /// Horizontal scroll, in columns, applied to the diff when wrap is off.
    pub h_scroll: usize,
    /// Whether long diff lines wrap (default) or are scrolled horizontally.
    pub wrap: bool,
    /// Whether the markdown preview is open for the active file tab's file. Both file tabs
    /// render it; the flag is per file tab and resets on a file change (specs/diff-view.md).
    /// Only the armed toggle — `preview_active()` is the honest on-screen predicate.
    preview: bool,
    /// Top visible rendered line of the markdown preview, clamped to the rendered length.
    pub preview_scroll: usize,
    /// The open markdown file's current content — the preview's render input, refreshed by
    /// `set_diff` and `set_file_view` so no frame rebuilds it. Empty whenever the current
    /// content does not render as a preview: a non-markdown file, a notice, or an empty new
    /// side (a deleted or empty file). One half of the `previewable()` signal.
    preview_text: String,
    /// A bounded raster preview derived from current raw bytes. It is mutually exclusive with
    /// markdown preview and has no selectable/commentable source rows.
    pub image_preview: Option<ImagePreview>,
    /// Fixed, non-path-derived image failure copy. SVG is deliberately recognized but not
    /// rasterized in v1; all render paths stay ordinary Ratatui cells.
    pub image_preview_note: Option<&'static str>,
    /// The preview's maximum useful scroll (rendered lines minus the viewport), noted
    /// by the renderer each frame so [`Self::preview_scroll_by`] can clamp. `usize::MAX`
    /// until the first paint.
    preview_max_scroll: std::cell::Cell<usize>,
    /// Whether a scroll input moved the preview since entry — the exact-restore
    /// predicate; a refresh clamp never sets it (specs/diff-view.md).
    preview_scrolled: bool,
    /// The diff pane's inner width, noted each paint, so the toggle's position mapping
    /// renders at the width the pane will paint with.
    pane_width: std::cell::Cell<usize>,
    /// The link regions painted this frame — a click resolves against the painted
    /// frame (specs/markdown.md).
    painted_links: std::cell::RefCell<Vec<PaintedLink>>,
    /// The painted markdown body's heading anchors as `(slug, content line index)`,
    /// covering the whole body — an anchor click can jump past the viewport.
    painted_anchors: std::cell::RefCell<Vec<(String, usize)>>,
    /// The PR read pane's maximum useful scroll, noted the same way for
    /// [`Self::pr_scroll_read`].
    pr_read_max_scroll: std::cell::Cell<usize>,
    /// The global navigator placement and the separate shares remembered for each split axis.
    pub navigator_position: crate::config::NavigatorPosition,
    pub navigator_side_pct: u16,
    pub navigator_stack_pct: u16,
    /// The presence toggle over the navigator, one state across all tabs, never a position
    /// (specs/tui.md). A restart shows the navigator; recovery preserves this.
    pub navigator_hidden: bool,
    /// The search screen's results-pane share — search's own session value, separate
    /// from the review layout's shares (specs/search.md).
    pub search_pct: u16,
    divider_drag: DividerDrag,
    pub select_anchor: Option<usize>,
    /// Changes-only presentation preference. It projects context into expandable folds without
    /// removing source rows, comments, or find coverage (`specs/diff-view.md`).
    pub hide_unchanged: bool,
    pub store: CommentStore,
    /// The ordinal currently highlighted in the list. It is presentation-only: actions resolve
    /// `list_selected` by stable ID at the moment they execute.
    pub list_cursor: usize,
    /// Exact selected comment in the list or on a card. It never aliases a different card after
    /// a neighboring deletion.
    pub comment_focus: Option<CommentId>,
    /// First visible comments-list row. The renderer keeps the highlighted ordinal in this window.
    pub list_scroll: std::cell::Cell<usize>,
    /// The picker's rows, frozen at the moment it opened. A refresh behind it adds, drops,
    /// and reorders nothing (`specs/herdr-host.md`).
    pub picker_rows: Vec<AgentChoice>,
    pub picker_cursor: usize,
    /// A no-agent or unavailable-Herdr explanation shown by the send confirmation sheet.
    pub picker_notice: Option<String>,
    /// The mode the picker opened over — `Normal`, the comments list, or the find band —
    /// so closing it restores the view the reviewer sent from (`specs/input.md`).
    pub picker_over: Mode,
    /// The agent this session last sent to, which arms the picker's highlight. Only a
    /// successful send sets it (`specs/herdr-host.md`).
    pub last_sent_pane: Option<String>,
    /// Session-local delivery receipts for remote GitHub findings. These are deliberately
    /// separate from forge data and do not resolve, reply to, or otherwise write a thread.
    pub remote_thread_assignments: HashMap<String, RemoteThreadReceipt>,
    /// Preview-owned pending GitHub reviews, keyed by the complete PR identity for this pane
    /// session. Retaining every exact head key means an A→B→A force-push sequence reuses A's
    /// own pending review instead of creating a duplicate.
    pub pending_github_reviews:
        HashMap<(String, String, String, u64, String), forge::PendingReviewBinding>,
    /// Cached exact anchors that can be published to the currently cached, open GitHub PR.
    /// The renderer reads this immutable snapshot; refreshes rebuild it from each comment's
    /// own diff so a footer never advertises an action that the entry gate will refuse.
    github_publishable_comments: HashSet<CommentId>,
    /// Latest PR-probe target, retained only as an in-memory identity cache for the footer.
    /// Rendering must not probe Git or a forge; a successful PR/identity worker result refreshes
    /// this value and [`Self::github_submit_available`] together.
    github_submit_target: Option<(String, String, String)>,
    /// Whether the cached open PR has an exact session-owned pending-review binding. This is the
    /// render-time eligibility cache for `S`; write-time validation remains authoritative.
    github_submit_available: bool,
    /// The base picker's rows, filter, and highlight while `Mode::BasePick` is open
    /// (`specs/input.md` Base picker).
    pub base_picker: Option<BasePicker>,
    pub mode: Mode,
    pub input: String,
    /// The comment editor's caret: a char index into `input` (`0..=chars().count()`).
    pub caret: usize,
    pub status: String,
    /// Whether the footer's `?` shortcut list is expanded. Global place state, not tab-stashed: one
    /// toggle across every tab, moved only by `?` and `esc`, preserved through a poll and config
    /// recovery (`specs/input.md`, `overview.md` Continuity).
    pub keys_expanded: bool,
    pub should_quit: bool,
    /// The read-only `PR` tab's view of the pull request (`specs/forge-host.md`).
    pub pr: forge::PrView,
    /// The resolved repository target's forge, from the latest input probe. Display strings
    /// pick their noun and reference form from it (`specs/forge-providers.md`); a forge
    /// change always changes the target, which clears the PR before a mismatch could paint.
    pub pr_forge: crate::git::Forge,
    /// Persistent same-input fetch remedy shown without replacing the visible snapshot.
    pr_notice: Option<String>,
    /// A same-input refresh that crossed the loading-indicator delay.
    pr_refreshing: bool,
    /// The PR navigator's cursor over its rows (checks then comments).
    pub(crate) pr_cursor: usize,
    /// Top visible line of the PR read pane, reset when the selected comment changes.
    pub(crate) pr_read_scroll: usize,
    /// Top visible row of the PR navigator, independent of its selection.
    pr_nav_scroll: std::cell::Cell<usize>,
    /// The PR navigator's maximum useful scroll, noted by the renderer each frame.
    pr_nav_max_scroll: std::cell::Cell<usize>,
    /// A cursor move requests the smallest navigator scroll that reveals the selection.
    reveal_pr_nav: std::cell::Cell<bool>,
    /// The PR refresh awaiting dispatch, if any; the event loop services it after drawing, so
    /// a `loading` frame shows before the blocking CLI calls run.
    pub pr_pending: Option<RefreshKind>,
    /// The world refresh request awaiting dispatch, if any; the event loop hands it to
    /// the worker after the frame paints (specs/tui.md).
    pub world_request: Option<crate::world::WorldRequest>,
    /// The search overlay's state while `mode == Mode::Search`, `None` otherwise.
    pub search: Option<SearchOverlay>,
    /// Set by every query edit (and the open); the event loop dispatches the query to the
    /// search worker after the frame paints, tagged latest-wins (specs/search.md).
    pub search_dirty: bool,
    /// A picked path awaiting its frecency record; the event loop hands it to the worker.
    pub search_track: Option<String>,
    /// The in-file find band's state while `mode == Mode::Find`, `None` otherwise
    /// (specs/find-in-file.md).
    pub find: Option<Find>,
    /// Whether the tab-strip glyph paints this frame — maintained by the event loop's
    /// appear-delay and minimum-display clocks (specs/tui.md).
    pub refresh_indicator: bool,
    /// Set by `r`: the next refresh is commanded, so the glyph lights immediately
    /// instead of waiting out the ambient appear delay (specs/tui.md).
    pub refresh_commanded: bool,
    /// Whether the active file tab has ever completed a reload (stash counterpart:
    /// `TabStash::visited`). Gates the first-visit synchronous load in [`Self::set_tab`].
    tab_visited: bool,
    highlighter: Highlighter,
    /// The active palette every renderer paints from (`specs/theme.md`).
    palette: Palette,
    /// The active theme's name, so re-resolving to the same theme is a no-op.
    theme_name: &'static str,
    /// The `--theme` override name (highest precedence); `None` lets the config file decide.
    cli_theme_name: Option<String>,
    /// The plugin is either ready with one validated snapshot or wholly blocked on its error.
    config: PluginConfigState,
    /// The last theme name requested, so re-resolving the same name skips work and logging.
    requested_theme_name: Option<String>,
    cache: DiffCache,
    /// The one-slot markdown render memo behind the PR read pane and the file tabs'
    /// preview (`specs/markdown.md`). Interior-mutable so the renderer can fill it from
    /// `&App`; cleared with the diff cache on a theme switch.
    markdown_cache: std::cell::RefCell<crate::markdown::RenderCache>,
    /// The worker-owned turn baseline, mirrored from completions so the sync `last-turn`
    /// paths (the diff's old side, the scope-switch rebuild) read it without a round-trip.
    turn_baseline: Option<String>,
    /// Whether any agent is in this worktree — the one home for the answer, held here
    /// because this is what paints it. `None` until a sample observes it, so a frame that
    /// has seen nothing waits instead of asserting an emptiness nobody looked for: stale is
    /// allowed, wrong is not (`specs/overview.md` Continuity). Only a sample that observed the
    /// whole worktree moves it — herdr answered and git resolved every member's directory — so
    /// `Some(false)` always means someone looked and found no member.
    agents_present: Option<bool>,
}

/// One painted link region: `x_start..x_end` on screen row `y`, in absolute cells.
#[derive(Clone, Debug)]
struct PaintedLink {
    x_start: u16,
    x_end: u16,
    y: u16,
    url: std::sync::Arc<str>,
}

#[derive(Debug)]
enum PluginConfigState {
    Ready(crate::config::PluginConfig),
    Blocked { error: String },
}

impl App {
    pub fn new(repo: PathBuf, scope: Scope, base: Option<String>) -> Self {
        let (repo, mode) = match git::toplevel(&repo) {
            Some(root) => (root, RepositoryMode::GitReview),
            None => (repo, RepositoryMode::FilesOnly),
        };
        Self::build(repo, mode, scope, base, true)
    }

    /// Construct the error-only pane without reading derived repository state.
    #[cfg(test)]
    pub(crate) fn blocked(repo: PathBuf, scope: Scope, base: Option<String>) -> Self {
        // Unit seams exercise the Git review projection without loading a repository.
        Self::build(repo, RepositoryMode::GitReview, scope, base, false)
    }

    pub(crate) fn blocked_with_mode(
        repo: PathBuf,
        repository_mode: RepositoryMode,
        scope: Scope,
        base: Option<String>,
    ) -> Self {
        Self::build(repo, repository_mode, scope, base, false)
    }

    /// Construct from an already classified root. The runtime performs the one necessary Git
    /// probe before this call; all Files-only refreshes thereafter remain filesystem-only.
    pub(crate) fn new_with_mode(
        repo: PathBuf,
        repository_mode: RepositoryMode,
        scope: Scope,
        base: Option<String>,
    ) -> Self {
        Self::build(repo, repository_mode, scope, base, true)
    }

    fn build(
        repo: PathBuf,
        repository_mode: RepositoryMode,
        scope: Scope,
        base: Option<String>,
        load_turn: bool,
    ) -> Self {
        // Mirror any persisted turn baseline for a Git worktree only. Files-only mode never
        // touches Git refs or invokes Git after launch classification.
        let turn_baseline = (load_turn && repository_mode == RepositoryMode::GitReview)
            .then(|| crate::world::seed_baseline(&repo))
            .flatten();
        // Retain the selected root once. A failure stays Files-only and becomes a bounded root
        // listing error; it never falls back to pathname-based filesystem access.
        let files_root = (repository_mode == RepositoryMode::FilesOnly)
            .then(|| crate::world::FilesRoot::open(&repo))
            .and_then(Result::ok);
        let theme = theme::resolve(None);
        Self {
            repo,
            repository_mode,
            base,
            branch_base: git::BaseStatus::default(),
            base_epoch: 0,
            scope,
            tab: if repository_mode == RepositoryMode::FilesOnly {
                Tab::AllFiles
            } else {
                Tab::Changes
            },
            active_file_tab: if repository_mode == RepositoryMode::FilesOnly {
                Tab::AllFiles
            } else {
                Tab::Changes
            },
            focus: Focus::Files,
            entries: Vec::new(),
            file_rows: Vec::new(),
            file_cursor: 0,
            file_scroll: 0,
            reveal_files: false,
            reveal_diff: false,
            armed_cross: None,
            resume_list: false,
            toggled_dirs: HashSet::new(),
            raw_tree: RawTree::default(),
            files_root,
            stash: TabStash::default(),
            changed: HashMap::new(),
            diff: FileDiff::empty(),
            visible: Vec::new(),
            expanded_folds: HashSet::new(),
            hide_unchanged: false,
            diff_path: None,
            diff_cursor: 0,
            diff_scroll: 0,
            h_scroll: 0,
            wrap: true,
            preview: false,
            preview_scroll: 0,
            preview_text: String::new(),
            image_preview: None,
            image_preview_note: None,
            preview_max_scroll: std::cell::Cell::new(usize::MAX),
            preview_scrolled: false,
            pane_width: std::cell::Cell::new(0),
            painted_links: std::cell::RefCell::new(Vec::new()),
            painted_anchors: std::cell::RefCell::new(Vec::new()),
            pr_read_max_scroll: std::cell::Cell::new(usize::MAX),
            navigator_position: crate::config::NavigatorPosition::Right,
            navigator_side_pct: DEFAULT_SIDE_PCT,
            navigator_stack_pct: DEFAULT_STACK_PCT,
            navigator_hidden: false,
            search_pct: DEFAULT_SEARCH_PCT,
            divider_drag: DividerDrag::Idle,
            select_anchor: None,
            store: CommentStore::new(),
            list_cursor: 0,
            comment_focus: None,
            list_scroll: std::cell::Cell::new(0),
            picker_rows: Vec::new(),
            picker_cursor: 0,
            picker_notice: None,
            picker_over: Mode::Normal,
            last_sent_pane: None,
            remote_thread_assignments: HashMap::new(),
            pending_github_reviews: HashMap::new(),
            github_publishable_comments: HashSet::new(),
            github_submit_target: None,
            github_submit_available: false,
            base_picker: None,
            mode: Mode::Normal,
            input: String::new(),
            caret: 0,
            status: String::new(),
            keys_expanded: false,
            should_quit: false,
            pr: forge::PrView::Pending,
            pr_forge: crate::git::Forge::GitHub,
            pr_notice: None,
            pr_refreshing: false,
            pr_cursor: 0,
            pr_read_scroll: 0,
            pr_nav_scroll: std::cell::Cell::new(0),
            pr_nav_max_scroll: std::cell::Cell::new(usize::MAX),
            reveal_pr_nav: std::cell::Cell::new(true),
            pr_pending: None,
            world_request: None,
            search: None,
            search_dirty: false,
            search_track: None,
            find: None,
            refresh_indicator: false,
            refresh_commanded: false,
            tab_visited: false,
            highlighter: Highlighter::new(theme.syntax),
            palette: theme.palette,
            theme_name: theme.name,
            cli_theme_name: None,
            config: PluginConfigState::Ready(crate::config::PluginConfig::default()),
            requested_theme_name: None,
            cache: DiffCache::new(),
            markdown_cache: std::cell::RefCell::new(crate::markdown::RenderCache::default()),
            turn_baseline,
            agents_present: None,
        }
    }

    /// Whether this pane has the Git review domain. Files-only callers gate Git-only state
    /// transitions here rather than treating an empty diff as a repository.
    #[must_use]
    pub fn is_git_review(&self) -> bool {
        self.repository_mode == RepositoryMode::GitReview
    }

    fn files_only_unavailable(&mut self) {
        self.status = "unavailable in Files-only mode".to_string();
    }

    /// Resolve `name` (a CLI or config value; `None` = default) and apply it when it changes:
    /// rebuild the highlighter and drop cached diffs so they re-render. Unknown or
    /// not-yet-supported names fall back to the default (`specs/theme.md`).
    fn set_theme(&mut self, name: Option<&str>) {
        // Re-resolving the same name every poll would redo derivation and re-log an unknown
        // name, so skip when the request is unchanged.
        if self.requested_theme_name.as_deref() == name {
            return;
        }
        self.requested_theme_name = name.map(str::to_owned);
        let theme = theme::resolve(name);
        if theme.name != self.theme_name {
            self.theme_name = theme.name;
            self.palette = theme.palette;
            self.highlighter = Highlighter::new(theme.syntax);
            self.cache = DiffCache::new();
            self.markdown_cache.borrow_mut().clear();
        }
    }

    /// Record the `--theme` override name (highest precedence) and apply the resolved theme now.
    pub fn set_cli_theme(&mut self, name: Option<String>) {
        self.cli_theme_name = name;
        self.refresh_theme();
    }

    /// Apply one complete validated plugin configuration snapshot.
    pub fn set_plugin_config(&mut self, config: crate::config::PluginConfig) {
        let previous_position =
            self.plugin_config().map(crate::config::PluginConfig::navigator_position);
        let next_position = config.navigator_position();
        self.config = PluginConfigState::Ready(config);
        if previous_position != Some(next_position) {
            self.cancel_divider_drag();
            self.navigator_position = next_position;
        }
        self.refresh_theme();
    }

    /// The validated plugin configuration snapshot normal work currently uses.
    pub fn plugin_config(&self) -> Option<&crate::config::PluginConfig> {
        match &self.config {
            PluginConfigState::Ready(config) => Some(config),
            PluginConfigState::Blocked { .. } => None,
        }
    }

    /// Block the reviewr pane on one whole-file configuration failure.
    pub fn set_config_error(&mut self, error: String) {
        self.cancel_divider_drag();
        // The search overlay, the find band, and the agent picker close when the config view
        // takes over; recovery restores the tab beneath them. The query is not restored, and
        // neither are the picker's frozen rows, which would be stale by then (specs/search.md,
        // specs/find-in-file.md, specs/herdr-host.md).
        // The picker closes first, onto the mode it opened over, so the two closers below then
        // tear down that mode's own state instead of leaving it restored but emptied.
        self.close_picker();
        self.close_search();
        self.close_find();
        self.config = PluginConfigState::Blocked { error };
        self.pr_pending = None;
    }

    /// The active keymap: the snapshot's while ready, the defaults while blocked. The blocked
    /// arm only keeps this total — blocked key handling never reaches dispatch; the event
    /// loop's error gate answers the default `quit` key itself (`lib.rs`).
    pub fn keymap(&self) -> &crate::keymap::Keymap {
        match &self.config {
            PluginConfigState::Ready(config) => config.keymap(),
            PluginConfigState::Blocked { .. } => crate::keymap::default_keymap(),
        }
    }

    /// The error-only state rendered while plugin configuration is invalid.
    pub fn config_error(&self) -> Option<&str> {
        match &self.config {
            PluginConfigState::Ready(_) => None,
            PluginConfigState::Blocked { error, .. } => Some(error),
        }
    }

    /// Move user-authored review state into a freshly loaded app after config recovery. Saved
    /// comments always survive; an in-progress draft keeps the exact frozen diff it was written
    /// against, matching the ordinary refresh invariant.
    pub(crate) fn carry_authored_state_from(&mut self, old: &mut Self) {
        // The Changes projection belongs to that tab even while `All files` is active, where
        // it lives in the stash. Recovery rebuilds a fresh Changes frame first, then reapplies
        // this user-held choice without borrowing the other tab's preference.
        let changes_hide_unchanged = old.changes_hide_unchanged();
        self.store = std::mem::take(&mut old.store);
        self.list_cursor = old.list_cursor;
        self.comment_focus = old.comment_focus;
        self.list_scroll.set(old.list_scroll.get());
        // The footer expansion is one global toggle, carried regardless of the recovered mode
        // (`specs/input.md`).
        self.keys_expanded = old.keys_expanded;
        // The `last used` arming is session memory, like the comments themselves — a config
        // error must not forget which agent the session sent to (`specs/herdr-host.md`).
        self.last_sent_pane = old.last_sent_pane.take();
        self.navigator_side_pct = old.navigator_side_pct;
        self.navigator_stack_pct = old.navigator_stack_pct;
        self.navigator_hidden = old.navigator_hidden;
        // A hidden navigator keeps focus on the read pane (specs/tui.md); the fresh app
        // starts on the file list. The `List`/`Composing` arm re-carries the exact focus.
        if self.navigator_hidden {
            self.focus = Focus::Diff;
        }
        self.search_pct = old.search_pct;
        // A tab switch requested its refresh and recovery landed first: the carried fields
        // below may reinstate the stale stashed frame, so the pending request must survive
        // the swap or that frame never refreshes until the next poll.
        self.world_request = old.world_request.take();
        let old_mode = old.mode.clone();
        match old_mode {
            // `set_config_error` closes the search overlay, the find band, and the agent picker
            // before the mode is stored, so none reaches recovery; the search query is not
            // restored and the picker's frozen rows are not either (specs/search.md,
            // specs/find-in-file.md, specs/herdr-host.md).
            Mode::Normal
            | Mode::Search
            | Mode::Find
            | Mode::Picker
            | Mode::AssignPicker { .. }
            | Mode::RemoteAssignPicker { .. }
            | Mode::ConfirmRemoteAssign { .. } => {}
            Mode::List
            | Mode::Composing { .. }
            | Mode::ConfirmDelete { .. }
            | Mode::ConfirmPublish { .. }
            | Mode::SubmitReview { .. }
            | Mode::BasePick => {
                self.scope = old.scope;
                self.tab = old.tab;
                self.active_file_tab = old.active_file_tab;
                self.focus = old.focus;
                self.entries = std::mem::take(&mut old.entries);
                self.file_rows = std::mem::take(&mut old.file_rows);
                self.file_cursor = old.file_cursor;
                self.file_scroll = old.file_scroll;
                self.reveal_files = old.reveal_files;
                self.reveal_diff = old.reveal_diff;
                self.changed = std::mem::take(&mut old.changed);
                // The header's base label describes the carried list, so it carries too —
                // a fresh app would paint `no base` beside a populated frame
                // (`specs/tui.md`).
                self.branch_base = std::mem::take(&mut old.branch_base);
                self.diff = std::mem::take(&mut old.diff);
                self.visible = std::mem::take(&mut old.visible);
                self.expanded_folds = std::mem::take(&mut old.expanded_folds);
                self.diff_path = old.diff_path.take();
                self.diff_cursor = old.diff_cursor;
                self.diff_scroll = old.diff_scroll;
                self.h_scroll = old.h_scroll;
                self.select_anchor = old.select_anchor;
                self.resume_list = old.resume_list;
                self.toggled_dirs = std::mem::take(&mut old.toggled_dirs);
                self.raw_tree = std::mem::take(&mut old.raw_tree);
                self.stash = std::mem::take(&mut old.stash);
                self.wrap = old.wrap;
                self.preview = old.preview;
                self.preview_scroll = old.preview_scroll;
                self.preview_scrolled = old.preview_scrolled;
                self.preview_text = std::mem::take(&mut old.preview_text);
                self.mode = old.mode.clone();
                self.input = std::mem::take(&mut old.input);
                self.caret = old.caret;
                // The base picker survives recovery whole — rows, filter, and highlight
                // (`specs/tui.md`).
                self.base_picker = old.base_picker.take();
            }
        }
        self.set_changes_hide_unchanged(changes_hide_unchanged);
    }

    /// The Changes tab's projection preference, whether it is the active file tab or stashed
    /// behind `All files` (`specs/diff-view.md`).
    fn changes_hide_unchanged(&self) -> bool {
        if self.active_file_tab == Tab::Changes {
            self.hide_unchanged
        } else {
            self.stash.hide_unchanged
        }
    }

    /// Restore the Changes projection after config recovery without moving the reader away
    /// from the source row they were on. A context row hidden by the projection reconciles to
    /// the fold that now contains it, rather than retaining a stale numeric row index.
    fn set_changes_hide_unchanged(&mut self, hide_unchanged: bool) {
        if self.active_file_tab != Tab::Changes {
            self.stash.hide_unchanged = hide_unchanged;
            return;
        }
        if self.hide_unchanged == hide_unchanged {
            return;
        }
        let current = self.visible.get(self.diff_cursor).cloned();
        self.hide_unchanged = hide_unchanged;
        self.rebuild_visible();
        if let Some(current) = current
            && let Some(cursor) = self.visible.iter().position(|candidate| {
                candidate == &current
                    || matches!(candidate, Row::Fold { lines } if lines.contains(&current))
            })
        {
            self.diff_cursor = cursor;
        }
        self.settle_read();
    }

    fn config_snapshot(&self) -> &crate::config::PluginConfig {
        match &self.config {
            PluginConfigState::Ready(config) => config,
            PluginConfigState::Blocked { .. } => {
                unreachable!("normal work is gated while plugin configuration is invalid")
            }
        }
    }

    fn ensure_config_ready(&self) -> Result<()> {
        match &self.config {
            PluginConfigState::Ready(_) => Ok(()),
            PluginConfigState::Blocked { error } => {
                Err(anyhow::anyhow!("plugin configuration is invalid: {error}"))
            }
        }
    }

    /// Re-resolve the active theme from the CLI override or current validated snapshot.
    fn refresh_theme(&mut self) {
        let name = self
            .cli_theme_name
            .clone()
            .unwrap_or_else(|| self.config_snapshot().theme().to_owned());
        self.set_theme(Some(&name));
    }

    /// The active palette every renderer paints from (`specs/theme.md`).
    pub fn palette(&self) -> &Palette {
        &self.palette
    }

    pub fn composing(&self) -> bool {
        matches!(self.mode, Mode::Composing { .. })
    }

    /// The entry under the cursor when the cursor is on a file row; `None` on a directory
    /// row (or an empty list).
    pub fn current_entry(&self) -> Option<&Entry> {
        self.file_under_cursor_index().map(|i| &self.entries[i])
    }

    /// A directory's resting state in the active tab: `Changes` opens expanded, `All files`
    /// collapsed (specs/file-list.md).
    fn default_expanded(&self) -> bool {
        self.tab == Tab::Changes
    }

    /// The `entries` index of the file row under the cursor, or `None` on a directory row.
    fn file_under_cursor_index(&self) -> Option<usize> {
        self.file_rows.get(self.file_cursor).and_then(file_list::Row::file_index)
    }

    /// The visible-row index of the file at `path`, for restoring selection across a poll.
    fn file_row_of_path(&self, path: &str) -> Option<usize> {
        self.file_rows
            .iter()
            .position(|r| r.file_index().is_some_and(|i| self.entries[i].path == path))
    }

    /// The visible-row index of the first file row, the initial selection so a diff shows
    /// at once even when the tree opens on a directory.
    fn first_file_row(&self) -> Option<usize> {
        self.file_rows.iter().position(|r| r.file_index().is_some())
    }

    /// Rebuild the flattened tree from `entries` and the toggled-directory set.
    fn rebuild_file_rows(&mut self) {
        self.file_rows =
            file_list::build(&self.entries, &self.toggled_dirs, self.default_expanded());
    }

    /// What the cursor currently points at — a file (by path) or a directory (by path) — so
    /// the cursor can be put back on the same target after the tree rebuilds.
    fn cursor_anchor(&self) -> Option<Anchor> {
        self.file_rows.get(self.file_cursor).map(|r| match &r.kind {
            RowKind::File { index, .. } => Anchor::File(self.entries[*index].path.clone()),
            RowKind::Dir { path, .. } => Anchor::Dir(path.clone()),
        })
    }

    /// The visible-row index matching `anchor`, for restoring the cursor after a rebuild.
    fn row_of_anchor(&self, anchor: &Anchor) -> Option<usize> {
        self.file_rows.iter().position(|r| match (anchor, &r.kind) {
            (Anchor::File(p), RowKind::File { index, .. }) => &self.entries[*index].path == p,
            (Anchor::Dir(p), RowKind::Dir { path, .. }) => path == p,
            _ => false,
        })
    }

    /// The file whose diff the pane shows: the file under the cursor, or — when the cursor
    /// rests on a directory — the already-open file (matched by `diff_path`), so scanning the
    /// tree never blanks the diff. `None` only when nothing is open.
    fn shown_entry(&self) -> Option<Entry> {
        if let Some(e) = self.current_entry() {
            return Some(e.clone());
        }
        let open = self.diff_path.as_deref()?;
        self.entries.iter().find(|e| e.path == open).cloned()
    }

    /// Never touches the comment store or the in-progress input — that is the
    /// "a comment is never lost to a refresh" invariant (`specs/overview.md`).
    pub fn reload(&mut self) -> Result<()> {
        self.ensure_config_ready()?;
        // The PR tab holds its own state and renders nothing from the file tree, so a poll on
        // it skips the rebuild; switching back to a file tab reloads it then (specs/tui.md).
        if !self.tab.is_file_tab() {
            return Ok(());
        }
        let snapshot = crate::world::build(&self.world_input())?;
        self.reconcile_world(snapshot);
        Ok(())
    }

    /// The input the next world build reads — the tag a landed snapshot is checked against
    /// before it may reconcile (specs/tui.md).
    pub fn world_input(&self) -> crate::world::WorldInput {
        crate::world::WorldInput {
            repo: self.repo.clone(),
            repository_mode: self.repository_mode,
            tab: self.tab,
            scope: self.scope,
            base: self.base.clone(),
            base_epoch: self.base_epoch,
            turn_baseline: self.turn_baseline.clone(),
            // `Changes` never reads the toggled set, so it stays out of that tab's tag —
            // a directory toggle there must not invalidate an in-flight build.
            toggled_dirs: if self.tab == Tab::AllFiles {
                self.toggled_dirs.clone()
            } else {
                HashSet::new()
            },
            raw_dirs: self.raw_request_dirs(),
            files_root: self.files_root.clone(),
            raw_tree_epoch: self.raw_tree.epoch,
        }
    }

    /// Directories for the next Files-only world job. Root is always included. The currently
    /// loading set has priority so a capped job drains user requests before refresh work.
    fn raw_request_dirs(&self) -> BTreeSet<String> {
        if self.repository_mode != RepositoryMode::FilesOnly {
            return BTreeSet::new();
        }
        let mut candidates = BTreeSet::new();
        candidates.insert(String::new());
        for path in &self.raw_tree.loading {
            if self.raw_dir_reachable(path) {
                candidates.insert(path.clone());
            }
        }
        for path in &self.toggled_dirs {
            if self.dir_expanded(path) && self.raw_dir_reachable(path) {
                candidates.insert(path.clone());
            }
        }
        candidates.into_iter().take(RAW_DIR_BATCH_CAP).collect()
    }

    /// A nested raw directory may be listed only if each ancestor is a loaded, expanded real
    /// directory. This rejects stale paths after a parent disappears and keeps collapsed cache
    /// data from re-entering a request or the painted tree.
    fn raw_dir_reachable(&self, path: &str) -> bool {
        if path.is_empty() {
            return true;
        }
        let Some((parent, _)) = path.rsplit_once('/') else {
            return self.raw_tree.listings.get("").is_some_and(|entries| {
                entries.iter().any(|entry| entry.is_dir && entry.path == path)
            });
        };
        self.dir_expanded(parent)
            && self.raw_tree.listings.get(parent).is_some_and(|entries| {
                entries.iter().any(|entry| entry.is_dir && entry.path == path)
            })
            && self.raw_dir_reachable(parent)
    }

    /// Fold cached direct listings into exactly the root and descendants reachable through
    /// currently expanded ancestors. This is the only Files-only tree materialization path.
    fn materialized_raw_entries(&self) -> Vec<Entry> {
        fn append(app: &App, path: &str, out: &mut Vec<Entry>) {
            let Some(entries) = app.raw_tree.listings.get(path) else { return };
            for entry in entries {
                out.push(entry.clone());
                if entry.is_dir && app.dir_expanded(&entry.path) {
                    append(app, &entry.path, out);
                }
            }
        }
        let mut entries = Vec::new();
        append(self, "", &mut entries);
        entries
    }

    /// Apply one Files-only batch. Successful reads replace only their own direct listing;
    /// failed reads retain the known subtree and leave a retryable status for a never-loaded one.
    fn apply_raw_listings(&mut self, listings: Vec<crate::world::DirectoryListing>) {
        for listing in listings {
            self.raw_tree.loading.remove(&listing.path);
            match listing.entries {
                Ok(entries) => {
                    self.raw_tree.failed.remove(&listing.path);
                    self.raw_tree.listings.insert(listing.path, entries);
                    self.prune_raw_cache();
                }
                Err(error) => {
                    let unknown = !self.raw_tree.listings.contains_key(&listing.path);
                    self.raw_tree.failed.insert(listing.path.clone(), error);
                    if unknown {
                        self.status = format!(
                            "could not read {}; press r to retry",
                            raw_dir_label(&listing.path)
                        );
                    }
                }
            }
        }
        // The worker takes a bounded batch. Continue after its landing rather than enqueueing
        // a wide/deep expansion sweep ahead of input.
        if self.raw_tree.loading.iter().any(|path| self.raw_dir_reachable(path)) {
            self.request_world_refresh(false, false);
        }
    }

    /// Drop cached descendants only after a successful parent listing proves they no longer name
    /// real directories. A failed listing never reaches here, so stale-but-known content remains.
    fn prune_raw_cache(&mut self) {
        loop {
            let removed: Vec<String> = self
                .raw_tree
                .listings
                .keys()
                .filter(|path| !path.is_empty())
                .filter(|path| {
                    let parent = path.rsplit_once('/').map_or("", |(parent, _)| parent);
                    self.raw_tree.listings.get(parent).is_some_and(|entries| {
                        !entries.iter().any(|entry| entry.is_dir && entry.path == **path)
                    })
                })
                .cloned()
                .collect();
            if removed.is_empty() {
                break;
            }
            for path in removed {
                let descendant_prefix = format!("{path}/");
                self.raw_tree.listings.remove(&path);
                self.raw_tree.loading.remove(&path);
                self.raw_tree.failed.remove(&path);
                self.toggled_dirs.retain(|candidate| {
                    candidate != &path && !candidate.starts_with(&descendant_prefix)
                });
            }
        }
    }

    /// Adopt a build's base outcome — the one rule for both writers (the landed snapshot
    /// and the scope switch's synchronous rebuild): only the `branch` scope owns a base,
    /// and the base and the changeset it produced land together, so the header name and
    /// the list it heads never disagree (specs/tui.md).
    fn adopt_branch_base(&mut self, base: git::BaseStatus) {
        if self.scope == Scope::Branch {
            self.branch_base = base;
        }
    }

    /// Reconcile a built snapshot into the view — the one place a world result touches place
    /// state, by identity first, then fallback, then clamp (`specs/overview.md` Continuity).
    pub fn reconcile_world(&mut self, snapshot: crate::world::WorldSnapshot) {
        // Keep the cursor on the same row target across the rebuild; fall back to the open
        // file, then the first file. The toggled-directory set survives untouched.
        let anchor = self.cursor_anchor();
        let open = self.diff_path.clone();
        self.changed = snapshot.changed;
        if let Some(listings) = snapshot.raw_listings {
            self.apply_raw_listings(listings);
            self.entries = self.materialized_raw_entries();
        } else {
            self.entries = snapshot.entries;
        }
        self.adopt_branch_base(snapshot.branch_base);
        self.rebuild_file_rows();
        self.file_cursor = anchor
            .and_then(|a| self.row_of_anchor(&a))
            .or_else(|| open.as_deref().and_then(|p| self.file_row_of_path(p)))
            .or_else(|| self.first_file_row())
            .unwrap_or(0)
            .min(self.file_rows.len().saturating_sub(1));
        // A poll preserves the file-list wheel scroll — it does not reveal the cursor.
        // Explicit actions (navigation, a scope switch) request their own reveal.
        // While a modal is open the diff below it is frozen, so a poll can't shift the anchor
        // beneath the writer, reset the scroll and selection under the overlay, or move the
        // reviewer's place while they choose an agent (`Mode::is_modal`, overview.md Continuity).
        // The file list still updates above (specs/tui.md).
        if !self.mode.is_modal() && self.select_anchor.is_none() {
            // A poll keeps the reader on the same file; only a different shown file resets
            // the diff view to the top. It also drops an armed crossing, which was armed at the
            // edge of a file that is no longer the one on screen (specs/input.md).
            if self.shown_entry().map(|e| e.path) != self.diff_path {
                self.reset_diff_view();
                self.armed_cross = None;
            }
            self.load_read();
        }
        // A landed poll repaints the search preview in place — never the results, which
        // describe the worktree when their query ran (specs/search.md).
        self.refresh_search_preview();
        // The find band closes if its file lost its searchable rows or changed identity under the
        // poll — a forced return, like the markdown preview (specs/find-in-file.md). The current
        // match otherwise follows the reconciled cursor, so nothing else to do.
        if self.mode == Mode::Find && (open != self.diff_path || !self.find_available()) {
            self.close_find();
        }
        self.tab_visited = true;
    }

    /// Load the read pane for the active tab: the scope diff in `Changes`, the whole-file
    /// content in `All files`. Both flatten into `visible` and settle the cursor/scroll.
    fn load_read(&mut self) {
        let Some(entry) = self.shown_entry() else {
            // One atomic unavailable-reader transition: no old source, markdown, or image
            // payload may survive an emptied/removed selection (`diff-view.md` Continuity).
            self.diff = FileDiff::empty();
            self.diff_path = None;
            self.visible.clear();
            self.preview_text.clear();
            self.image_preview = None;
            self.image_preview_note = None;
            self.clear_selection();
            self.comment_focus = None;
            if matches!(self.mode, Mode::ConfirmDelete { .. }) {
                self.mode = Mode::Normal;
            }
            self.reset_diff_view();
            return;
        };
        self.open_path_in_tab(entry.path, entry.previous_path);
    }

    /// Open `path` in the active tab's read pane: the scope diff in `Changes` (rename-aware via
    /// `previous_path`), the whole-file content in `All files`. The one place this dispatch lives,
    /// so opening a file from the tree and from a comment edit can't drift apart.
    fn open_path_in_tab(&mut self, path: String, previous_path: Option<String>) {
        match self.tab {
            Tab::AllFiles => self.set_file_view(&path),
            // `Changes` (the `PR` tab never opens a file in the read pane).
            _ => self.set_diff(path, previous_path),
        }
    }

    /// Build the diff for a specific `path` regardless of whether its row is visible in the
    /// tree — so editing a comment can surface its file even from a collapsed directory.
    fn set_diff(&mut self, path: String, previous_path: Option<String>) {
        // A different file opens with all folds collapsed and in source. `expanded_folds` is
        // keyed by line number, so without the clear a fold in the new file whose first hidden
        // line matches an expanded one in the old file would render pre-expanded. A same-file
        // poll or scope switch keeps both the folds and the preview choice (specs/diff-view.md).
        if self.diff_path.as_deref() != Some(path.as_str()) {
            self.expanded_folds.clear();
            self.preview = false;
            self.preview_scroll = 0;
            self.preview_max_scroll.set(usize::MAX);
        }
        self.image_preview = None;
        self.image_preview_note = None;
        self.diff_path = Some(path.clone());
        let (old, new) = self.content_sides(&path, previous_path.as_deref());
        self.load_current_image_preview(&path);
        self.diff = self.cache.get(path, previous_path, &old, &new, &self.highlighter);
        // Hold the new side as the preview's render input, the same current content the File
        // view previews. A non-markdown file, a notice, or a deleted file (empty new side)
        // holds nothing, so its toggle stays inert (specs/diff-view.md).
        if self.markdown_file() && self.diff.state == crate::diff::FileState::Normal {
            self.preview_text = new;
        } else {
            self.preview_text.clear();
        }
        self.rebuild_visible();
        self.settle_read();
    }

    /// Build the File view for `path`: its current worktree content as `Context` rows, no
    /// folds. The `All files` read pane (specs/diff-view.md). Content is scope-independent.
    fn set_file_view(&mut self, path: &str) {
        self.set_file_view_with(path, None);
    }

    /// Set the File view from an optional descriptor-relative read already made for a selected
    /// Files-only search result. Reusing that read closes the preflight-to-open race: a result
    /// that becomes unavailable never leaves search for an empty replacement view.
    fn set_file_view_with(&mut self, path: &str, prepared: Option<(FileDiff, String)>) {
        // Opening a different file starts in source; a same-file refresh keeps the
        // preview choice and its scroll (specs/diff-view.md).
        if self.diff_path.as_deref() != Some(path) {
            self.preview = false;
            self.preview_scroll = 0;
            self.preview_max_scroll.set(usize::MAX);
        }
        self.image_preview = None;
        self.image_preview_note = None;
        self.diff_path = Some(path.to_string());
        self.expanded_folds.clear(); // the File view has no folds
        let (diff, content) = prepared.unwrap_or_else(|| self.file_view(path));
        // Keep the preview's render input current without a per-frame rebuild. A file the
        // source view degrades to a notice never previews (specs/diff-view.md), so its
        // content is not held either.
        if self.markdown_file() && diff.state == crate::diff::FileState::Normal {
            self.preview_text = content;
        } else {
            self.preview_text.clear();
        }
        self.diff = diff;
        self.load_current_image_preview(path);
        self.rebuild_visible();
        self.settle_read();
    }

    /// Build the read pane's File view for `path`: an over-budget blob (a model weight, a
    /// vendored bundle) previews as the too-large notice without a read — reading it whole
    /// would spike the UI thread before `build_file`'s budget could discard it — else the
    /// worktree content is highlighted through the shared content-hash cache. Returns the
    /// diff and the content read (empty for the notice), for a caller that also keeps the
    /// raw content (specs/diff-view.md). The one build the source view and the search
    /// preview share.
    fn file_view(&mut self, path: &str) -> (FileDiff, String) {
        if self.repository_mode == RepositoryMode::FilesOnly {
            return self.files_only_file_view(path).unwrap_or_else(|| {
                // A disappeared, replaced, or unreadable Files-only file has no pathname
                // fallback. Its empty read pane is bounded to the retained capability.
                let content = String::new();
                let diff = self.cache.get_file(path.to_string(), &content, &self.highlighter);
                (diff, content)
            });
        }
        let oversize = std::fs::metadata(self.repo.join(path))
            .is_ok_and(|m| crate::diff::over_byte_budget(m.len() as usize));
        if oversize {
            (FileDiff::too_large_notice(path.to_string()), String::new())
        } else {
            let content = worktree_content(&self.repo, path);
            let diff = self.cache.get_file(path.to_string(), &content, &self.highlighter);
            (diff, content)
        }
    }

    /// Build one Files-only File view through the retained descriptor authority. `None` means
    /// the exact relative target no longer resolves to a regular, no-follow file below the root.
    fn files_only_file_view(&mut self, path: &str) -> Option<(FileDiff, String)> {
        // The descriptor read uses the image cap so a valid image is not rejected by the smaller
        // text budget. Non-images retain the text-view limit below.
        match self.files_root.as_ref()?.read_file(path, image_preview::MAX_SOURCE_BYTES).ok()? {
            crate::world::RawFile::TooLarge => {
                Some((FileDiff::too_large_notice(path.to_string()), String::new()))
            }
            crate::world::RawFile::Content(bytes) => {
                self.set_image_preview(&bytes);
                if !self.image_view_active() && crate::diff::over_byte_budget(bytes.len()) {
                    return Some((FileDiff::too_large_notice(path.to_string()), String::new()));
                }
                let content = String::from_utf8_lossy(&bytes).into_owned();
                let diff = self.cache.get_file(path.to_string(), &content, &self.highlighter);
                Some((diff, content))
            }
        }
    }

    /// Establish a bounded image-only read for the current worktree file. Files-only remains
    /// descriptor-relative; Git review gets a separate capped raw reader and never reuses the
    /// lossy text path.
    fn load_current_image_preview(&mut self, path: &str) {
        if self.repository_mode == RepositoryMode::FilesOnly {
            // `files_only_file_view` already supplied authoritative bytes. Changes calls this
            // directly, so reopen only through the retained root capability.
            if self.tab == Tab::Changes
                && let Some(root) = &self.files_root
                && let Ok(crate::world::RawFile::Content(bytes)) =
                    root.read_file(path, image_preview::MAX_SOURCE_BYTES)
            {
                self.set_image_preview(&bytes);
            }
            return;
        }
        if let Ok(bytes) = bounded_worktree_bytes(&self.repo, path, image_preview::MAX_SOURCE_BYTES)
        {
            self.set_image_preview(&bytes);
        }
    }

    fn set_image_preview(&mut self, bytes: &[u8]) {
        match image_preview::decode(bytes) {
            Ok(preview) => self.image_preview = Some(preview),
            Err(ImagePreviewError::SvgUnavailable) => {
                self.image_preview_note = Some("SVG preview unavailable");
            }
            Err(ImagePreviewError::TooLarge) => self.image_preview_note = Some("image too large"),
            Err(ImagePreviewError::Malformed) => {
                self.image_preview_note = Some("unsupported image format");
            }
            Err(ImagePreviewError::NotImage) => return,
        }
        // Images have no source rows. A modal/comment focus authored before a refresh must not
        // survive as an invisible action against the newly non-text reader.
        self.clear_selection();
        self.comment_focus = None;
        self.find = None;
        if matches!(self.mode, Mode::ConfirmDelete { .. } | Mode::Composing { .. } | Mode::Find) {
            self.mode = Mode::Normal;
        }
    }

    /// Clamp the cursor, scroll, and selection to the rebuilt `visible`, keeping the reader's
    /// position. A shrunk view that forced the cursor to move reveals it; a poll that left it
    /// in range does not, so a wheel scroll survives.
    fn settle_read(&mut self) {
        if self.visible.is_empty() {
            self.reset_diff_view();
            return;
        }
        let last = self.visible.len() - 1;
        let clamped = self.diff_cursor.min(last);
        if clamped != self.diff_cursor {
            self.reveal_diff = true;
        }
        self.diff_cursor = clamped;
        self.diff_scroll = self.diff_scroll.min(last);
        self.select_anchor = self.select_anchor.map(|a| a.min(last));
    }

    /// Project source rows into the displayed diff. Hide-unchanged turns every contiguous
    /// context run into an independently expandable fold; it is a projection, never data loss.
    fn rebuild_visible(&mut self) {
        let normal: Vec<Row> = self
            .diff
            .rows
            .iter()
            .flat_map(|row| match row {
                Row::Fold { lines }
                    if row.fold_anchor().is_some_and(|a| self.expanded_folds.contains(&a)) =>
                {
                    lines.clone()
                }
                _ => vec![row.clone()],
            })
            .collect();
        if !self.hide_unchanged || self.tab != Tab::Changes {
            self.visible = normal;
            self.refresh_github_publishable_comments();
            return;
        }
        let mut projected = Vec::new();
        let mut context = Vec::new();
        let flush = |projected: &mut Vec<Row>, context: &mut Vec<Row>, expanded: &HashSet<u32>| {
            if context.is_empty() {
                return;
            }
            let anchor = context[0].new_no().or_else(|| context[0].old_no());
            if anchor.is_some_and(|a| expanded.contains(&a)) {
                projected.append(context);
            } else {
                projected.push(Row::Fold { lines: std::mem::take(context) });
            }
        };
        for row in normal {
            if matches!(row, Row::Context { .. }) {
                context.push(row);
            } else {
                flush(&mut projected, &mut context, &self.expanded_folds);
                projected.push(row);
            }
        }
        flush(&mut projected, &mut context, &self.expanded_folds);
        self.visible = projected;
        self.refresh_github_publishable_comments();
    }

    /// Expand the fold under the cursor, revealing its hidden lines. Expansion is
    /// permanent for the session — an expand is taken as intentional, so there is no
    /// collapse-back.
    /// Expand the fold under the cursor, keeping the viewport visually still. Where the fold
    /// sits decides which way it grows: a fold in the top half of the diff expands upward (the
    /// lines below it hold their screen position); one in the bottom half expands downward (the
    /// lines above hold theirs). `heights`/`viewport` are this frame's pre-expand diff geometry.
    pub fn expand_fold(&mut self, heights: &[usize], viewport: usize) {
        let fold_idx = self.diff_cursor;
        let Some(anchor) = self.visible.get(fold_idx).and_then(Row::fold_anchor) else {
            return;
        };
        // Expanding replaces the 1 fold row with N context rows; rows below it shift by N-1.
        let shift = self.visible[fold_idx].hidden().saturating_sub(1);
        // Display rows between the viewport top and the fold; < half ⇒ top half. When the fold
        // is wheeled above the viewport (fold_idx < diff_scroll), the range is empty → above 0 →
        // top half, which is correct: the inserted rows land above the viewport, so advancing
        // diff_scroll by `shift` holds the visible content in place.
        let above: usize = heights.get(self.diff_scroll..fold_idx).map_or(0, |s| s.iter().sum());
        let top_half = above < viewport / 2;
        self.expanded_folds.insert(anchor);
        self.rebuild_visible();
        if top_half {
            self.diff_scroll += shift; // hold the content below the fold; grow upward
        }
        // bottom half: leave diff_scroll — the content above the fold stays put, grow downward
    }

    /// The old and new content of `file` for the current scope: old from `HEAD` (or the
    /// merge-base on the branch scope), new from the worktree. A rename reads its old side
    /// from `previous_path`, so the diff shows real edits, not a wholesale delete-and-add.
    fn content_sides(&self, path: &str, previous_path: Option<&str>) -> (String, String) {
        if !self.is_git_review() {
            let content = self
                .files_root
                .as_ref()
                .and_then(|root| match root.read_file(path, crate::diff::MAX_BYTES) {
                    Ok(crate::world::RawFile::Content(bytes)) => {
                        Some(String::from_utf8_lossy(&bytes).into_owned())
                    }
                    Ok(crate::world::RawFile::TooLarge) | Err(_) => None,
                })
                .unwrap_or_default();
            return (String::new(), content);
        }
        let new_path = path;
        let old_path = previous_path.unwrap_or(new_path);
        match self.scope {
            Scope::Uncommitted => {
                let old = git::file_content(&self.repo, "HEAD", old_path);
                let new = worktree_content(&self.repo, new_path);
                (old, new)
            }
            Scope::Branch => {
                let mb = self
                    .branch_base
                    .winner
                    .as_ref()
                    .and_then(|b| git::merge_base(&self.repo, &b.oid));
                let old =
                    mb.map(|m| git::file_content(&self.repo, &m, old_path)).unwrap_or_default();
                (old, worktree_content(&self.repo, new_path))
            }
            Scope::LastTurn => {
                let old = self
                    .turn_baseline
                    .as_deref()
                    .map(|b| git::file_content(&self.repo, b, old_path))
                    .unwrap_or_default();
                (old, worktree_content(&self.repo, new_path))
            }
        }
    }

    /// Whether the `last-turn` scope is active but no baseline has been captured yet — the
    /// cold-start state the UI paints as [`Self::turn_wait_message`] (`specs/tui.md`).
    pub fn awaiting_turn(&self) -> bool {
        self.scope == Scope::LastTurn && self.turn_baseline.is_none()
    }

    /// The one message both panes paint for an [`Self::awaiting_turn`] frame, chosen here
    /// so the file list and the diff view cannot disagree (`specs/tui.md`). An empty
    /// worktree will never produce a turn, so saying so beats waiting — but only a sample
    /// that found no member says it, since the pre-poll frame may only wait: stale is
    /// allowed, wrong is not (`specs/overview.md` Continuity).
    pub fn turn_wait_message(&self) -> &'static str {
        match self.agents_present {
            Some(false) => "no agent works here",
            _ => "waiting for the first turn",
        }
    }

    /// The membership mirror itself: `None` until a sample observes it. The UI reads only
    /// [`Self::turn_wait_message`], which paints `None` and `Some(true)` alike; this exposes the
    /// held-versus-empty distinction underneath, which the turn-tracking tests assert directly.
    pub fn agents_present(&self) -> Option<bool> {
        self.agents_present
    }

    /// Follow the worker's baseline. Every completion carries the authoritative value, so
    /// the mirror syncs even when the completion's snapshot is superseded or discarded.
    pub fn sync_turn_baseline(&mut self, baseline: Option<String>) {
        self.turn_baseline = baseline;
    }

    /// Follow what a sample saw. `None` is a sample that could not observe the whole worktree —
    /// herdr was unreachable, or a member's directory would not resolve — and so saw nothing,
    /// which holds the previous answer rather than replacing it. Like
    /// [`Self::sync_turn_baseline`], this lands even from a superseded completion — the
    /// worker is serial, so no completion can carry membership newer than a later one.
    pub fn sync_agents_present(&mut self, present: Option<bool>) {
        self.agents_present = present.or(self.agents_present);
    }

    /// Queue a world refresh for the event loop to dispatch after the frame paints.
    /// `sample` rides the poll's status sample along; `reveal` re-reveals the cursor when
    /// the result lands, for user-initiated switches only (specs/tui.md).
    pub fn request_world_refresh(&mut self, sample_turn: bool, reveal: bool) {
        if self.repository_mode == RepositoryMode::FilesOnly {
            self.raw_tree.loading.insert(String::new());
            for path in self.toggled_dirs.clone() {
                if self.dir_expanded(&path) && self.raw_dir_reachable(&path) {
                    self.raw_tree.loading.insert(path);
                }
            }
        }
        let request = self.world_request.get_or_insert(crate::world::WorldRequest::default());
        request.sample_turn |= sample_turn;
        request.reveal |= reveal;
    }

    /// Snap the diff view back to the top, clearing any pending selection.
    fn reset_diff_view(&mut self) {
        self.diff_cursor = 0;
        self.diff_scroll = 0;
        self.h_scroll = 0;
        self.select_anchor = None;
    }

    /// Scroll the diff horizontally by `delta` columns, clamped at the left edge. A no-op
    /// while wrap is on, since the renderer ignores `h_scroll` when wrapping — so the offset
    /// never silently accumulates and then jumps the view when wrap is toggled off.
    pub fn scroll_h(&mut self, delta: isize) {
        if self.wrap || self.preview_active() || self.image_view_active() {
            return;
        }
        self.h_scroll = if delta >= 0 {
            self.h_scroll + delta as usize
        } else {
            self.h_scroll.saturating_sub(delta.unsigned_abs())
        };
    }

    /// Toggle line wrap; reset the horizontal scroll, which only applies with wrap off.
    pub fn toggle_wrap(&mut self) {
        if self.preview_active() || self.image_view_active() {
            return; // the wrap toggle is inert in the preview (specs/diff-view.md)
        }
        self.wrap = !self.wrap;
        self.h_scroll = 0;
    }

    /// Whether the open file qualifies for the markdown preview: a `.md`/`.markdown`
    /// extension, case-insensitive (specs/diff-view.md).
    #[must_use]
    fn markdown_file(&self) -> bool {
        self.diff_path.as_deref().is_some_and(is_markdown_path)
    }

    /// Whether the `m` toggle would open a preview here: a file tab holding current markdown
    /// content over a rendered pane. `preview_text` is filled only for a markdown file whose
    /// source rows render, so a notice, a deleted file (empty new side), or a rename away from
    /// markdown empties it and makes the toggle inert. The `visible` guard is not redundant:
    /// an emptied changeset clears `visible` through `load_read` without routing through
    /// `set_diff`, so a stale `preview_text` must not preview over a pane with no rows. The
    /// footer offers `m preview` exactly when this holds.
    #[must_use]
    fn previewable(&self) -> bool {
        self.tab.is_file_tab() && !self.preview_text.is_empty() && !self.visible.is_empty()
    }

    /// Whether the markdown preview is on screen: previewable and the toggle armed. A file
    /// renamed away from markdown or degraded mid-preview empties `preview_text` and drops
    /// back to source without disarming the toggle.
    #[must_use]
    pub fn preview_active(&self) -> bool {
        self.previewable() && self.preview
    }

    /// Whether the read pane is an image raster or fixed image fallback rather than source rows.
    /// This is the one gate for source-only interactions (specs/diff-view.md).
    #[must_use]
    pub fn image_view_active(&self) -> bool {
        self.image_preview.is_some() || self.image_preview_note.is_some()
    }

    /// Toggle source ↔ preview on a markdown file in a file tab; inert anywhere else.
    /// Entering clears a live selection and opens at the cursor's block; returning in the
    /// File view maps the top visible block back to a source cursor (specs/diff-view.md).
    pub fn toggle_preview(&mut self) {
        if self.image_view_active() {
            return;
        }
        // A file whose source view shows a notice, or a deleted file with no current
        // content, is not previewable, so the title can never claim a preview over a
        // notice (specs/diff-view.md).
        if !self.previewable() {
            return;
        }
        if self.preview {
            self.return_from_preview();
        } else {
            self.clear_selection();
            self.preview = true;
            self.preview_scrolled = false;
            self.align_preview_to_cursor();
        }
    }

    /// Scroll the preview to the block holding the cursor's current-content line, or the
    /// nearest block above it. Meta source lines are non-decreasing, so both lookups bisect.
    fn align_preview_to_cursor(&mut self) {
        self.preview_scroll = 0;
        let width = self.pane_width.get();
        if width == 0 || self.preview_text.is_empty() {
            return;
        }
        // The preview renders the current content, so a row's new-side line is its render
        // source line. A row without one — a deletion, a fold — aligns by the nearest row
        // above with one; none above leaves the preview at its top (specs/diff-view.md). A
        // File-view row is a context row numbered by its position, so this reduces to it.
        let Some(target) = self.visible[..=self.diff_cursor]
            .iter()
            .rev()
            .find_map(Row::new_no)
            .map(|n| n as usize)
        else {
            return;
        };
        let rendered = self.markdown_render(&self.preview_text, width);
        let after = rendered.meta.partition_point(|m| m.source_line <= target);
        let Some(last) = after.checked_sub(1) else {
            return;
        };
        let block_line = rendered.meta[last].source_line;
        self.preview_scroll = rendered.meta.partition_point(|m| m.source_line < block_line);
    }

    /// Leave the preview. In the Diff view the cursor, scroll, and folds stay exactly as
    /// they were left. In the File view a scrolled preview maps its top visible block back
    /// to a source cursor; an unscrolled one leaves the source position exactly as it was
    /// (specs/diff-view.md).
    fn return_from_preview(&mut self) {
        let scrolled = self.preview_scrolled;
        self.preview = false;
        if self.tab != Tab::AllFiles {
            return;
        }
        let width = self.pane_width.get();
        if !scrolled || width == 0 || self.preview_text.is_empty() {
            return;
        }
        let rendered = self.markdown_render(&self.preview_text, width);
        if rendered.meta.is_empty() || self.visible.is_empty() {
            return;
        }
        // Clamp to what the frame painted: a stale scroll past the max would map to a
        // block below the one the reader actually saw at the top of the pane.
        let top =
            self.preview_scroll.min(self.preview_max_scroll.get()).min(rendered.meta.len() - 1);
        let row = rendered.meta[top].source_line.saturating_sub(1);
        self.diff_cursor = row.min(self.visible.len() - 1);
        self.reveal_diff = true;
    }

    /// Scroll the preview by `delta` rendered lines, stopping with the last line at the
    /// pane's bottom edge — content that fits the pane does not scroll, and over-scroll
    /// never builds a dead zone the reader must unwind.
    pub fn preview_scroll_by(&mut self, delta: isize) {
        self.preview_scrolled = true;
        self.preview_scroll =
            clamp_scroll(self.preview_scroll, delta, self.preview_max_scroll.get());
    }

    /// The open markdown file's current content — the preview's render input.
    #[must_use]
    pub(crate) fn preview_text(&self) -> &str {
        &self.preview_text
    }

    /// Note the preview's maximum useful scroll; the renderer calls this each preview frame.
    pub fn note_preview_max_scroll(&self, max: usize) {
        self.preview_max_scroll.set(max);
    }

    /// Note the PR read pane's maximum useful scroll; the renderer calls this each frame.
    pub(crate) fn note_pr_read_max_scroll(&self, max: usize) {
        self.pr_read_max_scroll.set(max);
    }

    /// Record the navigator's painted scroll bound for wheel and page input.
    pub(crate) fn note_pr_nav_max_scroll(&self, max: usize) {
        self.pr_nav_max_scroll.set(max);
    }

    /// The first painted row in the PR navigator.
    #[must_use]
    pub(crate) fn pr_nav_scroll(&self) -> usize {
        self.pr_nav_scroll.get()
    }

    /// Set the bounded first row chosen by the renderer.
    pub(crate) fn set_pr_nav_scroll(&self, scroll: usize) {
        self.pr_nav_scroll.set(scroll);
    }

    /// Consume the request to reveal the selected PR row on this frame.
    pub(crate) fn take_pr_nav_reveal(&self) -> bool {
        self.reveal_pr_nav.replace(false)
    }

    /// Note the diff pane's inner width; the renderer calls this each paint, and the
    /// toggle's position mapping renders at this width.
    pub fn note_diff_width(&self, width: usize) {
        self.pane_width.set(width);
    }

    /// Drop the painted link and anchor regions; the renderer calls this each frame.
    pub(crate) fn clear_painted_links(&self) {
        self.painted_links.borrow_mut().clear();
        self.painted_anchors.borrow_mut().clear();
    }

    /// Note one painted link region, in absolute screen cells.
    pub(crate) fn note_painted_link(
        &self,
        x_start: u16,
        x_end: u16,
        y: u16,
        url: std::sync::Arc<str>,
    ) {
        self.painted_links.borrow_mut().push(PaintedLink { x_start, x_end, y, url });
    }

    /// Note one heading anchor of the painted markdown body, by content line index.
    pub(crate) fn note_painted_anchor(&self, slug: String, content_line: usize) {
        self.painted_anchors.borrow_mut().push((slug, content_line));
    }

    /// The destination under `(col, row)` on the painted frame, if a link was there.
    #[must_use]
    pub fn painted_link_at(&self, col: u16, row: u16) -> Option<std::sync::Arc<str>> {
        self.painted_links
            .borrow()
            .iter()
            .find(|l| l.y == row && col >= l.x_start && col < l.x_end)
            .map(|l| l.url.clone())
    }

    /// Act on a clicked link destination (`specs/markdown.md`): a `#anchor` scrolls its
    /// own surface to the matching heading, an `http(s)` destination opens in the
    /// browser, and anything else is inert.
    pub fn open_link(&mut self, url: &str) {
        if let Some(fragment) = url.strip_prefix('#') {
            // The fragment runs through the same normalization that made the slugs, so
            // `#Set-Up!` and `#İstanbul` find their headings (`specs/markdown.md`).
            self.jump_to_anchor(&crate::markdown::slug_text(fragment));
            return;
        }
        if let Ok(clean) = crate::browser::openable_url(url) {
            match crate::browser::open(clean) {
                Ok(()) => self.status = "opened link in browser".to_string(),
                Err(e) => self.status = e.to_string(),
            }
        }
    }

    /// Scroll the painted markdown surface to `slug`'s heading; a missing anchor is inert.
    fn jump_to_anchor(&mut self, slug: &str) {
        let target = self.painted_anchors.borrow().iter().find(|(s, _)| s == slug).map(|(_, i)| *i);
        let Some(idx) = target else {
            return;
        };
        if self.tab == Tab::Pr {
            self.pr_read_scroll = idx.min(self.pr_read_max_scroll.get());
        } else if self.preview_active() {
            self.preview_scrolled = true;
            self.preview_scroll = idx.min(self.preview_max_scroll.get());
        }
    }

    /// Render `text` as markdown wrapped to `width`, through the one-slot memo
    /// (`specs/markdown.md`).
    #[must_use]
    pub(crate) fn markdown_render(&self, text: &str, width: usize) -> crate::markdown::Rendered {
        self.markdown_cache.borrow_mut().get(text, width, &self.highlighter, &self.palette)
    }

    /// The navigator share remembered for the active side or stacked axis.
    #[must_use]
    pub fn navigator_share(&self) -> u16 {
        if self.navigator_position.stacked() {
            self.navigator_stack_pct
        } else {
            self.navigator_side_pct
        }
    }

    /// Move clockwise and cancel any drag captured under the previous geometry. Inert while
    /// the navigator is hidden (specs/input.md).
    pub fn cycle_navigator_position(&mut self) {
        if self.navigator_hidden_here() {
            return;
        }
        self.cancel_divider_drag();
        self.navigator_position = self.navigator_position.clockwise();
    }

    /// Whether the active tab can hide its navigator — `PR` never does (specs/tui.md).
    fn navigator_can_hide(&self) -> bool {
        self.tab != Tab::Pr
    }

    /// Whether the hidden state applies on the active tab.
    #[must_use]
    pub fn navigator_hidden_here(&self) -> bool {
        self.navigator_hidden && self.navigator_can_hide()
    }

    /// Hide the navigator, or show it back in its kept position and share. Hiding moves focus
    /// to the read pane; showing leaves it there. Inert on `PR` (specs/tui.md).
    pub fn toggle_navigator_hidden(&mut self) {
        if !self.navigator_can_hide() {
            return;
        }
        self.cancel_divider_drag();
        self.navigator_hidden = !self.navigator_hidden;
        if self.navigator_hidden {
            self.focus = Focus::Diff;
        } else {
            // File reveals wait out the hidden state (the files viewport is zero);
            // request one now at the shown size.
            self.reveal_files = true;
        }
    }

    /// Grow or shrink the navigator by `delta` percentage points on the active split axis.
    /// Inert while the navigator is hidden (specs/input.md).
    pub fn resize_navigator(&mut self, delta: i16) {
        if self.navigator_hidden_here() {
            return;
        }
        let next = (self.navigator_share() as i16).saturating_add(delta).max(0) as u16;
        self.set_navigator_share(next);
    }

    /// Capture a divider gesture for the current position; cancelled capture waits for mouse-up.
    pub fn start_divider_drag(&mut self) {
        if self.divider_drag != DividerDrag::Cancelled {
            self.divider_drag = DividerDrag::Active { position: self.navigator_position };
        }
    }

    /// Cancel movement while retaining capture so later drag events cannot become a selection.
    pub fn cancel_divider_drag(&mut self) {
        if matches!(self.divider_drag, DividerDrag::Active { .. }) {
            self.divider_drag = DividerDrag::Cancelled;
        }
    }

    /// Release divider capture on mouse-up.
    pub fn finish_divider_drag(&mut self) {
        self.divider_drag = DividerDrag::Idle;
    }

    /// Whether a divider gesture still owns drag and mouse-up events.
    #[must_use]
    pub fn divider_drag_active(&self) -> bool {
        matches!(self.divider_drag, DividerDrag::Active { .. })
    }

    /// Whether captured drag events are being consumed without resizing.
    #[must_use]
    pub fn divider_drag_cancelled(&self) -> bool {
        self.divider_drag == DividerDrag::Cancelled
    }

    /// Whether the current gesture still owns drag and mouse-up events in either state.
    #[must_use]
    pub fn divider_drag_captured(&self) -> bool {
        self.divider_drag_active() || self.divider_drag_cancelled()
    }

    /// Set the active share from the captured split axis, cancelling if the position changed.
    pub fn drag_divider(&mut self, axis_len: u16, offset: u16) {
        let DividerDrag::Active { position } = self.divider_drag else {
            return;
        };
        if position != self.navigator_position {
            self.divider_drag = DividerDrag::Cancelled;
            return;
        }
        if axis_len == 0 {
            return;
        }
        let offset = offset.min(axis_len);
        let navigator_len = match self.navigator_position {
            crate::config::NavigatorPosition::Left | crate::config::NavigatorPosition::Top => {
                offset
            }
            crate::config::NavigatorPosition::Right | crate::config::NavigatorPosition::Bottom => {
                axis_len.saturating_sub(offset)
            }
        };
        let pct = (u32::from(navigator_len) * 100 / u32::from(axis_len)) as u16;
        self.set_navigator_share(pct);
    }

    /// Set the search screen's results share from a captured drag on its divider. The
    /// review layout's shares are untouched (specs/search.md).
    pub fn drag_search_divider(&mut self, axis_len: u16, offset: u16) {
        if !self.divider_drag_active() || axis_len == 0 {
            return;
        }
        let offset = offset.min(axis_len);
        let pct = (u32::from(offset) * 100 / u32::from(axis_len)) as u16;
        self.search_pct = pct.clamp(MIN_SEARCH_PCT, MAX_SEARCH_PCT);
    }

    /// Clamp and store one share through the active axis's single bounds/ownership contract.
    fn set_navigator_share(&mut self, share: u16) {
        let max = if self.navigator_position.stacked() { MAX_STACK_PCT } else { MAX_SIDE_PCT };
        let clamped = share.clamp(MIN_NAVIGATOR_PCT, max);
        if self.navigator_position.stacked() {
            self.navigator_stack_pct = clamped;
        } else {
            self.navigator_side_pct = clamped;
        }
    }

    // --- Scroll model (shared by both panes) ---------------------------------------
    //
    // Each pane has a cursor (selection) and a scroll offset (viewport top). They are
    // independent: keyboard navigation moves the cursor and requests a reveal; the wheel
    // moves the offset and requests nothing. Every frame the event loop reveals the cursor
    // *only if a move requested it* (so the wheel can leave the cursor off screen) and then
    // bounds the offset (so an over-scroll never shows a blank tail). Both panes run the
    // same `keep_in_view` + `bound`; the file list passes all-height-1 rows.

    /// Scroll the file list so `file_cursor` is on screen — the minimal nudge. Called once
    /// per frame when a navigation requested a reveal, not on a wheel scroll.
    pub fn reveal_file_cursor(&mut self, viewport: usize) {
        if self.file_rows.is_empty() {
            self.file_scroll = 0;
            return;
        }
        let cursor = self.file_cursor.min(self.file_rows.len() - 1);
        let heights = vec![1usize; self.file_rows.len()];
        self.file_scroll = keep_in_view(cursor, self.file_scroll, &heights, viewport);
    }

    /// Clamp `file_scroll` within range (no blank tail). Called every frame.
    pub fn bound_file_scroll(&mut self, viewport: usize) {
        self.file_scroll = bound(self.file_scroll, self.file_rows.len(), viewport);
    }

    /// Scroll the diff so the reveal target's row fits the `viewport`-display-row window —
    /// `heights` is each visible row's display height (wrap + comment cards). Called once
    /// per frame when a navigation requested a reveal, not on a wheel scroll.
    ///
    /// The target is the cursor, except while composing: the box opens under the selection's
    /// last line (`specs/tui.md`), so that line is what has to stay in view. A selection built
    /// upward has its cursor at the top, and following the cursor there would leave the box
    /// off the bottom — the selection covers the same rows either way (`specs/diff-view.md`).
    pub fn reveal_diff_cursor(&mut self, heights: &[usize], viewport: usize) {
        if self.visible.is_empty() {
            self.diff_scroll = 0;
            return;
        }
        let target = if self.composing() { self.selection_range().1 } else { self.diff_cursor };
        let target = target.min(self.visible.len() - 1);
        self.diff_scroll = keep_in_view(target, self.diff_scroll, heights, viewport);
    }

    /// Clamp `diff_scroll` within range (no blank tail). Called every frame. Height-aware:
    /// the cap is the offset that shows the LAST row at the bottom — computed from `heights`,
    /// not the row count, so a wrapped diff (tall rows) stays fully reachable. A row-count cap
    /// would stop short of the bottom whenever rows span more than one display line.
    pub fn bound_diff_scroll(&mut self, heights: &[usize], viewport: usize) {
        if heights.is_empty() {
            self.diff_scroll = 0;
            return;
        }
        let max_top = keep_in_view(heights.len() - 1, self.diff_scroll, heights, viewport);
        self.diff_scroll = self.diff_scroll.min(max_top);
    }

    /// Switch the changeset scope and reload. A no-op while composing, so a comment
    /// in progress is never stranded against a different diff.
    pub fn set_scope(&mut self, scope: Scope) -> Result<()> {
        self.ensure_config_ready()?;
        if !self.is_git_review() {
            self.files_only_unavailable();
            return Ok(());
        }
        if self.scope != scope && !self.composing() && self.select_anchor.is_none() {
            self.scope = scope;
            self.rebase_changes()?;
            // An explicit switch reveals the cursor (a poll does not).
            self.reveal_files = true;
        }
        Ok(())
    }

    /// Rebuild the views after the changeset's base moved — a scope switch or a base pick.
    /// The change replaces the Changes changeset (and each file's old side), so the Changes
    /// tab snaps to the top: reset its cursor, folds, and diff scroll, and drop cached diffs.
    /// The `All files` listing and File view are base-independent (only the annotations
    /// move), so its own state is held by `reload`. The Changes state is the active one on
    /// `Changes` and the stashed one while `All files` is shown — reset whichever holds it,
    /// so a return to Changes never lands on a stale scroll or a pre-expanded fold.
    fn rebase_changes(&mut self) -> Result<()> {
        self.cache = DiffCache::new();
        if self.tab == Tab::Changes {
            self.file_cursor = 0;
            self.expanded_folds.clear();
            self.reset_diff_view();
            // The changed set rebuilds before the frame, so the list never shows another
            // base's files under the new base's label (specs/tui.md). In `Changes` the
            // changeset is the whole snapshot, so this is the full (cheap) reload.
            self.reload()?;
        } else {
            self.stash.file_cursor = 0;
            self.stash.expanded_folds.clear();
            self.stash.diff_cursor = 0;
            self.stash.diff_scroll = 0;
            self.stash.h_scroll = 0;
            self.stash.select_anchor = None;
            // `All files` keeps its tree; only the changed set rebuilds before the frame.
            // The tree's annotations refresh behind it via the worker (specs/tui.md).
            let (branch_base, changed) = crate::world::build_changed(&self.world_input())?;
            self.adopt_branch_base(branch_base);
            self.changed = crate::world::annotate(&changed);
            // Re-mark the tree in place — the rows are base-independent, only their
            // badges move, so the switch frame never shows the old base's badges
            // under the new base's header (policies/ux-responsiveness.md). The tree
            // itself still refreshes behind the switch.
            for entry in &mut self.entries {
                entry.annotation = self.changed.get(&entry.path).cloned();
            }
            self.rebuild_file_rows();
            self.request_world_refresh(false, false);
        }
        Ok(())
    }

    /// Queue a PR refresh, merging into any request already pending: the stronger kind
    /// wins, so an ambient trigger can never downgrade the user's commanded refresh.
    pub fn request_pr_refresh(&mut self, kind: RefreshKind) {
        if self.is_git_review() {
            self.pr_pending = self.pr_pending.max(Some(kind));
        }
    }

    /// Switch to `tab`, saving the active tab's navigator and read-pane state and restoring the
    /// target's. Each tab keeps its own opened file and scroll, so returning to a tab lands
    /// exactly where you left it (specs/tui.md). The switch frame paints the restored state as
    /// it was; a world refresh lands behind it — stale until it lands, never wrong
    /// (specs/overview.md Continuity). A no-op on the active tab or while composing; focus
    /// stays on the same side.
    pub fn set_tab(&mut self, tab: Tab) -> Result<()> {
        self.ensure_config_ready()?;
        if !self.is_git_review() && tab != Tab::AllFiles {
            self.files_only_unavailable();
            return Ok(());
        }
        if self.tab == tab || self.composing() || self.select_anchor.is_some() {
            return Ok(());
        }
        self.tab = tab;
        // Entering the PR tab leaves the file tabs frozen in place and fetches the PR. A
        // `loading` frame draws before the blocking fetch the event loop services, and a
        // re-entry keeps the last snapshot on screen while it refetches.
        if tab == Tab::Pr {
            self.request_pr_refresh(RefreshKind::Ambient);
            return Ok(());
        }
        // Entering a file tab: bring its state into the diff fields if the other file tab holds
        // them (a Changes↔AllFiles switch, or a return from PR onto the stashed tab).
        if self.active_file_tab != tab {
            self.swap_active_with_stash();
            self.active_file_tab = tab;
        }
        // A first visit has no stash to paint: refreshing behind would show an empty tree
        // under a live changed-count, a header/body disagreement
        // (policies/ux-responsiveness.md). Load it before the frame instead; every return
        // visit paints its stash instantly and refreshes behind it. The visited marker,
        // not emptiness — a clean repo's `Changes` tab is legitimately empty.
        if self.tab_visited {
            self.request_world_refresh(false, true);
        } else {
            self.reload()?;
        }
        self.settle_tab_entry();
        self.reveal_files = true; // pull the restored cursor back into view
        Ok(())
    }

    /// An empty read pane — a first visit landing on a collapsed tree, or an open file gone
    /// empty — focuses the tree, so the cursor keys aren't trapped on a pane with nothing to
    /// move (specs/tui.md). Runs on the switch frame and again when its world refresh lands.
    pub(crate) fn settle_tab_entry(&mut self) {
        if self.navigator_hidden_here() {
            // A `PR` visit may have focused its always-shown navigator; entry restores
            // the hidden-state invariant.
            self.focus = Focus::Diff;
            return;
        }
        if self.visible.is_empty() {
            self.focus = Focus::Files;
        }
    }

    // ---- PR tab (specs/forge-host.md, specs/pr-tab.md) -------------------------------------

    /// Clear a snapshot whose complete fetch input no longer matches the worktree.
    pub fn clear_pr(&mut self) {
        self.pr = forge::PrView::Pending;
        self.recompute_github_submit_availability();
        self.pr_notice = None;
        self.pr_refreshing = false;
        self.pr_cursor = 0;
        self.pr_read_scroll = 0;
        self.pr_nav_scroll.set(0);
        self.reveal_pr_nav.set(true);
    }

    /// Apply a snapshot fetched off-thread (`forge::fetch` runs on a worker so the UI never
    /// blocks — `lib.rs`). A transient `Error` keeps the last good snapshot frozen with a status
    /// note, so a failed poll never blanks a populated tab; the cursor clamps to the new rows.
    pub fn apply_pr(&mut self, view: forge::PrView) {
        self.pr_refreshing = false;
        let retry = view.retry_remedy(self.keymap().hint(crate::keymap::Action::Refresh));
        let has_snapshot =
            matches!(self.pr, forge::PrView::Pr(_) | forge::PrView::NoPr | forge::PrView::Detached);
        if has_snapshot && let Some(message) = retry {
            self.pr_notice = Some(message);
            return;
        }
        // A held resolution keeps the painted story, and so does a transient detach
        // while a snapshot is on screen (`specs/forge-host.md` Refresh).
        if matches!(view, forge::PrView::Held)
            || (matches!(view, forge::PrView::Detached) && matches!(self.pr, forge::PrView::Pr(_)))
        {
            self.pr_notice = None;
            return;
        }
        self.pr_notice = None;
        // Follow the selected row by identity, not index, so a refresh that inserts a newer
        // comment (the list is newest-first) keeps the cursor on the same one and leaves the read
        // scroll intact — only a vanished or absent selection resets it (mirrors the file tabs'
        // poll-preservation, specs/pr-tab.md). The pinned description row's identity is itself:
        // it survives while the new snapshot still has a description, and an emptied one
        // vanishes like a deleted comment.
        let on_description = self.pr_on_description();
        let selected = self
            .pr_selected_comment()
            .map(|c| (c.author.clone(), c.created_at.clone(), c.anchor.clone()));
        self.pr = view;
        self.recompute_github_submit_availability();
        let offset = self.pr_description_offset();
        let restored = if on_description {
            self.pr_has_description().then_some(0)
        } else {
            selected.as_ref().and_then(|(author, created, anchor)| {
                let i = self.pr_snapshot()?.comments.iter().position(|c| {
                    c.author == *author && c.created_at == *created && c.anchor == *anchor
                })?;
                Some(i + offset)
            })
        };
        if let Some(i) = restored {
            self.pr_cursor = i;
        } else {
            // The selection vanished (or there was none): clamp the cursor into range,
            // and reset the read pane whenever a selected row disappeared — the pane now
            // shows a different row (specs/pr-tab.md).
            let clamped = self.pr_row_count().saturating_sub(1);
            if self.pr_cursor > clamped || on_description || selected.is_some() {
                self.pr_read_scroll = 0;
            }
            self.pr_cursor = self.pr_cursor.min(clamped);
        }
    }

    /// Persistent remedy for a failed same-input refresh.
    pub fn pr_notice(&self) -> Option<&str> {
        self.pr_notice.as_deref()
    }

    pub fn set_pr_refreshing(&mut self, refreshing: bool) {
        if refreshing && matches!(self.pr, forge::PrView::Pending) {
            self.pr = forge::PrView::Loading;
            self.pr_refreshing = false;
        } else {
            self.pr_refreshing = refreshing;
        }
    }

    pub fn pr_refreshing(&self) -> bool {
        self.pr_refreshing
    }

    /// The resolved snapshot, or `None` in a loading/degraded view.
    #[must_use]
    pub fn pr_snapshot(&self) -> Option<&forge::PrSnapshot> {
        match &self.pr {
            forge::PrView::Pr(s) => Some(s),
            _ => None,
        }
    }

    /// Whether the snapshot carries a PR description — the pinned `description` row's
    /// existence condition (specs/pr-tab.md).
    #[must_use]
    pub fn pr_has_description(&self) -> bool {
        self.pr_snapshot().is_some_and(|s| !s.body.trim().is_empty())
    }

    /// Whether the navigator cursor sits on the pinned `description` row.
    #[must_use]
    pub fn pr_on_description(&self) -> bool {
        self.pr_has_description() && self.pr_cursor == 0
    }

    /// How many cursor rows the pinned description occupies before the comments — the
    /// one home for the comment-index ↔ cursor-index shift every consumer applies.
    #[must_use]
    pub fn pr_description_offset(&self) -> usize {
        usize::from(self.pr_has_description())
    }

    /// The navigator's cursor count: the pinned description row (when the PR has one)
    /// plus the comments. Checks are a status display, not a cursor stop — landing on
    /// one shows nothing the row itself doesn't.
    #[must_use]
    pub fn pr_row_count(&self) -> usize {
        self.pr_snapshot().map_or(0, |s| s.comments.len() + self.pr_description_offset())
    }

    /// The comment under the navigator cursor, for the read pane. `None` on the pinned
    /// description row ([`Self::pr_on_description`]) and in a degraded view.
    #[must_use]
    pub fn pr_selected_comment(&self) -> Option<&forge::Comment> {
        if self.pr_on_description() {
            return None;
        }
        let offset = self.pr_description_offset();
        self.pr_snapshot()?.comments.get(self.pr_cursor - offset)
    }

    /// Move the navigator cursor by `delta`, resetting the read pane to the top.
    pub fn pr_move(&mut self, delta: isize) {
        let n = self.pr_row_count();
        if n == 0 {
            return;
        }
        self.pr_select(step(self.pr_cursor, delta, n));
    }

    /// Select navigator row `i`, resetting the read pane to the top — the one place the
    /// cursor-move and the read-scroll reset stay paired (a click and `j`/`k` share it).
    pub(crate) fn pr_select(&mut self, i: usize) {
        self.pr_cursor = i;
        self.pr_read_scroll = 0;
        self.reveal_pr_nav.set(true);
    }

    pub(crate) fn pr_scroll_nav(&mut self, delta: isize) {
        self.reveal_pr_nav.set(false);
        self.pr_nav_scroll.set(clamp_scroll(
            self.pr_nav_scroll.get(),
            delta,
            self.pr_nav_max_scroll.get(),
        ));
    }

    /// Scroll the read pane by `delta` lines (the wheel and `PageUp`/`PageDown`), stopping
    /// with the last line at the pane's bottom edge. The base clamps first, so a stale
    /// scroll (the pane grew, or the body shrank) never swallows the first upward input.
    pub(crate) fn pr_scroll_read(&mut self, delta: isize) {
        self.pr_read_scroll =
            clamp_scroll(self.pr_read_scroll, delta, self.pr_read_max_scroll.get());
    }

    /// Open the pull request in the browser (`specs/pr-tab.md`). A resolved PR always carries a
    /// `url`, so there is nothing to guard against.
    pub fn pr_open(&mut self) {
        let Some(url) = self.pr_snapshot().map(|s| s.url.clone()) else {
            return;
        };
        match crate::browser::open(&url) {
            Ok(()) => self.status = format!("opened {} in browser", self.pr_forge.abbr()),
            Err(e) => self.status = e.to_string(),
        }
    }

    /// Exchange the active per-tab fields with the inactive tab's saved snapshot. Every per-tab
    /// field on `App` must be swapped here — a new per-tab field left out silently bleeds one
    /// tab's selection or scroll into the other.
    fn swap_active_with_stash(&mut self) {
        std::mem::swap(&mut self.entries, &mut self.stash.entries);
        std::mem::swap(&mut self.raw_tree, &mut self.stash.raw_tree);
        std::mem::swap(&mut self.file_rows, &mut self.stash.file_rows);
        std::mem::swap(&mut self.file_cursor, &mut self.stash.file_cursor);
        std::mem::swap(&mut self.file_scroll, &mut self.stash.file_scroll);
        std::mem::swap(&mut self.toggled_dirs, &mut self.stash.toggled_dirs);
        std::mem::swap(&mut self.diff, &mut self.stash.diff);
        std::mem::swap(&mut self.visible, &mut self.stash.visible);
        std::mem::swap(&mut self.expanded_folds, &mut self.stash.expanded_folds);
        std::mem::swap(&mut self.diff_path, &mut self.stash.diff_path);
        std::mem::swap(&mut self.diff_cursor, &mut self.stash.diff_cursor);
        std::mem::swap(&mut self.diff_scroll, &mut self.stash.diff_scroll);
        std::mem::swap(&mut self.h_scroll, &mut self.stash.h_scroll);
        std::mem::swap(&mut self.select_anchor, &mut self.stash.select_anchor);
        std::mem::swap(&mut self.hide_unchanged, &mut self.stash.hide_unchanged);
        std::mem::swap(&mut self.preview, &mut self.stash.preview);
        std::mem::swap(&mut self.preview_scroll, &mut self.stash.preview_scroll);
        std::mem::swap(&mut self.preview_text, &mut self.stash.preview_text);
        std::mem::swap(&mut self.image_preview, &mut self.stash.image_preview);
        std::mem::swap(&mut self.image_preview_note, &mut self.stash.image_preview_note);
        std::mem::swap(&mut self.preview_scrolled, &mut self.stash.preview_scrolled);
        std::mem::swap(&mut self.tab_visited, &mut self.stash.visited);
    }

    /// While the navigator is hidden, `tab` shows it and focuses it instead of flipping
    /// between panes (specs/input.md).
    pub fn toggle_focus(&mut self) {
        if self.navigator_hidden_here() {
            self.navigator_hidden = false;
            self.focus = Focus::Files;
            self.reveal_files = true;
            return;
        }
        self.focus = match self.focus {
            Focus::Files => Focus::Diff,
            Focus::Diff => Focus::Files,
        };
    }

    /// Move the cursor in the focused pane by `delta` rows. In the files pane the cursor steps
    /// over the tree's visible rows; landing on a file row opens its diff, while a directory row
    /// keeps the current diff so scanning the tree never blanks the pane. The page/half-page keys
    /// reuse this with a larger `delta`, since paging is just a bigger cursor move in the focus.
    pub fn move_cursor(&mut self, delta: isize) -> Result<()> {
        self.ensure_config_ready()?;
        if self.focus == Focus::Diff && self.image_view_active() {
            return Ok(());
        }
        if self.select_anchor.is_some() && self.focus == Focus::Files {
            return Ok(());
        }
        match self.focus {
            Focus::Files => {
                if !self.file_rows.is_empty() {
                    self.file_cursor = step(self.file_cursor, delta, self.file_rows.len());
                    self.open_cursor_file();
                    // Reveal even when the index clamps unchanged (e.g. `k` at the top), so a
                    // navigation always pulls the cursor back after a wheel scroll.
                    self.reveal_files = true;
                }
            }
            Focus::Diff => {
                // The preview has no cursor: vertical movement scrolls it, and the source
                // view's cursor waits untouched for the toggle back (specs/diff-view.md).
                if self.preview_active() {
                    self.preview_scroll_by(delta);
                } else if !self.visible.is_empty() {
                    let mut target = step(self.diff_cursor, delta, self.visible.len());
                    if let Some(a) = self.select_anchor {
                        target = self.fold_clamped(a, target);
                    }
                    self.diff_cursor = target;
                    self.reveal_diff = true;
                }
            }
        }
        Ok(())
    }

    /// Open the diff for the file under the cursor when it differs from the one shown; a
    /// no-op on a directory row, so the current diff stays put.
    fn open_cursor_file(&mut self) {
        if let Some(i) = self.file_under_cursor_index()
            && Some(self.entries[i].path.as_str()) != self.diff_path.as_deref()
        {
            self.reset_diff_view();
            self.load_read();
        }
    }

    /// `next-file`: open the next file, from either pane (`specs/input.md`).
    pub fn next_file(&mut self) {
        self.step_file(true);
    }

    /// `prev-file`: open the previous file; see [`Self::next_file`].
    pub fn prev_file(&mut self) {
        self.step_file(false);
    }

    /// Move the file cursor to the nearest file row and open it, keeping the focused pane. The
    /// cursor carries the selection with it, so the list always highlights the open file.
    ///
    /// The list steps from its own cursor, which is what the reviewer is moving there. The diff
    /// steps from the open file, so a press always opens a file — the cursor may sit elsewhere,
    /// parked on a directory row (which keeps the open diff).
    fn step_file(&mut self, forward: bool) {
        if !self.can_traverse() {
            return;
        }
        let from = if self.focus == Focus::Files { self.file_cursor } else { self.open_file_row() };
        let Some(row) = self.file_row_from(from, forward) else { return };
        self.file_cursor = row;
        self.open_cursor_file();
        self.reveal_files = true;
    }

    /// `next-hunk`: jump to the nearest change run below the cursor (`specs/input.md`).
    pub fn next_hunk(&mut self) {
        self.step_hunk(true);
    }

    /// `prev-hunk`: jump to the nearest hunk above the cursor; see [`Self::next_hunk`].
    pub fn prev_hunk(&mut self) {
        self.step_hunk(false);
    }

    /// Move the diff cursor to the nearest hunk's first changed row past it. With no hunk left
    /// this way, the first press arms the crossing and the second one takes it, so a held key
    /// stops at each file. Only `Changes` paints change rows — `All files` is all context, and
    /// the preview has no cursor — so a step anywhere else has no target (`specs/input.md`).
    fn step_hunk(&mut self, forward: bool) {
        if !self.is_git_review() {
            self.files_only_unavailable();
            return;
        }
        // Any step drops the standing arm. A step the other way is not the repeat it waits for.
        let armed = self.armed_cross.take().filter(|a| a.forward == forward);
        if !self.can_traverse()
            || self.tab != Tab::Changes
            || self.preview_active()
            || self.image_view_active()
        {
            return;
        }
        if let Some(row) = hunk_row(&self.visible, Some(self.diff_cursor), forward) {
            self.diff_cursor = row;
            self.reveal_diff = true;
            return;
        }
        let Some(armed) = armed else {
            // The first press resolves the crossing and arms it, so the footer can offer it and
            // a held key stops at the file boundary. With no file to cross to — the changeset's
            // end — nothing is offered and the press is inert.
            if let Some(row) = self.cross_target(forward)
                && let Some(path) = self.path_of_row(row)
            {
                self.armed_cross = Some(ArmedCross { forward, path });
            }
            return;
        };
        // The armed file is normally still there, since a poll that changes the open diff
        // disarms. A poll that dropped the armed file alone leaves the crossing to re-resolve.
        let Some(row) = self.file_row_of_path(&armed.path).or_else(|| self.cross_target(forward))
        else {
            return;
        };
        self.file_cursor = row;
        self.open_cursor_file();
        // The landing hunk reads off the rows now on screen, so a file reshaped since the arm
        // still lands on a real change.
        self.diff_cursor = hunk_row(&self.visible, None, forward).unwrap_or(0);
        self.reveal_files = true;
        self.reveal_diff = true;
    }

    /// Step exactly one added or removed source row, crossing to the nearest changed row in an
    /// adjacent file when needed. Change-run traversal remains independent for compatibility.
    pub fn step_change(&mut self, forward: bool) {
        if !self.is_git_review() {
            self.files_only_unavailable();
            return;
        }
        if !self.can_traverse()
            || self.tab != Tab::Changes
            || self.preview_active()
            || self.image_view_active()
        {
            return;
        }
        let range: Box<dyn Iterator<Item = usize>> = if forward {
            Box::new(self.diff_cursor.saturating_add(1)..self.visible.len())
        } else {
            Box::new((0..self.diff_cursor).rev())
        };
        if let Some(i) = range.into_iter().find(|&i| is_change(&self.visible[i])) {
            self.diff_cursor = i;
            self.reveal_diff = true;
            return;
        }
        let mut row = self.open_file_row();
        while let Some(next) = self.file_row_from(row, forward) {
            row = next;
            self.file_cursor = row;
            self.open_cursor_file();
            let pick = if forward {
                (0..self.visible.len()).find(|&i| is_change(&self.visible[i]))
            } else {
                (0..self.visible.len()).rev().find(|&i| is_change(&self.visible[i]))
            };
            if let Some(i) = pick {
                self.diff_cursor = i;
                self.reveal_files = true;
                self.reveal_diff = true;
                return;
            }
        }
    }

    /// Toggle Changes-only context projection. A live selection or non-source view holds its
    /// exact visible identity, so the control is deliberately inert there.
    pub fn toggle_hide_unchanged(&mut self) {
        if !self.is_git_review() {
            self.files_only_unavailable();
            return;
        }
        if self.tab != Tab::Changes
            || self.preview_active()
            || self.image_view_active()
            || self.select_anchor.is_some()
            || self.diff.state != crate::diff::FileState::Normal
        {
            return;
        }
        self.set_changes_hide_unchanged(!self.hide_unchanged);
    }

    /// The row of the nearest file a crossing would open: the first one that has a hunk. A file
    /// with no hunk — a binary, a pure rename, an over-budget notice — is crossed over, so a
    /// crossing always lands on a change. `None` when no such file lies that way.
    fn cross_target(&mut self, forward: bool) -> Option<usize> {
        // From the open file, never the file cursor: parked on a directory row above the open
        // file, the cursor would find that same file again and wrap the diff to its first hunk.
        let mut row = self.open_file_row();
        while let Some(next) = self.file_row_from(row, forward) {
            row = next;
            let i = self.file_rows[row].file_index().expect("file_row_from yields file rows");
            let entry = self.entries[i].clone();
            // Cross over the files git already counted as having no lines — a binary, a pure
            // rename — without reading them. The reload's `--numstat` knows (`file_list.rs`), so
            // a keystroke that only passes a file by spends no git on it.
            if entry.annotation.as_ref().is_some_and(|a| a.additions + a.deletions == 0) {
                continue;
            }
            // An over-budget file renders a notice, so it holds no hunk either. Check the size
            // before reading, as `set_file_view` does: pulling a vendored bundle in whole would
            // spike the UI thread for a file the reviewer only crosses over.
            if self.is_git_review()
                && std::fs::metadata(self.repo.join(&entry.path))
                    .is_ok_and(|m| crate::diff::over_byte_budget(m.len() as usize))
            {
                continue;
            }
            let (old, new) = self.content_sides(&entry.path, entry.previous_path.as_deref());
            let diff =
                self.cache.get(entry.path, entry.previous_path, &old, &new, &self.highlighter);
            if hunk_row(&diff.rows, None, forward).is_some() {
                return Some(row);
            }
        }
        None
    }

    /// The path of the file at visible row `row`; `None` on a directory row.
    fn path_of_row(&self, row: usize) -> Option<String> {
        let i = self.file_rows.get(row)?.file_index()?;
        Some(self.entries[i].path.clone())
    }

    /// The direction of the crossing the footer is offering, if a hunk step armed one.
    #[must_use]
    pub fn armed_cross(&self) -> Option<bool> {
        self.armed_cross.as_ref().map(|a| a.forward)
    }

    /// Drop an armed crossing. Every input but a repeat of the step that armed it disarms
    /// (`specs/input.md`).
    pub fn disarm_cross(&mut self) {
        self.armed_cross = None;
    }

    /// Toggle the footer's `?` shortcut list. Called only from `Normal` mode, so a modal's `?` stays
    /// text or inert (`specs/input.md`).
    pub fn toggle_keys(&mut self) {
        self.keys_expanded = !self.keys_expanded;
    }

    /// The `esc` ladder in `Normal` mode: peel exactly one layer per press — a live selection, then
    /// an armed crossing, then the footer expansion (`specs/input.md`). The selection and crossing
    /// are file-tab place state, frozen in place while `PR` is active, so `esc` on `PR` closes only
    /// the expansion and never disturbs the file tab the reviewer will return to (`overview.md`
    /// Continuity).
    pub fn escape(&mut self) {
        if self.tab != Tab::Pr {
            if self.select_anchor.is_some() {
                self.clear_selection();
                return;
            }
            if self.armed_cross.is_some() {
                self.armed_cross = None;
                return;
            }
        }
        self.keys_expanded = false;
    }

    /// Whether the traversal keys act at all: a live selection holds the cursor still, since a
    /// jump would silently drop the selection under it (`specs/input.md`).
    fn can_traverse(&self) -> bool {
        self.plugin_config().is_some() && self.select_anchor.is_none() && !self.image_view_active()
    }

    /// The open file's row, the origin of every traversal the diff drives. Falls back to the
    /// cursor when the open file has no visible row, as a file opened from a collapsed
    /// directory does.
    fn open_file_row(&self) -> usize {
        self.diff_path
            .as_deref()
            .and_then(|path| self.file_row_of_path(path))
            .unwrap_or(self.file_cursor)
    }

    /// The visible-row index of the nearest file row past `row`, in `forward`'s direction.
    /// Directory rows are skipped. `None` when no file lies that way, which is how both
    /// traversals clamp at the changeset's ends.
    fn file_row_from(&self, row: usize, forward: bool) -> Option<usize> {
        let is_file = |i: &usize| self.file_rows[*i].file_index().is_some();
        if forward {
            (row + 1..self.file_rows.len()).find(is_file)
        } else {
            (0..row).rev().find(is_file)
        }
    }

    /// Act on a whole file-list row. A directory click selects it and toggles its expansion;
    /// a file click selects and opens it (`specs/file-list.md`).
    pub fn select_file(&mut self, index: usize) -> Result<()> {
        self.ensure_config_ready()?;
        if self.select_anchor.is_some() {
            // Keep the anchor authoritative, but make the mouse no-op legible rather than
            // looking like a missed click (`specs/file-list.md` Selection).
            "clear selection before opening a file".clone_into(&mut self.status);
            return Ok(());
        }
        if index >= self.file_rows.len() {
            return Ok(());
        }
        self.focus = Focus::Files;
        self.file_cursor = index;
        self.reveal_files = true;
        if self.on_folder() {
            self.toggle_dir();
        } else {
            self.open_cursor_file();
        }
        Ok(())
    }

    /// Expand the directory under the cursor, or move onto its first visible child when it is
    /// already open. This is the tree's fixed `→` control (`specs/input.md`).
    pub fn right_dir(&mut self) {
        let Some(path) = self.dir_under_cursor() else { return };
        if !self.dir_expanded(&path) {
            self.expand_dir();
            return;
        }
        let depth = self.file_rows[self.file_cursor].depth;
        if self.file_rows.get(self.file_cursor + 1).is_some_and(|row| row.depth > depth) {
            // The next visible indented row is the first child. Use normal cursor movement so
            // selecting a file still opens it and reveal behavior stays centralized.
            let _ = self.move_cursor(1);
        }
    }

    /// Collapse the directory under the cursor, or move to its nearest visible parent after it
    /// is already closed. This is the tree's fixed `←` control (`specs/input.md`).
    pub fn left_dir(&mut self) {
        let path = if let Some(path) = self.dir_under_cursor() {
            if self.dir_expanded(&path) {
                self.collapse_dir();
                return;
            }
            path
        } else if let Some(index) = self.file_under_cursor_index() {
            self.entries[index].path.clone()
        } else {
            return;
        };
        if let Some(parent) = self.file_rows[..self.file_cursor].iter().rposition(|row| {
            row.dir_path().is_some_and(|candidate| {
                path.strip_prefix(candidate).is_some_and(|rest| rest.starts_with('/'))
            })
        }) {
            self.file_cursor = parent;
            self.reveal_files = true;
        }
    }

    /// Activate the current navigator row. Directories toggle like a whole-row mouse click;
    /// files are selected already, so activation simply ensures their read pane is loaded.
    pub fn activate_file_row(&mut self) {
        if self.on_folder() {
            self.toggle_dir();
        } else if self.focus == Focus::Files {
            self.open_cursor_file();
        }
    }

    /// Collapse or expand the directory under the cursor, then rebuild the tree. The cursor
    /// stays on the directory row (still present, now toggled).
    fn toggle_dir(&mut self) {
        let Some(path) = self.dir_under_cursor() else { return };
        // Flip its membership in the toggled set (toggled = flipped from the tab's default).
        if !self.toggled_dirs.remove(&path) {
            self.toggled_dirs.insert(path);
        }
        self.apply_dir_change();
    }

    /// Whether directory `path` is currently expanded under the active tab's resting state.
    fn dir_expanded(&self, path: &str) -> bool {
        self.default_expanded() ^ self.toggled_dirs.contains(path)
    }

    /// Force directory `path` to `want` (expanded or collapsed); returns whether it changed.
    fn set_dir_expanded(&mut self, path: &str, want: bool) -> bool {
        if self.dir_expanded(path) == want {
            return false;
        }
        if !self.toggled_dirs.remove(path) {
            self.toggled_dirs.insert(path.to_string());
        }
        true
    }

    /// Whether the cursor is on a directory row in the focused file list — the rows `←`/`→`
    /// collapse and expand (elsewhere those keys scroll the diff).
    pub fn on_folder(&self) -> bool {
        self.focus == Focus::Files
            && self.file_rows.get(self.file_cursor).is_some_and(|r| r.dir_path().is_some())
    }

    /// Whether the diff cursor is on a fold row — the row `→` expands (elsewhere `→` scrolls
    /// the diff sideways). Folds are expand-only, so `←` never collapses one.
    pub fn on_fold(&self) -> bool {
        self.focus == Focus::Diff
            && self.visible.get(self.diff_cursor).and_then(Row::fold_anchor).is_some()
    }

    /// Expand the directory under the cursor (`→`); a no-op if it is a file or already open.
    pub fn expand_dir(&mut self) {
        if self.plugin_config().is_none() {
            return;
        }
        if let Some(path) = self.dir_under_cursor()
            && self.set_dir_expanded(&path, true)
        {
            self.apply_dir_change();
        }
    }

    /// Collapse the directory under the cursor (`←`); a no-op if it is a file or already shut.
    pub fn collapse_dir(&mut self) {
        if self.plugin_config().is_none() {
            return;
        }
        if let Some(path) = self.dir_under_cursor()
            && self.set_dir_expanded(&path, false)
        {
            self.apply_dir_change();
        }
    }

    /// The path of the directory row under the cursor, if any.
    fn dir_under_cursor(&self) -> Option<String> {
        self.file_rows.get(self.file_cursor).and_then(|r| r.dir_path()).map(str::to_string)
    }

    /// Rebuild the tree after a directory's expansion changed, keeping the cursor in range.
    fn apply_dir_change(&mut self) {
        if self.repository_mode == RepositoryMode::FilesOnly {
            // Expansion is authored place state. It never reads the filesystem on the event
            // loop; its one-level listing is requested after this frame paints.
            self.raw_tree.epoch = self.raw_tree.epoch.wrapping_add(1);
            if let Some(path) = self.dir_under_cursor() {
                if self.dir_expanded(&path) && !self.raw_tree.listings.contains_key(&path) {
                    self.raw_tree.loading.insert(path.clone());
                    self.status = format!("loading {}", raw_dir_label(&path));
                } else if !self.dir_expanded(&path) {
                    self.raw_tree.loading.remove(&path);
                }
            }
            self.entries = self.materialized_raw_entries();
            self.request_world_refresh(false, true);
        } else if self.tab == Tab::AllFiles
            && let Ok(entries) = crate::world::all_files_entries(&self.world_input(), &self.changed)
        {
            // Git review retains its existing ignored-directory lazy listing behavior.
            self.entries = entries;
        }
        self.rebuild_file_rows();
        self.file_cursor = self.file_cursor.min(self.file_rows.len().saturating_sub(1));
        self.reveal_files = true; // the row may have moved off-screen; pull it back
    }

    /// Wheel-scroll the diff's viewport, leaving `diff_cursor` (the comment anchor) put —
    /// so wheeling to read context never moves what a comment will attach to. The upper
    /// bound is applied each frame by `bound_diff_scroll`.
    pub fn wheel_diff(&mut self, delta: isize) {
        if self.image_view_active() {
            return;
        }
        if self.preview_active() {
            self.preview_scroll_by(delta);
            return;
        }
        if self.visible.is_empty() {
            return;
        }
        self.diff_scroll = offset_by(self.diff_scroll, delta);
    }

    /// Wheel-scroll the file list's viewport, leaving the selection and the open diff
    /// untouched — so browsing the list never reloads a diff. Bounded each frame.
    pub fn wheel_files(&mut self, delta: isize) {
        if self.file_rows.is_empty() {
            return;
        }
        self.file_scroll = offset_by(self.file_scroll, delta);
    }

    /// Extend a mouse drag-selection to the diff line at `index`, anchoring on first drag.
    pub fn drag_select_to(&mut self, index: usize) {
        if !self.is_git_review() || self.preview_active() || self.image_view_active() {
            return;
        }
        if index < self.visible.len() && self.visible[index].is_content() {
            self.focus = Focus::Diff;
            let anchor = *self.select_anchor.get_or_insert(self.diff_cursor);
            if self.visible.get(anchor).is_some_and(Row::is_content) {
                self.diff_cursor = self.fold_clamped(anchor, index);
                self.reveal_diff = true;
            }
        }
    }

    /// Clamp `target` so the inclusive range from `anchor` to `target` crosses no fold: a
    /// selection treats a fold as a hard boundary, so its line range and snippet always agree
    /// (never bracketing hidden lines the snippet omits). Stops the moving end shy of the fold.
    fn fold_clamped(&self, anchor: usize, target: usize) -> usize {
        if target > anchor {
            (anchor + 1..=target).find(|&i| !self.visible[i].is_content()).map_or(target, |i| i - 1)
        } else {
            (target..anchor)
                .rev()
                .find(|&i| !self.visible[i].is_content())
                .map_or(target, |i| i + 1)
        }
    }

    /// Toggle a range-selection anchor at the current diff line.
    pub fn toggle_select(&mut self) {
        if !self.is_git_review()
            || self.preview_active()
            || self.image_view_active()
            || self.focus != Focus::Diff
            || !self.visible.get(self.diff_cursor).is_some_and(Row::is_content)
        {
            return;
        }
        self.select_anchor = match self.select_anchor {
            Some(_) => None,
            None => Some(self.diff_cursor),
        };
        self.reveal_diff = true;
    }

    /// Drop the range-selection anchor (the `esc` clear in the diff); a no-op when none is set.
    pub fn clear_selection(&mut self) {
        if self.select_anchor.is_some() {
            self.select_anchor = None;
            self.reveal_diff = true;
        }
    }

    /// The inclusive `[lo, hi]` diff-line range currently selected.
    pub fn selection_range(&self) -> (usize, usize) {
        match self.select_anchor {
            Some(a) => (a.min(self.diff_cursor), a.max(self.diff_cursor)),
            None => (self.diff_cursor, self.diff_cursor),
        }
    }

    pub fn start_comment(&mut self) {
        if !self.is_git_review() {
            self.files_only_unavailable();
            return;
        }
        if self.preview_active() || self.image_view_active() {
            return; // image and markdown previews are read-only (specs/diff-view.md)
        }
        if self.focus == Focus::Diff
            && self.visible.get(self.diff_cursor).is_some_and(Row::is_content)
        {
            // `c` from browse creates the same singleton selection as a one-line drag.
            if self.select_anchor.is_none() {
                self.select_anchor = Some(self.diff_cursor);
            }
            self.reveal_diff = true; // scroll the anchored line into view before the box opens
            self.input.clear();
            self.caret = 0;
            self.resume_list = false; // a fresh diff comment returns to the diff, not the list
            self.mode = Mode::Composing { editing: None };
        }
    }

    pub fn start_edit(&mut self) {
        if self.image_view_active() {
            return;
        }
        if !self.is_git_review() {
            self.files_only_unavailable();
            return;
        }
        let from_list = self.mode == Mode::List;
        let Some(id) = self.target_comment() else { return };
        self.start_edit_id(id, from_list);
    }

    /// Open the exact card the pointer hit. The UI carries its stable `CommentId`, never the
    /// store's mutable vector position.
    pub fn start_edit_comment(&mut self, id: CommentId) {
        if self.image_view_active() {
            return;
        }
        self.comment_focus = Some(id);
        self.start_edit_id(id, false);
    }

    fn start_edit_id(&mut self, id: CommentId, from_list: bool) {
        if !self.is_git_review()
            || self.image_view_active()
            || (self.preview_active() && !from_list)
        {
            return;
        }
        let Some(c) = self.store.get(id) else { return };
        let (file, text, owning_tab) = (
            c.file.clone(),
            c.text.clone(),
            if c.diff_anchored { Tab::Changes } else { Tab::AllFiles },
        );

        // A list item must enter the representation that authored it. In particular, a Changes
        // anchor cannot be revealed against matching File-view line numbers, or vice versa.
        if self.tab != owning_tab && self.set_tab(owning_tab).is_err() {
            self.status = "could not open comment view".to_string();
            return;
        }
        self.preview = false;
        if self.diff_path.as_deref() != Some(file.as_str())
            && let Some(e) = self.entries.iter().find(|e| e.path == file).cloned()
        {
            self.reset_diff_view();
            self.open_path_in_tab(e.path, e.previous_path);
            if let Some(fi) = self.file_row_of_path(&file) {
                self.file_cursor = fi;
            }
        }

        let resolved = self
            .store
            .get(id)
            .filter(|comment| self.diff_path.as_deref() == Some(comment.file.as_str()))
            .filter(|comment| self.comment_in_view(comment))
            .and_then(|comment| resolve_comment_anchor(comment, &self.visible));
        let Some(last) = resolved else {
            // A stale anchor remains safely stored and exportable, but no editor is rendered
            // below a merely same-numbered replacement line. It can be inspected in the list.
            self.status = "comment is STALE — anchor no longer visible".to_string();
            return;
        };
        self.diff_cursor = last;
        self.select_anchor = None;
        self.comment_focus = Some(id);
        self.focus = Focus::Diff;
        self.reveal_diff = true;
        self.caret = text.chars().count();
        self.input = text;
        self.resume_list = from_list;
        self.mode = Mode::Composing { editing: Some(id) };
    }

    // --- text editing: a character caret into the active field ----------------------------
    // The comment editor and the search input share one control set (specs/input.md,
    // specs/search.md). `caret` is a char index in `0..=text.chars().count()`. Edits
    // round-trip through a `Vec<char>` (both fields are short), so every op is
    // character-wise and multi-byte safe.

    /// The mode's editable text and caret: the comment draft while composing, the search
    /// query, the find query, the base picker's filter, nothing otherwise.
    fn active_field(&mut self) -> Option<(&mut String, &mut usize)> {
        match self.mode {
            Mode::Composing { .. } => Some((&mut self.input, &mut self.caret)),
            Mode::Search => self.search.as_mut().map(|s| (&mut s.query, &mut s.caret)),
            Mode::Find => self.find.as_mut().map(|f| (&mut f.query, &mut f.caret)),
            Mode::BasePick => self.base_picker.as_mut().map(|b| (&mut b.query, &mut b.caret)),
            Mode::Normal
            | Mode::ConfirmDelete { .. }
            | Mode::ConfirmPublish { .. }
            | Mode::SubmitReview { .. }
            | Mode::List
            | Mode::Picker
            | Mode::AssignPicker { .. }
            | Mode::RemoteAssignPicker { .. }
            | Mode::ConfirmRemoteAssign { .. } => None,
        }
    }

    /// Run a character-wise edit on the active field: collect it into a `Vec<char>` with
    /// the caret as an in-range index, hand both to `f`, then reassemble and re-clamp the
    /// caret. Every mutating `input_*` op routes through here, so the guard / collect /
    /// reassemble lives once instead of seven times. A changed search query re-queries
    /// (specs/search.md).
    fn edit_input(&mut self, f: impl FnOnce(&mut Vec<char>, &mut usize)) {
        let searching = self.mode == Mode::Search;
        // The highlighted branch's own row, read before the filter narrows under it.
        let highlighted =
            self.base_picker.as_ref().and_then(|bp| bp.filtered().get(bp.cursor).copied());
        let Some((text, caret_ref)) = self.active_field() else { return };
        let mut v: Vec<char> = text.chars().collect();
        let mut caret = (*caret_ref).min(v.len());
        f(&mut v, &mut caret);
        *caret_ref = caret.min(v.len());
        let edited: String = v.into_iter().collect();
        let changed = *text != edited;
        *text = edited;
        if searching && changed {
            self.search_dirty = true;
        }
        if changed && self.mode == Mode::BasePick {
            self.refilter_base_picker(highlighted);
        }
    }

    /// Re-seat the base picker's highlight after a filter edit: it follows its own row into
    /// the narrowed view when the row survives, else rests on the first match
    /// (`specs/overview.md` Continuity).
    fn refilter_base_picker(&mut self, highlighted: Option<usize>) {
        let Some(bp) = self.base_picker.as_mut() else { return };
        let filtered = bp.filtered();
        bp.cursor = highlighted.and_then(|h| filtered.iter().position(|&i| i == h)).unwrap_or(0);
    }

    /// Move the caret with a function of the current `Vec<char>` view; a no-op without an
    /// active field. The read-only sibling of [`edit_input`](Self::edit_input).
    fn move_caret(&mut self, f: impl FnOnce(&[char], usize) -> usize) {
        if let Some((text, caret)) = self.active_field() {
            let v: Vec<char> = text.chars().collect();
            *caret = f(&v, (*caret).min(v.len()));
        }
    }

    /// Insert `ch` at the caret.
    pub fn input_push(&mut self, ch: char) {
        self.edit_input(|v, caret| {
            v.insert(*caret, ch);
            *caret += 1;
        });
    }

    /// Insert pasted `text` at the caret as one unit, normalizing `\r\n`/`\r` to `\n`. The
    /// single-line search and find queries take a newline as a space (specs/search.md); the
    /// base picker's filter drops it, so a branch name pasted with the newline it was copied
    /// with still matches its branch (specs/input.md).
    pub fn input_paste(&mut self, text: &str) {
        let mut norm = text.replace("\r\n", "\n").replace('\r', "\n");
        match self.mode {
            Mode::Search | Mode::Find => norm = norm.replace('\n', " "),
            // No branch name holds a newline, so a name pasted with the one it was copied
            // with filters as the bare name rather than matching nothing.
            Mode::BasePick => norm.retain(|c| c != '\n'),
            _ => {}
        }
        let norm: Vec<char> = norm.chars().collect();
        self.edit_input(|v, caret| {
            let n = norm.len();
            v.splice(*caret..*caret, norm);
            *caret += n;
        });
    }

    /// Delete the character before the caret.
    pub fn input_backspace(&mut self) {
        self.edit_input(|v, caret| {
            if *caret > 0 {
                v.remove(*caret - 1);
                *caret -= 1;
            }
        });
    }

    /// Delete the character at the caret (`Delete`).
    pub fn input_delete_forward(&mut self) {
        self.edit_input(|v, caret| {
            if *caret < v.len() {
                v.remove(*caret);
            }
        });
    }

    /// Delete the word before the caret (`Ctrl+W`): the trailing whitespace, then the run of
    /// non-whitespace before it, so one press clears one word.
    pub fn input_delete_word(&mut self) {
        self.edit_input(|v, caret| {
            let start = word_start(v, *caret);
            v.drain(start..*caret);
            *caret = start;
        });
    }

    /// Delete from the start of the logical line to the caret (`Ctrl+U`).
    pub fn input_kill_to_start(&mut self) {
        self.edit_input(|v, caret| {
            let start = line_start(v, *caret);
            v.drain(start..*caret);
            *caret = start;
        });
    }

    /// Delete from the caret to the end of the logical line (`Ctrl+K`).
    pub fn input_kill_to_end(&mut self) {
        self.edit_input(|v, caret| {
            let end = line_end(v, *caret);
            v.drain(*caret..end);
        });
    }

    /// Move the caret one character left / right.
    pub fn caret_left(&mut self) {
        self.move_caret(|_, caret| caret.saturating_sub(1));
    }
    pub fn caret_right(&mut self) {
        self.move_caret(|v, caret| (caret + 1).min(v.len()));
    }

    /// Move the caret to the start / end of the logical line (between newlines).
    pub fn caret_home(&mut self) {
        self.move_caret(line_start);
    }
    pub fn caret_end(&mut self) {
        self.move_caret(line_end);
    }

    /// Move the caret one word left / right.
    pub fn caret_word_left(&mut self) {
        self.move_caret(word_start);
    }
    pub fn caret_word_right(&mut self) {
        self.move_caret(word_end);
    }

    pub fn cancel_comment(&mut self) {
        // Cancelling a new draft returns to Browse, so the implicit singleton selection made by
        // `c` cannot linger as a hidden gesture.
        if matches!(self.mode, Mode::Composing { editing: None }) {
            self.select_anchor = None;
        }
        self.leave_compose();
    }

    /// Leave compose mode, returning to the comments-list overlay if the compose was opened
    /// from it (and any comments remain), else to Normal.
    fn leave_compose(&mut self) {
        self.input.clear();
        self.caret = 0;
        let resume = std::mem::take(&mut self.resume_list);
        if resume && !self.store.is_empty() {
            self.list_cursor = self.list_cursor.min(self.store.len() - 1);
            self.mode = Mode::List;
        } else {
            self.mode = Mode::Normal;
        }
    }

    /// Save the in-progress comment — editing the existing one or anchoring a new one
    /// to the selection — then leave compose mode. Blank text cancels instead.
    pub fn submit_comment(&mut self) {
        let Mode::Composing { editing } = self.mode else { return };
        let text = self.input.trim().to_string();
        if text.is_empty() {
            self.cancel_comment();
            return;
        }
        match editing {
            Some(id) => {
                logln!("comment edit [{:?}] :: {text}", id);
                self.store.edit(id, text);
                self.status = "comment updated".to_string();
            }
            None => {
                if let Some(c) = self.build_comment(text) {
                    logln!("comment add {} :: {}", c.location(), c.text);
                    self.store.add(c);
                    self.status = "comment added".to_string();
                }
            }
        }
        self.refresh_github_publishable_comments();
        self.select_anchor = None;
        self.leave_compose();
    }

    /// The `(side, start, end, snippet)` the current selection anchors to.
    fn selection_anchor(&self) -> Option<(Side, u32, u32, String)> {
        let (lo, hi) = self.selection_range();
        anchor(self.visible.get(lo..=hi)?)
    }

    /// Human-readable immutable anchor summary for the dedicated selection action strip.
    pub fn selection_summary(&self) -> Option<String> {
        let (lo, hi) = self.selection_range();
        let selected = self.visible.get(lo..=hi)?;
        let (side, start, end, _) = anchor(selected)?;
        let count = selected.iter().filter(|row| row.is_content()).count();
        let has_old = selected.iter().any(|row| row.old_no().is_some());
        let has_new = selected.iter().any(|row| row.new_no().is_some());
        let side_label = if has_old && has_new {
            "mixed"
        } else if side == Side::Old {
            "old"
        } else {
            "new"
        };
        let range = if start == end { start.to_string() } else { format!("{start}–{end}") };
        Some(format!("{range} · {count} line{} · {side_label}", if count == 1 { "" } else { "s" }))
    }

    fn build_comment(&self, text: String) -> Option<Comment> {
        // Anchor to the file the open diff belongs to (`diff_path`), not the file-list
        // selection — they diverge if the list shifts under a comment in progress.
        let file = self.diff_path.clone()?;
        let (side, start, end, lines) = self.selection_anchor()?;
        // The File view marks every comment as content-anchored, so it ages by file existence,
        // not changeset membership (specs/review-model.md).
        let diff_anchored = self.diff.view == View::Diff;
        Some(Comment {
            file,
            side,
            start,
            end,
            lines,
            text,
            diff_anchored,
            assignment: None,
            github: None,
        })
    }

    /// The `path:line` the composer is anchored to (selection for a new comment,
    /// the existing location when editing). `None` when not composing.
    pub fn pending_location(&self) -> Option<String> {
        match self.mode {
            Mode::Composing { editing: Some(i) } => self.store.get(i).map(Comment::location),
            Mode::Composing { editing: None } => {
                let file = self.diff_path.clone()?;
                let (side, start, end, _) = self.selection_anchor()?;
                // Only `location()` is read here, which ignores `diff_anchored`.
                let c = Comment {
                    file,
                    side,
                    start,
                    end,
                    lines: String::new(),
                    text: String::new(),
                    diff_anchored: true,
                    assignment: None,
                    github: None,
                };
                Some(c.location())
            }
            Mode::Normal
            | Mode::ConfirmDelete { .. }
            | Mode::ConfirmPublish { .. }
            | Mode::SubmitReview { .. }
            | Mode::List
            | Mode::Picker
            | Mode::AssignPicker { .. }
            | Mode::RemoteAssignPicker { .. }
            | Mode::ConfirmRemoteAssign { .. }
            | Mode::BasePick
            | Mode::Search
            | Mode::Find => None,
        }
    }

    /// Whether comment `c` anchors to the pane's current view — a diff comment to the Diff view,
    /// a content comment to the File view. Stops a comment of one kind rendering on, or being
    /// acted on at, an unrelated line in the other tab's view of the same file (the diff's line
    /// numbering and the File view's worktree line numbering differ; specs/review-model.md).
    fn comment_in_view(&self, c: &Comment) -> bool {
        c.diff_anchored == (self.diff.view == View::Diff)
    }

    /// Row indices on the open diff's file that a comment anchors to.
    pub fn commented_lines(&self) -> HashSet<usize> {
        let Some(file) = self.diff_path.clone() else {
            return HashSet::new();
        };
        self.visible
            .iter()
            .enumerate()
            .filter(|(_, row)| {
                self.store.iter().any(|c| {
                    c.file == file
                        && self.comment_in_view(c)
                        && resolve_comment_anchor(c, &self.visible).is_some()
                        && line_in(c, row)
                })
            })
            .map(|(i, _)| i)
            .collect()
    }

    /// For each visible diff row, the stable IDs of exactly the comments whose cards render
    /// after it. The authoritative snippet must match before a card can attach to code.
    pub fn comment_cards(&self) -> Vec<Vec<CommentId>> {
        let mut cards = vec![Vec::new(); self.visible.len()];
        let Some(file) = self.diff_path.as_deref() else { return cards };
        for (id, comment) in self.store.iter_with_ids() {
            if comment.file == file
                && self.comment_in_view(comment)
                && let Some(last) = resolve_comment_anchor(comment, &self.visible)
            {
                cards[last].push(id);
            }
        }
        cards
    }

    fn target_comment(&self) -> Option<CommentId> {
        if self.image_view_active() {
            return None;
        }
        if self.mode == Mode::List {
            return self.comment_focus.or_else(|| self.store.id_at(self.list_cursor));
        }
        self.comment_under_cursor()
    }

    fn comment_under_cursor(&self) -> Option<CommentId> {
        if self.image_view_active() {
            return None;
        }
        let file = self.diff_path.as_deref()?;
        let row = self.visible.get(self.diff_cursor)?;
        let applies = |c: &Comment| {
            c.file == file
                && self.comment_in_view(c)
                && resolve_comment_anchor(c, &self.visible).is_some()
                && line_in(c, row)
        };
        self.comment_focus
            .filter(|&id| self.store.get(id).is_some_and(applies))
            .or_else(|| self.store.iter_with_ids().find_map(|(id, c)| applies(c).then_some(id)))
    }

    /// Begin a deliberate delete for the exact selected card/list item.
    pub fn delete_comment(&mut self) {
        if self.image_view_active() || (self.preview_active() && self.mode != Mode::List) {
            return;
        }
        if let Some(id) = self.target_comment() {
            self.comment_focus = Some(id);
            self.mode = Mode::ConfirmDelete { id };
        }
    }

    pub fn confirm_delete_comment(&mut self) {
        let Mode::ConfirmDelete { id } = self.mode else { return };
        if self.image_view_active() {
            self.mode = Mode::Normal;
            self.comment_focus = None;
            return;
        }
        self.store.take(id);
        self.comment_focus = None;
        self.clamp_list_cursor();
        self.status = "comment deleted".to_string();
        self.mode = Mode::Normal;
    }

    pub fn cancel_delete_comment(&mut self) {
        if matches!(self.mode, Mode::ConfirmDelete { .. }) {
            self.mode = Mode::Normal;
        }
    }

    /// Move through exact resolved comment cards, including several attached to one source row.
    pub fn jump_comment(&mut self, dir: isize) {
        if self.preview_active() || self.image_view_active() {
            return;
        }
        let mut targets: Vec<(usize, CommentId)> = self
            .comment_cards()
            .into_iter()
            .enumerate()
            .flat_map(|(row, ids)| ids.into_iter().map(move |id| (row, id)))
            .collect();
        if targets.is_empty() {
            return;
        }
        targets.sort_by_key(|(row, id)| (*row, *id));
        let current = self
            .comment_focus
            .and_then(|id| targets.iter().position(|&(_, candidate)| candidate == id));
        let position = match (current, dir >= 0) {
            (Some(i), true) => (i + 1) % targets.len(),
            (Some(i), false) => (i + targets.len() - 1) % targets.len(),
            (None, true) => {
                targets.iter().position(|(row, _)| *row > self.diff_cursor).unwrap_or(0)
            }
            (None, false) => targets
                .iter()
                .rposition(|(row, _)| *row < self.diff_cursor)
                .unwrap_or(targets.len() - 1),
        };
        let (row, id) = targets[position];
        self.focus = Focus::Diff;
        self.select_anchor = None;
        self.diff_cursor = row;
        self.comment_focus = Some(id);
        self.reveal_diff = true;
    }

    // --- Search overlay (specs/search.md) ------------------------------------------------

    /// The active scope's annotation for `path` — the search overlay's file rows wear the
    /// same marker and stats as the file list (specs/search.md).
    pub(crate) fn changed_annotation(&self, path: &str) -> Option<&Annotation> {
        self.changed.get(path)
    }

    /// Whether global search may use the Git-worktree search engine. Files-only's retained
    /// descriptor is its only filesystem authority, so it never invokes that pathname-based
    /// scanner or its Git discovery (`specs/search.md`).
    #[must_use]
    pub fn search_available(&self) -> bool {
        self.is_git_review()
    }

    /// `/`: open the search screen in Git review. Files-only keeps in-file find but must not
    /// start the global pathname search engine (`specs/search.md`).
    pub fn open_search(&mut self) {
        if !self.search_available() {
            self.status = "global search unavailable in Files-only mode".to_string();
            return;
        }
        // A navigator-divider drag held from the review view must not become a search-split
        // resize: cancel it so its remaining drag events are consumed, not acted on — the
        // search divider only drags a gesture it started itself (specs/input.md).
        self.cancel_divider_drag();
        self.search = Some(SearchOverlay::new());
        self.mode = Mode::Search;
        // The empty query runs too: the warm engine answers it with its frecency-ranked
        // files, so the screen is useful before the first keystroke.
        self.search_dirty = true;
    }

    /// `esc`: drop the screen whole, place untouched (specs/search.md).
    pub fn close_search(&mut self) {
        if self.mode == Mode::Search {
            self.mode = Mode::Normal;
        }
        self.search = None;
        self.search_dirty = false;
    }

    /// Whether the find band opens: the read pane shows searchable content rows — a file tab, not
    /// the markdown preview, at least one content row. A notice (binary, too large) and an empty
    /// file carry no content rows, so `any(is_content)` excludes them (specs/find-in-file.md).
    pub fn find_available(&self) -> bool {
        self.tab.is_file_tab()
            && !self.preview
            && !self.image_view_active()
            && self.visible.iter().any(Row::is_content)
    }

    /// `ctrl+f`: open the find band over the read pane, inert with nothing to search. Opening is a
    /// fresh gesture — it cancels a held drag, clears a live selection, and focuses the read pane
    /// so the steps land there (specs/find-in-file.md, specs/diff-view.md).
    pub fn open_find(&mut self) {
        if !self.find_available() {
            return;
        }
        self.cancel_divider_drag();
        self.clear_selection();
        self.focus = Focus::Diff;
        self.mode = Mode::Find;
        self.find = Some(Find::default());
        // The band steals the pane's bottom row, so pull the cursor above it — otherwise a cursor
        // on the old last row hides behind the band until the first step (specs/find-in-file.md).
        self.reveal_diff = true;
    }

    /// `esc`: close the band, dropping the query. The cursor stays where the last step left it
    /// (specs/find-in-file.md).
    pub fn close_find(&mut self) {
        if self.mode == Mode::Find {
            self.mode = Mode::Normal;
        }
        self.find = None;
    }

    /// Every match of `query` over the open file in file order, the runs hidden inside folds
    /// included, with the cursor's rank among them (matches strictly before it) and whether the
    /// cursor's own row matches. The current match is the cursor's row when it matches, so both
    /// the count and stepping derive from this walk (specs/find-in-file.md).
    fn find_hits(&self, query: &str) -> (Vec<FindHit>, usize, bool) {
        let cs = find_case_sensitive(query);
        let is_hit = |row: &Row| !find_match_ranges(&row.text(), query, cs).is_empty();
        let mut hits = Vec::new();
        let mut vis = 0usize;
        let mut cursor_rank = 0usize;
        let mut on_match = false;
        for row in &self.diff.rows {
            let expanded = row.fold_anchor().is_some_and(|a| self.expanded_folds.contains(&a));
            match row {
                Row::Fold { lines } if !expanded => {
                    // The collapsed marker sits at `vis`; its lines are hidden, still searched.
                    if vis == self.diff_cursor {
                        cursor_rank = hits.len();
                        on_match = false;
                    }
                    let anchor = row.fold_anchor().expect("a fold has a first hidden line");
                    for line in lines {
                        if is_hit(line) {
                            // Folds hold only context runs, so a folded line has a new-side number.
                            let new_no = line.new_no().expect("a folded row is context");
                            hits.push(FindHit::Folded { anchor, new_no });
                        }
                    }
                    vis += 1;
                }
                Row::Fold { lines } => {
                    // Expanded: its lines are visible rows, inline at `vis`.
                    for line in lines {
                        let m = is_hit(line);
                        if vis == self.diff_cursor {
                            cursor_rank = hits.len();
                            on_match = m;
                        }
                        if m {
                            hits.push(FindHit::Visible(vis));
                        }
                        vis += 1;
                    }
                }
                content => {
                    let m = is_hit(content);
                    if vis == self.diff_cursor {
                        cursor_rank = hits.len();
                        on_match = m;
                    }
                    if m {
                        hits.push(FindHit::Visible(vis));
                    }
                    vis += 1;
                }
            }
        }
        (hits, cursor_rank, on_match)
    }

    /// `enter`/`↓` (`delta > 0`) and `↑` (`delta < 0`): move the cursor to the nearest matching
    /// row below or above it, wrapping. A match in a collapsed fold expands it first, then the
    /// cursor lands on the revealed row. Inert while nothing matches (specs/find-in-file.md).
    pub fn find_step(&mut self, delta: i32) {
        let Some(query) = self.find.as_ref().map(|f| f.query.clone()) else { return };
        if query.is_empty() {
            return;
        }
        let (hits, cursor_rank, on_match) = self.find_hits(&query);
        if hits.is_empty() {
            return;
        }
        let len = hits.len();
        // `cursor_rank` counts matches strictly before the cursor, so a forward step adds `1` to
        // skip a match the cursor already sits on; a backward step never lands on it.
        let target = if delta > 0 {
            (cursor_rank + usize::from(on_match)) % len
        } else {
            (cursor_rank + len - 1) % len
        };
        match hits[target] {
            FindHit::Visible(v) => self.diff_cursor = v,
            FindHit::Folded { anchor, new_no } => {
                self.expanded_folds.insert(anchor);
                self.rebuild_visible();
                // Expanding the fold reveals the context row that held this new-side line number.
                self.diff_cursor = self
                    .visible
                    .iter()
                    .position(|r| r.new_no() == Some(new_no))
                    .expect("the expanded fold reveals the row for this new_no");
            }
        }
        self.reveal_diff = true;
    }

    /// The find band's count: the current match's 1-based ordinal (`None` off a match) and the
    /// total. `None` while the query is empty — the band shows a blank count then
    /// (specs/find-in-file.md).
    pub fn find_count(&self) -> Option<(Option<usize>, usize)> {
        let query = &self.find.as_ref()?.query;
        if query.is_empty() {
            return None;
        }
        let (hits, cursor_rank, on_match) = self.find_hits(query);
        Some((on_match.then_some(cursor_rank + 1), hits.len()))
    }

    /// `tab`: flip the mode, keeping the query. The held results paint at once and the
    /// pick lands on the new mode's first result row (specs/search.md).
    pub fn search_flip(&mut self) {
        if let Some(s) = self.search.as_mut() {
            s.search_mode = match s.search_mode {
                SearchMode::Files => SearchMode::Code,
                SearchMode::Code => SearchMode::Files,
            };
            s.pick = 0;
            s.scroll.set(0);
        }
    }

    /// `↓`/`↑`, `ctrl+n`/`p`: move the pick by `delta`, only while `Ready` (specs/search.md).
    pub fn search_move(&mut self, delta: isize) {
        // Off `Ready` the screen paints a message, not rows, so there is nothing to
        // move onto — the same guard `search_open_pick` and `build_search_preview` uphold.
        if let Some(s) = self.search.as_mut()
            && s.phase == SearchPhase::Ready
        {
            s.pick = step(s.pick, delta, s.picks());
        }
    }

    /// Land one completion. The dispatcher already dropped stale generations; while a
    /// query is in flight the previous results stay painted. A landed set resets the pick
    /// to the first result row (specs/search.md).
    pub fn apply_search_completion(&mut self, completion: crate::search::SearchCompletion) {
        use crate::search::SearchOutcome;
        let Some(s) = self.search.as_mut() else { return };
        match completion.outcome {
            SearchOutcome::Ready(results) => {
                s.results = results;
                s.phase = SearchPhase::Ready;
                s.pick = 0;
                s.scroll.set(0);
            }
            SearchOutcome::Indexing => s.phase = SearchPhase::Indexing,
            SearchOutcome::Failed(e) => {
                s.phase = SearchPhase::Error(e);
                // Drop the last preview so the pane falls back to its notice — a stale file
                // under a red error reads as a result (specs/search.md).
                s.preview = None;
            }
        }
    }

    /// Rebuild the picked result's preview when it no longer matches the pick — idempotent,
    /// so the event loop can call it every settled frame. It runs only with no input pending,
    /// so a pick sweep never waits on it (specs/search.md Preview).
    pub fn build_search_preview(&mut self) {
        let Some(s) = self.search.as_ref() else { return };
        // The pick's target, or `None` when nothing is pickable (off `Ready`, or empty).
        let picked = (s.phase == SearchPhase::Ready).then(|| s.picked()).flatten();
        // Skip when the settled preview already shows this pick — the compare is by reference,
        // so the common no-change frame allocates nothing. A pick move or a landed set makes it
        // differ (rebuild); a poll refreshes the diff in place without moving the pick (skip).
        let shows_pick = match (s.preview.as_ref(), picked.as_ref()) {
            (None, None) => true,
            (Some(pv), Some(PickedResult::File(f))) => pv.hit.is_none() && pv.path == f.path,
            (Some(pv), Some(PickedResult::Code(c))) => {
                pv.path == c.path
                    && pv.hit.as_ref().is_some_and(|(l, sp)| *l == c.line && sp == &c.spans)
            }
            _ => false,
        };
        if shows_pick {
            return;
        }
        let target = picked.map(|picked| match picked {
            PickedResult::File(f) => (f.path.clone(), None),
            PickedResult::Code(c) => (c.path.clone(), Some((c.line, c.spans.clone()))),
        });
        let Some((path, hit)) = target else {
            if let Some(s) = self.search.as_mut() {
                s.preview = None;
            }
            return;
        };
        // A deleted file reads empty and previews empty; an over-budget file previews as the
        // File view's notice (specs/search.md).
        let diff = self.file_view(&path).0;
        if let Some(s) = self.search.as_mut() {
            s.preview = Some(SearchPreview {
                path,
                diff,
                hit,
                scroll: std::cell::Cell::new(0),
                center: std::cell::Cell::new(true),
            });
        }
    }

    /// `PageUp`/`PageDown`: scroll the settled preview. The renderer clamps; the next
    /// pick re-centers (specs/search.md).
    pub fn scroll_search_preview(&mut self, delta: isize) {
        if let Some(p) = self.search.as_ref().and_then(|s| s.preview.as_ref()) {
            p.center.set(false);
            p.scroll.set(p.scroll.get().saturating_add_signed(delta));
        }
    }

    /// A landed poll's preview reconcile: rebuild the previewed file in place, keeping the
    /// scroll. The renderer clamps the scroll and bands the hit only while its line still
    /// exists (specs/search.md, `overview.md` Continuity).
    pub fn refresh_search_preview(&mut self) {
        let Some(path) =
            self.search.as_ref().and_then(|s| s.preview.as_ref()).map(|p| p.path.clone())
        else {
            return;
        };
        let diff = self.file_view(&path).0;
        if let Some(pv) = self.search.as_mut().and_then(|s| s.preview.as_mut()) {
            pv.diff = diff;
        }
    }

    /// `enter`: open the picked result in `All files` whatever tab the search left — the
    /// file in the read pane, the navigator selection onto it, ancestors expanded; a code
    /// pick lands the cursor on its line, clamped into the file's current length. A
    /// vanished path opens nothing and the screen stays (specs/search.md).
    pub fn search_open_pick(&mut self) -> Result<()> {
        let Some(s) = self.search.as_ref() else { return Ok(()) };
        // Off `Ready`, the painted screen shows no rows — held results are stale and
        // invisible, so nothing opens (specs/search.md).
        if s.phase != SearchPhase::Ready {
            return Ok(());
        }
        let (path, line) = match s.picked() {
            Some(PickedResult::File(f)) => (f.path.clone(), None),
            Some(PickedResult::Code(c)) => (c.path.clone(), Some(c.line)),
            None => return Ok(()),
        };
        // Git review validates its worktree path as before. Files-only instead builds the
        // File view through its retained root descriptor before leaving search. This both
        // rejects stale/symlinked results and reuses the successful no-follow read below, so a
        // replacement between a separate preflight and open cannot create a blank escape hatch
        // (specs/search.md, specs/diff-view.md).
        let prepared = if self.repository_mode == RepositoryMode::FilesOnly {
            let Some(view) = self.files_only_file_view(&path) else {
                return Ok(());
            };
            Some(view)
        } else {
            if !self.repo.join(&path).is_file() {
                return Ok(());
            }
            None
        };
        self.close_search();
        // Opening is a deliberate leave: the origin tab stashes its place on the switch,
        // kept for `1`/`2`/`3` (specs/search.md). Files-only is already and always in All
        // files, so it cannot enter a Git tab or scope here.
        if self.repository_mode == RepositoryMode::GitReview && self.tab != Tab::AllFiles {
            self.set_tab(Tab::AllFiles)?;
        }
        // The Git worktree-backed engine records access by a pathname. Files-only deliberately
        // skips that side effect: its selected path remains descriptor-relative through
        // `set_file_view` / `FilesRoot::read_file`, with no root-joined fallback.
        if self.repository_mode == RepositoryMode::GitReview {
            self.search_track = Some(path.clone());
        }
        let mut expanded = false;
        let mut dir = path.as_str();
        while let Some((parent, _)) = dir.rsplit_once('/') {
            expanded |= self.set_dir_expanded(parent, true);
            dir = parent;
        }
        if expanded {
            // Re-flatten the rows only: the picked file is already in `entries` (search
            // never returns an ignored path), so the worktree walk `apply_dir_change`
            // runs for lazy ignored children would block the pick for nothing
            // (policies/ux-responsiveness.md).
            self.rebuild_file_rows();
        }
        self.reset_diff_view();
        // A same-file pick must land on the hit line in source, not behind an open
        // markdown preview (`set_file_view` keeps the choice for a same-path open).
        self.preview = false;
        self.set_file_view_with(&path, prepared);
        if let Some(fi) = self.file_row_of_path(&path) {
            self.file_cursor = fi;
            self.reveal_files = true;
        }
        self.focus = Focus::Diff;
        if let Some(line) = line {
            let last = self.visible.len().saturating_sub(1);
            self.diff_cursor = (line.saturating_sub(1) as usize).min(last);
        }
        self.reveal_diff = true;
        Ok(())
    }

    pub fn open_list(&mut self) {
        if self.image_view_active() {
            return;
        }
        if !self.is_git_review() {
            self.files_only_unavailable();
            return;
        }
        if !self.store.is_empty() {
            self.list_cursor =
                self.comment_focus.and_then(|id| self.store.position_of(id)).unwrap_or(0);
            self.comment_focus = self.store.id_at(self.list_cursor);
            self.list_scroll.set(0);
            self.mode = Mode::List;
        }
    }

    pub fn close_list(&mut self) {
        if self.mode == Mode::List {
            self.mode = Mode::Normal;
        }
    }

    /// The footer's actions for the current context, tagged with their [`Band`] — row 1 (primary,
    /// send, the cursor's `Do` actions) and the `?`-expansion bands (`Go`, `Move`). Pure: a context
    /// → action mapping, unit-tested without a terminal. The renderer packs row 1, spills trimmed
    /// `Do` actions into the `do` band, and wraps the bands below (`specs/input.md`).
    #[must_use]
    pub fn footer_bands(&self) -> Vec<(FooterAction, Band)> {
        use Band::{Do, Go, Move, Primary, Send, Submit};
        use FooterAction as A;

        // This check precedes modal footers: a refresh may turn a previously-confirmed source
        // action into an image view, which must never keep an invisible delete/edit affordance.
        if self.image_view_active() {
            let mut out = vec![(A::Refresh, Primary), (A::Tabs, Go)];
            if !self.file_rows.is_empty() || self.navigator_hidden_here() {
                out.push((A::TogglePane, Go));
            }
            if !self.navigator_hidden_here() {
                out.push((A::NavigatorPosition, Go));
            }
            out.push((A::NavigatorHide, if self.navigator_hidden_here() { Do } else { Go }));
            out.push((A::Quit, Go));
            return out;
        }

        // A modal sub-task owns the whole bar: one row, the primary then its own actions, no `?`
        // and no bands. The escape action comes right after the primary so the exit hint survives a
        // narrow-width trim (trailing `Do` actions drop first).
        match self.mode {
            Mode::Composing { .. } => {
                return vec![(A::Save, Primary), (A::Cancel, Do), (A::Newline, Do)];
            }
            Mode::ConfirmDelete { .. } => {
                return vec![(A::DeleteComment, Primary), (A::Cancel, Do)];
            }
            Mode::ConfirmPublish { .. } => {
                return vec![(A::Publish, Primary), (A::Cancel, Do)];
            }
            Mode::SubmitReview { .. } => {
                return vec![(A::SubmitReview, Primary), (A::Cancel, Do)];
            }
            Mode::ConfirmRemoteAssign { .. } => {
                return vec![(A::PickAgent, Primary), (A::Cancel, Do)];
            }
            Mode::List => {
                let mut actions = vec![
                    (A::Send, Primary),
                    (A::CloseList, Do),
                    (A::Copy, Do),
                    (A::EditComment, Do),
                    (A::DeleteComment, Do),
                ];
                if self.github_publish_cached_available() {
                    actions.push((A::Publish, Do));
                }
                if self.github_submit_cached_available() {
                    actions.push((A::SubmitReview, Submit));
                }
                return actions;
            }
            Mode::Picker | Mode::AssignPicker { .. } | Mode::RemoteAssignPicker { .. } => {
                return vec![(A::PickAgent, Primary), (A::ClosePicker, Do), (A::MovePickerRow, Do)];
            }
            Mode::BasePick => {
                return vec![(A::PickBaseRow, Primary), (A::ClosePicker, Do), (A::MoveBaseRow, Do)];
            }
            Mode::Search => {
                // With nothing pickable — warming, errored, or no matches — only the
                // mode flip and the exit are offered, so the bar never lists a key that
                // would not work (specs/search.md).
                let pickable = self
                    .search
                    .as_ref()
                    .is_some_and(|s| s.phase == SearchPhase::Ready && s.picks() > 0);
                return if pickable {
                    vec![
                        (A::FlipSearchMode, Primary),
                        (A::PickResult, Do),
                        (A::OpenResult, Do),
                        (A::CloseSearch, Do),
                    ]
                } else {
                    vec![(A::FlipSearchMode, Primary), (A::CloseSearch, Do)]
                };
            }
            Mode::Find => {
                // The steps show only with a match to step to, so the bar never lists a key that
                // would not work (specs/find-in-file.md).
                let has_match = self.find_count().is_some_and(|(_, total)| total > 0);
                return if has_match {
                    vec![(A::FindStep, Primary), (A::CloseFind, Do)]
                } else {
                    vec![(A::CloseFind, Primary)]
                };
            }
            Mode::Normal => {}
        }

        // Files-only deliberately has no Git review state to offer. Its navigator and reader
        // retain the normal browser controls, but scopes, PR, comment, export, and change
        // traversal never reach this action model (`specs/overview.md`).
        if !self.is_git_review() {
            let mut out = Vec::new();
            if self.preview_active() && self.focus == Focus::Diff {
                out.push((A::Preview, Primary));
            } else if self.file_rows.is_empty() {
                out.push((A::Refresh, Primary));
            } else if self.focus == Focus::Files {
                match self.file_rows.get(self.file_cursor).map(|r| &r.kind) {
                    Some(RowKind::Dir { expanded: true, .. }) => {
                        out.push((A::CollapseDir, Primary));
                    }
                    Some(RowKind::Dir { expanded: false, .. }) => out.push((A::ExpandDir, Primary)),
                    _ => out.push((A::TogglePane, Primary)),
                }
            } else if self.find_available() {
                out.push((A::Find, Primary));
            } else {
                out.push((A::Refresh, Primary));
            }
            if self.search_available() {
                out.push((A::Search, Go));
            }
            if self.find_available() {
                out.push((A::Find, Go));
            }
            out.push((A::Wrap, Go));
            out.push((A::Refresh, Go));
            if !self.navigator_hidden_here() {
                out.push((A::NavigatorPosition, Go));
            }
            out.push((A::NavigatorHide, if self.navigator_hidden_here() { Do } else { Go }));
            out.push((A::Quit, Go));
            if !self.file_rows.is_empty() {
                out.push((A::MoveLine, Move));
                out.push((A::MoveFile, Move));
                out.push((A::MovePage, Move));
            }
            return out;
        }

        // The read-only PR tab: the state summary leads row 1 (rendered separately); `o open` is the
        // act — available for any resolved PR, not only while a comment is selected, since `o`
        // opens the PR URL itself (`pr_open`). The `go` band carries the always-there keys; `move`
        // carries only the steps the tab has — the PR has no hunk or file steps (`specs/pr-tab.md`).
        if self.tab == Tab::Pr {
            let mut out = Vec::new();
            if self.pr_snapshot().is_some() {
                out.push((A::OpenPr, Primary));
                if self.pr_selected_comment().is_some_and(|comment| {
                    self.pr_forge == git::Forge::GitHub
                        && comment.kind == forge::CommentKind::Finding
                        && !comment.url.is_empty()
                }) {
                    out.push((A::AssignRemote, Do));
                }
            }
            out.push((A::Search, Go));
            out.push((A::TogglePane, Go));
            out.push((A::NavigatorPosition, Go));
            out.push((A::Tabs, Go));
            out.push((A::Refresh, Go));
            out.push((A::Quit, Go));
            out.push((A::MoveLine, Move));
            out.push((A::MovePage, Move));
            return out;
        }

        let mut out: Vec<(FooterAction, Band)> = Vec::new();
        // Whether the diff-jump is already the primary, so the `go` band doesn't repeat the toggle.
        let mut pane_is_primary = false;

        if self.preview_active() && self.focus == Focus::Diff {
            // The read-only preview: the way back to the commentable source leads, and
            // no comment key is offered (specs/input.md); the shared tail below adds the
            // scope, send, and band actions. With the file list focused, the tree's own
            // actions apply instead.
            out.push((A::Preview, Primary));
        } else if self.file_rows.is_empty()
            && self.branch_base.winner.is_none()
            && self.base_pick_available()
        {
            // The `branch` scope with no base: the picker is the way forward, and `b` would
            // re-select the scope already showing, so only the other two offer
            // (`specs/input.md`, `specs/review-model.md`).
            out.push((A::BasePick, Primary));
            out.push((A::ScopeOther, Do));
            out.push((A::Refresh, Do));
        } else if self.file_rows.is_empty() {
            // Nothing in scope to review: only switching scope or refreshing is useful.
            out.push((A::Scope, Primary));
            out.push((A::Refresh, Do));
        } else if self.focus == Focus::Files {
            match self.file_rows.get(self.file_cursor).map(|r| &r.kind) {
                Some(RowKind::Dir { expanded: true, .. }) => out.push((A::CollapseDir, Primary)),
                Some(RowKind::Dir { expanded: false, .. }) => out.push((A::ExpandDir, Primary)),
                _ => {
                    out.push((A::TogglePane, Primary)); // tab into the diff to review
                    pane_is_primary = true;
                }
            }
            // The files pane's calm row 1 has the room for the hide key (specs/input.md).
            out.push((A::NavigatorHide, Do));
        } else if self.visible.is_empty() {
            if self.navigator_hidden_here() {
                // The hidden empty read pane: the way back leads row 1 (specs/input.md).
                out.push((A::NavigatorHide, Primary));
                out.push((A::TogglePane, Do));
            } else {
                // Diff focused but nothing to show (e.g. a binary): only the scope switch helps.
                out.push((A::Scope, Primary));
            }
        } else if self.on_fold() {
            out.push((A::ExpandFold, Primary));
        } else if self.select_anchor.is_some() {
            out.push((A::Comment, Primary));
            out.push((A::ClearSelection, Do));
        } else if self.comment_under_cursor().is_some() {
            out.push((A::EditComment, Primary));
            out.push((A::DeleteComment, Do));
            if self.github_publish_cached_available() {
                out.push((A::Publish, Do));
            }
            out.push((A::JumpComment, Do));
        } else {
            out.push((A::Comment, Primary));
            out.push((A::Select, Do));
            // On a markdown file's source line that previews, surface the way in —
            // otherwise the rendered view is undiscoverable (specs/input.md). A deleted
            // file, holding no current content, offers nothing.
            if self.previewable() {
                out.push((A::Preview, Do));
            }
        }

        // An armed crossing leads row 1: nothing else on screen says the next press leaves the
        // file. The cursor's own action stays, demoted — commenting still works here
        // (specs/input.md).
        if let Some(forward) = self.armed_cross() {
            out[0].1 = Do;
            out.insert(0, (A::CrossFile { forward }, Primary));
        }

        // `send` closes row 1 once a comment is written, after the cursor's actions and before the
        // `?` (the renderer keeps it when a narrow row trims the actions before it).
        if !self.store.is_empty() {
            out.push((A::Send, Send));
        }
        // A submit is discoverable only for this pane's exact, session-owned pending review.
        // It has its own footer band and never consumes local comments.
        if self.github_submit_cached_available() {
            out.push((A::SubmitReview, Submit));
        }

        // The `go` band: the keys that work anywhere. `scope` and `refresh` only when they are not
        // already row-1 actions (the empty / no-diff states above lead with them), and the pane
        // toggle only when it is not the primary — so a band never repeats a row-1 key.
        if !out.iter().any(|&(a, _)| a == A::Scope || a == A::ScopeOther) {
            out.push((A::Scope, Go));
        }
        // The base picker's key shows only where it works (`specs/input.md`).
        if self.base_pick_available() && !out.iter().any(|&(a, _)| a == A::BasePick) {
            out.push((A::BasePick, Go));
        }
        out.push((A::Search, Go));
        // In-file find shows wherever the read pane has content to search (specs/find-in-file.md).
        if self.find_available() {
            out.push((A::Find, Go));
        }
        out.push((A::Wrap, Go));
        if self.tab == Tab::Changes
            && !self.preview_active()
            && self.diff.state == crate::diff::FileState::Normal
        {
            out.push((A::HideUnchanged, Go));
        }
        if !self.store.is_empty() {
            out.push((A::List, Go));
            out.push((A::Copy, Go));
        }
        if !out.iter().any(|&(a, _)| a == A::Refresh) {
            out.push((A::Refresh, Go));
        }
        out.push((A::Tabs, Go));
        // `tab` un-hides while hidden, so it stays offered even with an empty changeset
        // (specs/input.md).
        if !out.iter().any(|&(a, _)| a == A::TogglePane)
            && !pane_is_primary
            && (!self.file_rows.is_empty() || self.navigator_hidden_here())
        {
            out.push((A::TogglePane, Go));
        }
        if !self.navigator_hidden_here() {
            out.push((A::NavigatorPosition, Go));
        }
        if !out.iter().any(|&(a, _)| a == A::NavigatorHide) {
            out.push((A::NavigatorHide, if self.navigator_hidden_here() { Do } else { Go }));
        }
        out.push((A::Quit, Go));

        // The `move` band: the cursor-movement pairs, shown only when there is a changeset to
        // traverse. The hunk step follows `step_hunk`'s own reach — the `Changes` diff only, never a
        // preview — and drops while a crossing is armed, since the armed primary already owns that
        // key (`specs/input.md`).
        if !self.file_rows.is_empty() {
            out.push((A::MoveLine, Move));
            if self.tab == Tab::Changes && !self.preview_active() && self.armed_cross().is_none() {
                out.push((A::MoveHunk, Move));
                out.push((A::MoveChange, Move));
            }
            out.push((A::MoveFile, Move));
            out.push((A::MovePage, Move));
        }
        out
    }

    pub fn list_move(&mut self, delta: isize) {
        if self.mode == Mode::List && !self.store.is_empty() {
            self.list_cursor = step(self.list_cursor, delta, self.store.len());
            self.comment_focus = self.store.id_at(self.list_cursor);
        }
    }

    pub fn list_select(&mut self, row: usize) {
        if self.mode == Mode::List && row < self.store.len() {
            self.list_cursor = row;
            self.comment_focus = self.store.id_at(row);
        }
    }

    /// Enter on a list row opens its exact resolved card without turning it into an editor.
    /// Unresolved anchors remain in the bounded list and report their detached state.
    pub fn open_list_item(&mut self) {
        if self.image_view_active() {
            return;
        }
        let Some(id) = self.target_comment() else { return };
        let Some(comment) = self.store.get(id) else { return };
        let file = comment.file.clone();
        let owner = if comment.diff_anchored { Tab::Changes } else { Tab::AllFiles };
        if self.tab != owner && self.set_tab(owner).is_err() {
            self.status = "could not open comment view".to_string();
            return;
        }
        if self.diff_path.as_deref() != Some(file.as_str())
            && let Some(entry) = self.entries.iter().find(|entry| entry.path == file).cloned()
        {
            self.reset_diff_view();
            self.open_path_in_tab(entry.path, entry.previous_path);
        }
        let resolved = self
            .store
            .get(id)
            .filter(|c| self.comment_in_view(c))
            .and_then(|c| resolve_comment_anchor(c, &self.visible));
        if let Some(row) = resolved {
            self.mode = Mode::Normal;
            self.focus = Focus::Diff;
            self.diff_cursor = row;
            self.comment_focus = Some(id);
            self.reveal_diff = true;
        } else {
            self.status = "STALE — anchor detached; review text in this list".to_string();
        }
    }
}

/// The row the picker's highlight opens on: the agent this session sent to last, else the
/// first row. The last-sent agent counts only while it is still a candidate, so a closed
/// pane falls through (`specs/herdr-host.md`).
fn armed_row(rows: &[AgentChoice], last_sent: Option<&str>) -> usize {
    last_sent.and_then(|pane| rows.iter().position(|row| row.pane_id == pane)).unwrap_or(0)
}

impl App {
    /// `Send`: one agent goes straight out, several open the picker, none refuses and names
    /// the clipboard (`specs/herdr-host.md`). The empty-store refusal is repeated here, ahead
    /// of [`Self::export`]'s own, so `Send` with nothing written shells out to no herdr call
    /// and opens no picker.
    pub fn send_to_agent(&mut self) {
        if self.image_view_active() {
            return;
        }
        if !self.is_git_review() {
            self.files_only_unavailable();
            return;
        }
        if self.store.is_empty() {
            self.status = "no comments to send".to_string();
            return;
        }
        match herdr::send_target() {
            // Every delivery, including one agent, stops at the same explicit confirmation.
            Ok(SendTarget::One(agent)) => self.open_picker(vec![agent]),
            Ok(SendTarget::Many(rows)) => self.open_picker(rows),
            Err(e) => self.open_send_notice(e.to_string()),
        }
    }

    /// Open the picker over `rows`, arming the highlight on the agent this session sent to
    /// last when it is still a candidate, else the first row (`specs/herdr-host.md`).
    pub fn open_picker(&mut self, rows: Vec<AgentChoice>) {
        // A picker with no rows has nothing to choose and no `enter` that acts, and a second open
        // over a live one would capture `Picker` as the mode to restore — either way a modal that
        // swallows every key and that one `esc` cannot leave. The frozen row set also outranks a
        // later one: it is what the reviewer is reading (`specs/herdr-host.md`).
        if rows.is_empty() || self.mode == Mode::Picker {
            return;
        }
        self.picker_cursor = armed_row(&rows, self.last_sent_pane.as_deref());
        self.picker_notice = None;
        self.picker_rows = rows;
        self.picker_over = self.mode.clone();
        self.mode = Mode::Picker;
    }

    /// Close the picker back onto the view it opened over, so a reviewer who sent from the
    /// comments list or with the find band open is not dropped into `Normal` (`specs/input.md`).
    pub fn close_picker(&mut self) {
        if matches!(
            self.mode,
            Mode::Picker | Mode::AssignPicker { .. } | Mode::RemoteAssignPicker { .. }
        ) {
            self.mode = std::mem::replace(&mut self.picker_over, Mode::Normal);
        }
        self.picker_rows.clear();
        self.picker_cursor = 0;
        self.picker_notice = None;
    }

    fn open_send_notice(&mut self, notice: String) {
        if self.mode == Mode::Picker {
            return;
        }
        self.picker_rows.clear();
        self.picker_cursor = 0;
        self.picker_notice = Some(notice);
        self.picker_over = self.mode.clone();
        self.mode = Mode::Picker;
    }

    pub fn picker_move(&mut self, delta: isize) {
        if matches!(
            self.mode,
            Mode::Picker | Mode::AssignPicker { .. } | Mode::RemoteAssignPicker { .. }
        ) && !self.picker_rows.is_empty()
        {
            self.picker_cursor = step(self.picker_cursor, delta, self.picker_rows.len());
        }
    }

    /// Move the highlight to `row`, for a digit key or a click. A row past the end is inert
    /// rather than clamped, so a mistyped digit never arms a neighbour (`specs/input.md`).
    pub fn picker_goto(&mut self, row: usize) {
        if matches!(
            self.mode,
            Mode::Picker | Mode::AssignPicker { .. } | Mode::RemoteAssignPicker { .. }
        ) && row < self.picker_rows.len()
        {
            self.picker_cursor = row;
        }
    }

    /// Send every comment to the highlighted agent, then close whatever the outcome. A
    /// failure reports and keeps the comments, so the reviewer can reopen a fresh picker
    /// rather than retry against a frozen row (`specs/herdr-host.md`).
    pub fn picker_pick(&mut self) {
        let Some(agent) = self.picker_rows.get(self.picker_cursor).cloned() else { return };
        self.close_picker();
        self.export_to_agent(&agent);
    }

    /// Assign the focused comment to one Herdr agent without consuming the review note.
    /// Whether a cached, open GitHub PR can accept publishing this exact local comment. This is
    /// non-mutating but intentionally matches the entry gate, so footers never advertise a
    /// publish action that the confirmation path will reject.
    fn github_publish_cached_available_for(&self, id: Option<CommentId>) -> bool {
        self.is_git_review()
            && self.pr_forge == git::Forge::GitHub
            && self.pr_snapshot().is_some_and(|pr| {
                pr.state == forge::PrState::Open
                    && !pr.head_oid.is_empty()
                    && !pr.base_oid.is_empty()
            })
            && id.is_some_and(|id| self.github_publishable_comments.contains(&id))
    }

    fn github_publish_cached_available(&self) -> bool {
        self.github_publish_cached_available_for(self.target_comment())
    }

    /// Rebuild the render-safe eligibility snapshot from authoritative per-comment diffs.
    /// This is called whenever a file diff refreshes; it deliberately performs no forge or
    /// mutation work, and is the only expensive path behind the immutable footer predicate.
    fn refresh_github_publishable_comments(&mut self) {
        self.github_publishable_comments.clear();
        if !self.is_git_review() {
            return;
        }
        let comments: Vec<(CommentId, Comment)> =
            self.store.iter_with_ids().map(|(id, c)| (id, c.clone())).collect();
        for (id, comment) in comments {
            if comment.diff_anchored
                && comment.side == Side::New
                && comment.start == comment.end
                && self.comment_anchor_is_current(&comment)
            {
                self.github_publishable_comments.insert(id);
            }
        }
    }

    /// Open the explicit confirmation for publishing the focused local comment. Publishing is
    /// deliberately unavailable from stale/non-diff/all-files anchors: Phase 2B only maps one
    /// exact current-side diff line to GitHub's unambiguous RIGHT/line anchor.
    pub fn request_publish_comment(&mut self) {
        if self.image_view_active()
            || !self.is_git_review()
            || self.mode.is_modal() && self.mode != Mode::List
        {
            return;
        }
        if !self.github_publish_cached_available() {
            self.status = "GitHub publish unavailable: no open GitHub PR".into();
            return;
        }
        let Some(id) = self.target_comment() else {
            self.status = "select a resolved diff comment to publish".into();
            return;
        };
        let Some(comment) = self.store.get(id).cloned() else { return };
        if !comment.diff_anchored || comment.side != Side::New || comment.start != comment.end {
            self.status = "GitHub publish requires one current-side diff line".into();
            return;
        }
        if !self.comment_anchor_is_current(&comment) {
            self.status = "GitHub publish requires a resolved current diff anchor".into();
            return;
        }
        self.comment_focus = Some(id);
        self.mode = Mode::ConfirmPublish { id };
    }

    /// Confirm the one-comment GitHub pending-review mutation. This never submits a review and
    /// never removes the local note; the binding is session-only and exact-head keyed.
    pub fn confirm_publish_comment(&mut self) {
        let Mode::ConfirmPublish { id } = self.mode else { return };
        self.mode = Mode::Normal;
        let Some(comment) = self.store.get(id).cloned() else { return };
        let Some(pr) = self.pr_snapshot().cloned() else {
            self.status = "GitHub publish unavailable: no open PR".into();
            return;
        };
        if self.pr_forge != git::Forge::GitHub
            || pr.state != forge::PrState::Open
            || pr.head_oid.is_empty()
            || pr.base_oid.is_empty()
        {
            self.status = "GitHub publish unavailable: no open GitHub PR".into();
            return;
        }
        if !comment.diff_anchored || comment.side != Side::New || comment.start != comment.end {
            self.status = "GitHub publish requires one current-side diff line".into();
            return;
        }
        if !self.github_publish_worktree_clean() {
            self.status =
                "GitHub publish unavailable: commit or stash local changes before publishing"
                    .into();
            return;
        }
        let input =
            match forge::fetch_input(&self.repo, self.base.as_deref(), self.config_snapshot()) {
                Ok(input) => input,
                Err(error) => {
                    self.status = format!("GitHub publish unavailable: {error:?}");
                    return;
                }
            };
        let git::RepositoryIdentity::Repository(ref target) = input.repository else {
            self.status = "GitHub publish unavailable: no GitHub remote".into();
            return;
        };
        if target.forge() != git::Forge::GitHub {
            self.status = "GitHub publish unavailable: remote is not GitHub".into();
            return;
        }
        if input.local.head_oid.as_deref() != Some(pr.head_oid.as_str()) {
            self.status = "GitHub publish unavailable: PR head changed; refresh and retry".into();
            if let Some(stored) = self.store.get_mut(id) {
                stored.github = Some(GitHubReceipt::Failed {
                    message: "PR head changed; refresh and retry".into(),
                });
            }
            return;
        }
        // Probe the provider immediately before the mutation. The cached PR tab snapshot is not
        // sufficient: a force-push can replace the remote head while local HEAD stays unchanged.
        let forge::PrView::Pr(fresh_pr) = forge::fetch(&self.repo, &input) else {
            self.status = "GitHub publish unavailable: refresh PR and retry".into();
            if let Some(stored) = self.store.get_mut(id) {
                stored.github = Some(GitHubReceipt::Failed {
                    message: "PR changed or could not be refreshed; retry".into(),
                });
            }
            return;
        };
        if fresh_pr.number != pr.number
            || fresh_pr.head_oid != pr.head_oid
            || fresh_pr.base_oid.is_empty()
            || fresh_pr.base_oid != pr.base_oid
            || fresh_pr.state != forge::PrState::Open
        {
            self.status = "GitHub publish unavailable: PR changed; refresh and retry".into();
            if let Some(stored) = self.store.get_mut(id) {
                stored.github = Some(GitHubReceipt::Failed {
                    message: "PR changed or could not be refreshed; retry".into(),
                });
            }
            return;
        }
        // The provider probe can block while another agent commits. Re-read local HEAD at the
        // mutation boundary: a clean worktree alone does not prove it still names this PR head.
        let local_after_probe =
            match forge::fetch_input(&self.repo, self.base.as_deref(), self.config_snapshot()) {
                Ok(input) => input.local.head_oid,
                Err(error) => {
                    self.status = format!("GitHub publish unavailable: {error:?}");
                    return;
                }
            };
        if local_after_probe.as_deref() != Some(fresh_pr.head_oid.as_str()) {
            self.status =
                "GitHub publish unavailable: local HEAD changed; refresh and retry".into();
            if let Some(stored) = self.store.get_mut(id) {
                stored.github = Some(GitHubReceipt::Failed {
                    message: "local HEAD changed; refresh and retry".into(),
                });
            }
            return;
        }
        // The provider probe and final HEAD read can both block. Recheck every worktree/index/
        // untracked condition at the write boundary, immediately before deriving a position or
        // invoking the GitHub mutation.
        if !self.github_publish_worktree_clean() {
            self.status =
                "GitHub publish unavailable: commit or stash local changes before publishing"
                    .into();
            return;
        }
        // Use the same base/head pair that the just-confirmed provider snapshot reports.
        let Some(position) = self.github_position_for_comment(&comment, &fresh_pr) else {
            self.status =
                "GitHub publish unavailable: current diff has no stable GitHub position".into();
            return;
        };
        let cancel = std::sync::atomic::AtomicBool::new(false);
        let result = forge::publish_pending_comment(
            &self.repo,
            target.host(),
            target.owner(),
            target.name(),
            pr.number,
            &pr.head_oid,
            self.pending_github_reviews.get(&(
                target.host().to_owned(),
                target.owner().to_owned(),
                target.name().to_owned(),
                pr.number,
                pr.head_oid.clone(),
            )),
            forge::PendingReviewComment { path: &comment.file, position, body: &comment.text },
            &cancel,
        );
        match result {
            Ok(binding) => {
                let receipt = GitHubReceipt::Pending {
                    review_id: binding.review_id.clone(),
                    url: binding.comment_url.clone(),
                };
                let key = (
                    binding.host.clone(),
                    binding.owner.clone(),
                    binding.repository.clone(),
                    binding.number,
                    binding.head_oid.clone(),
                );
                self.pending_github_reviews.insert(key, binding);
                self.recompute_github_submit_availability();
                if let Some(stored) = self.store.get_mut(id) {
                    stored.github = Some(receipt);
                }
                self.status = "Added comment to GitHub pending review — not submitted".into();
            }
            Err(error) => {
                if let Some(stored) = self.store.get_mut(id) {
                    stored.github = Some(GitHubReceipt::Failed { message: format!("{error:?}") });
                }
                self.status = "GitHub publish failed — local comment kept".into();
            }
        }
    }

    pub fn cancel_publish_comment(&mut self) {
        if matches!(self.mode, Mode::ConfirmPublish { .. }) {
            self.mode = Mode::Normal;
        }
    }

    /// Rebuild the pure in-memory `S` eligibility cache when the PR worker observes an identity.
    /// This is deliberately called only from PR/identity-refresh paths or local binding changes:
    /// `footer_bands()` and painting must never run Git or forge commands.
    pub(crate) fn refresh_github_submit_availability(&mut self, input: &forge::PrFetchInput) {
        self.github_submit_target = match &input.repository {
            git::RepositoryIdentity::Repository(target) if target.forge() == git::Forge::GitHub => {
                Some((
                    target.host().to_owned(),
                    target.owner().to_owned(),
                    target.name().to_owned(),
                ))
            }
            _ => None,
        };
        self.recompute_github_submit_availability();
    }

    /// Recompute from cached snapshot, identity, and this pane session's bindings only.
    fn recompute_github_submit_availability(&mut self) {
        self.github_submit_available =
            self.github_submit_target.as_ref().is_some_and(|(host, owner, repository)| {
                self.pr_snapshot().is_some_and(|pr| {
                    self.pr_forge == git::Forge::GitHub
                        && pr.state == forge::PrState::Open
                        && !pr.base_oid.is_empty()
                        && !pr.head_oid.is_empty()
                        && self.pending_github_reviews.contains_key(&(
                            host.clone(),
                            owner.clone(),
                            repository.clone(),
                            pr.number,
                            pr.head_oid.clone(),
                        ))
                })
            });
    }

    /// A render-time predicate only. It reads the PR worker's cached identity and session-owned
    /// binding map; `request_submit_review` and `confirm_submit_review` revalidate externally.
    fn github_submit_cached_available(&self) -> bool {
        self.github_submit_available
    }

    /// Open an explicit submit sheet only for a review this pane created in this session.
    pub fn request_submit_review(&mut self) {
        let Some(pr) = self.pr_snapshot() else {
            self.status = "GitHub submit unavailable: refresh PR and retry".into();
            return;
        };
        if self.pr_forge != git::Forge::GitHub
            || pr.state != forge::PrState::Open
            || pr.base_oid.is_empty()
            || pr.head_oid.is_empty()
        {
            self.status = "GitHub submit unavailable: no open GitHub PR".into();
            return;
        }
        let input =
            match forge::fetch_input(&self.repo, self.base.as_deref(), self.config_snapshot()) {
                Ok(input) => input,
                Err(error) => {
                    self.status = format!("GitHub submit unavailable: {error:?}");
                    return;
                }
            };
        let git::RepositoryIdentity::Repository(target) = input.repository else {
            self.status = "GitHub submit unavailable: no GitHub remote".into();
            return;
        };
        if target.forge() != git::Forge::GitHub {
            self.status = "GitHub submit unavailable: no GitHub remote".into();
            return;
        }
        let key = (
            target.host().to_owned(),
            target.owner().to_owned(),
            target.name().to_owned(),
            pr.number,
            pr.head_oid.clone(),
        );
        if !self.pending_github_reviews.contains_key(&key) {
            self.status =
                "GitHub submit unavailable: no Preview pending review for this PR head".into();
            return;
        }
        self.mode = Mode::SubmitReview { key, event: forge::ReviewEvent::Comment };
    }

    pub fn select_submit_event(&mut self, event: forge::ReviewEvent) {
        if let Mode::SubmitReview { event: selected, .. } = &mut self.mode {
            *selected = event;
        }
    }

    /// The final, bare-Enter-confirmed pending-review submission. Local notes remain intact.
    pub fn confirm_submit_review(&mut self) {
        let Mode::SubmitReview { ref key, event } = self.mode else { return };
        let Some(binding) = self.pending_github_reviews.get(key).cloned() else {
            self.mode = Mode::Normal;
            self.status = "GitHub submit unavailable: pending review changed".into();
            return;
        };
        // A pending review is a forge write just like adding its comments. Its cached binding is
        // never authority: immediately before submit, prove the same open PR/base/head still
        // exists, local HEAD still names it, and no worktree/index/untracked change intervened.
        let Some(cached) = self.pr_snapshot().cloned() else {
            self.mode = Mode::Normal;
            self.status = "GitHub submit unavailable: refresh PR and retry".into();
            return;
        };
        if self.pr_forge != git::Forge::GitHub
            || cached.state != forge::PrState::Open
            || cached.number != binding.number
            || cached.head_oid != binding.head_oid
            || cached.base_oid.is_empty()
            || !self.github_publish_worktree_clean()
        {
            self.mode = Mode::Normal;
            self.status =
                "GitHub submit unavailable: PR or worktree changed; refresh and retry".into();
            return;
        }
        let input =
            match forge::fetch_input(&self.repo, self.base.as_deref(), self.config_snapshot()) {
                Ok(input) => input,
                Err(error) => {
                    self.mode = Mode::Normal;
                    self.status = format!("GitHub submit unavailable: {error:?}");
                    return;
                }
            };
        let git::RepositoryIdentity::Repository(ref target) = input.repository else {
            self.mode = Mode::Normal;
            self.status = "GitHub submit unavailable: no GitHub remote".into();
            return;
        };
        if target.forge() != git::Forge::GitHub
            || target.host() != binding.host
            || target.owner() != binding.owner
            || target.name() != binding.repository
            || input.local.head_oid.as_deref() != Some(binding.head_oid.as_str())
        {
            self.mode = Mode::Normal;
            self.status = "GitHub submit unavailable: PR head changed; refresh and retry".into();
            return;
        }
        let forge::PrView::Pr(fresh) = forge::fetch(&self.repo, &input) else {
            self.mode = Mode::Normal;
            self.status = "GitHub submit unavailable: refresh PR and retry".into();
            return;
        };
        if fresh.state != forge::PrState::Open
            || fresh.number != binding.number
            || fresh.head_oid != binding.head_oid
            || fresh.base_oid.is_empty()
            || fresh.base_oid != cached.base_oid
        {
            self.mode = Mode::Normal;
            self.status = "GitHub submit unavailable: PR changed; refresh and retry".into();
            return;
        }
        let final_input =
            match forge::fetch_input(&self.repo, self.base.as_deref(), self.config_snapshot()) {
                Ok(input) => input,
                Err(error) => {
                    self.mode = Mode::Normal;
                    self.status = format!("GitHub submit unavailable: {error:?}");
                    return;
                }
            };
        if final_input.local.head_oid.as_deref() != Some(binding.head_oid.as_str())
            || !self.github_publish_worktree_clean()
        {
            self.mode = Mode::Normal;
            self.status =
                "GitHub submit unavailable: local state changed; refresh and retry".into();
            return;
        }
        let cancel = std::sync::atomic::AtomicBool::new(false);
        match forge::submit_pending_review(&self.repo, &binding, event, &cancel) {
            Ok(url) => {
                self.pending_github_reviews.remove(key);
                self.recompute_github_submit_availability();
                for (_, comment) in self.store.iter_mut() {
                    if matches!(comment.github, Some(GitHubReceipt::Pending { ref review_id, .. }) if review_id == &binding.review_id)
                    {
                        comment.github = Some(GitHubReceipt::Submitted {
                            review_id: binding.review_id.clone(),
                            url: url.clone(),
                        });
                    }
                }
                self.status = "Submitted GitHub review".into();
            }
            Err(error) => {
                self.status = format!("GitHub submit failed — pending review kept: {error:?}");
            }
        }
        self.mode = Mode::Normal;
    }

    pub fn cancel_submit_review(&mut self) {
        if matches!(self.mode, Mode::SubmitReview { .. }) {
            self.mode = Mode::Normal;
        }
    }

    pub fn assign_comment_to_agent(&mut self) {
        let Some(id) = self.target_comment() else {
            self.status = "select a comment to assign".into();
            return;
        };
        if self.image_view_active() || !self.is_git_review() {
            return;
        }
        match herdr::send_target() {
            Ok(SendTarget::One(agent)) => self.open_assign_picker(id, vec![agent]),
            Ok(SendTarget::Many(rows)) => self.open_assign_picker(id, rows),
            Err(e) => {
                self.status = format!("agent assignment unavailable: {e}");
            }
        }
    }

    /// Open a remote finding assignment picker. The eligible target is captured by value so a
    /// refresh cannot redirect a confirmation to a neighboring newest-first row.
    pub fn assign_remote_thread_to_agent(&mut self) {
        if self.image_view_active() || !self.is_git_review() || self.tab != Tab::Pr {
            return;
        }
        let Some(comment) = self.pr_selected_comment() else {
            self.status = "select a GitHub inline finding to assign".into();
            return;
        };
        if self.pr_forge != git::Forge::GitHub || comment.kind != forge::CommentKind::Finding {
            self.status = "select a GitHub inline finding to assign".into();
            return;
        }
        let thread = RemoteThread::from_comment(comment);
        if thread.url.is_empty() {
            self.status = "remote thread has no GitHub URL; refresh and retry".into();
            return;
        }
        match herdr::send_target() {
            Ok(SendTarget::One(agent)) => self.open_remote_assign_picker(thread, vec![agent]),
            Ok(SendTarget::Many(rows)) => self.open_remote_assign_picker(thread, rows),
            Err(error) => self.status = format!("remote assignment unavailable: {error}"),
        }
    }

    fn open_remote_assign_picker(&mut self, thread: RemoteThread, rows: Vec<AgentChoice>) {
        if rows.is_empty() || self.mode.is_modal() {
            return;
        }
        self.picker_cursor = armed_row(&rows, self.last_sent_pane.as_deref());
        self.picker_rows = rows;
        self.picker_notice = None;
        self.picker_over = self.mode.clone();
        self.mode = Mode::RemoteAssignPicker { thread };
    }

    /// Advance from agent selection to a separate confirmation sheet; no Herdr write happens
    /// merely by choosing a row.
    pub fn remote_assign_picker_pick(&mut self) {
        let Mode::RemoteAssignPicker { thread } = self.mode.clone() else { return };
        let Some(agent) = self.picker_rows.get(self.picker_cursor).cloned() else { return };
        self.picker_rows.clear();
        self.picker_cursor = 0;
        self.picker_notice = None;
        self.mode = Mode::ConfirmRemoteAssign { thread, agent };
    }

    pub fn confirm_remote_thread_assignment(&mut self) {
        let Mode::ConfirmRemoteAssign { thread, agent } = self.mode.clone() else { return };
        let snippet = thread.snippet.as_deref().unwrap_or("(no snippet returned by GitHub)");
        let payload = format!(
            "## Remote GitHub review task from Herdr Preview\n\n**Thread URL:** {}\n**Author:** @{}\n**Location:** {}\n\n**Thread body:**\n{}\n\n**Current snippet:**\n```diff\n{}\n```\n\nPlease inspect this exact remote finding, implement a fix if appropriate, and report validation. Do not submit, approve, request changes, reply to, resolve, or otherwise modify GitHub on my behalf.\n",
            thread.url, thread.author, thread.anchor, thread.body, snippet
        );
        let target = Agent { pane: agent.pane_id.clone(), name: agent.name.clone() };
        let delivered = target.export(&payload).is_ok();
        self.remote_thread_assignments.insert(
            thread.identity(),
            if delivered {
                RemoteThreadReceipt::Delivered { agent: agent.name.clone(), tab: agent.tab.clone() }
            } else {
                RemoteThreadReceipt::Failed { agent: agent.name.clone() }
            },
        );
        self.status = if delivered {
            format!(
                "Assigned remote GitHub thread to {} · {} — GitHub unchanged",
                agent.name, agent.tab
            )
        } else {
            "Agent not found — remote thread assignment kept".into()
        };
        self.mode = std::mem::replace(&mut self.picker_over, Mode::Normal);
    }

    /// The session-only receipt for this raw remote finding, if it was assigned in this pane.
    pub fn remote_thread_receipt(&self, comment: &forge::Comment) -> Option<&RemoteThreadReceipt> {
        self.remote_thread_assignments.get(&RemoteThread::from_comment(comment).identity())
    }

    pub fn cancel_remote_thread_assignment(&mut self) {
        if matches!(self.mode, Mode::ConfirmRemoteAssign { .. }) {
            self.mode = std::mem::replace(&mut self.picker_over, Mode::Normal);
        }
    }

    fn open_assign_picker(&mut self, id: CommentId, rows: Vec<AgentChoice>) {
        // The comments list is a browse modal and is a valid assignment origin. Preserve it in
        // `picker_over` so Esc/delivery returns to the same selected card; other modals own
        // their input and cannot safely be nested.
        if rows.is_empty() || (self.mode.is_modal() && self.mode != Mode::List) {
            return;
        }
        self.picker_cursor = 0;
        self.picker_rows = rows;
        self.picker_notice = None;
        self.picker_over = self.mode.clone();
        self.mode = Mode::AssignPicker { id };
    }

    pub fn assign_picker_pick(&mut self) {
        let Mode::AssignPicker { id } = self.mode.clone() else {
            return;
        };
        let Some(agent) = self.picker_rows.get(self.picker_cursor).cloned() else {
            return;
        };
        let Some(comment) = self.store.get(id).cloned() else {
            self.close_picker();
            return;
        };
        let payload = format!(
            "## Review task from Herdr Preview\n\n**Target:** {}\n\n**Review note:** {}\n\n```diff\n{}\n```\n\nPlease inspect this exact code, implement a fix if appropriate, and report validation. Do not submit a GitHub review on my behalf.\n",
            comment.location(),
            comment.text,
            comment.lines
        );
        let target = Agent { pane: agent.pane_id.clone(), name: agent.name.clone() };
        let delivered = target.export(&payload).is_ok();
        if let Some(stored) = self.store.get_mut(id) {
            stored.assignment = Some(if delivered {
                DeliveryReceipt::Delivered { agent: agent.name.clone(), tab: agent.tab.clone() }
            } else {
                DeliveryReceipt::Failed { agent: agent.name.clone() }
            });
        }
        self.status = if delivered {
            format!("Assigned comment to {} · {} — not submitted", agent.name, agent.tab)
        } else {
            "Agent not found — comment assignment kept".to_string()
        };
        self.close_picker();
    }

    /// Whether the base picker can open here: a file tab, the `branch` scope, and no
    /// `--base` flag (`specs/input.md` Base picker).
    #[must_use]
    pub fn base_pick_available(&self) -> bool {
        self.is_git_review()
            && self.tab.is_file_tab()
            && self.scope == Scope::Branch
            && self.base.is_none()
    }

    /// Open the base picker: one row per branch name, the open PR's target starred first,
    /// the default branch next, the rest by commit recency (`specs/input.md` Base picker).
    /// The highlight opens on the current base, else the first row.
    pub fn open_base_picker(&mut self) {
        if !self.is_git_review() {
            self.files_only_unavailable();
            return;
        }
        if !self.base_pick_available() || self.mode != Mode::Normal {
            return;
        }
        let default = git::default_branch_name(&self.repo).ok().flatten();
        let names = match git::list_branches(&self.repo, default.as_deref()) {
            Ok(names) => names,
            Err(e) => {
                self.status = e.0;
                return;
            }
        };
        if names.is_empty() {
            // Nothing to choose: refuse with the cause, like an empty send
            // (`specs/input.md` Base picker).
            self.status = "no branches to pick".to_string();
            return;
        }
        let target = self
            .pr_snapshot()
            .filter(|s| s.state == forge::PrState::Open)
            .map(|s| s.base_ref.clone());
        let mut rows: Vec<BaseChoice> = names
            .into_iter()
            .map(|name| BaseChoice {
                starred: target.as_deref() == Some(name.as_str()),
                is_default: default.as_deref() == Some(name.as_str()),
                name,
            })
            .collect();
        // A stable sort, so recency still orders the promoted pair and the rest alike.
        rows.sort_by_key(|r| (!r.starred, !r.is_default));
        let current = self.branch_base.winner.as_ref().map(|b| b.name.as_str());
        let cursor = current.and_then(|c| rows.iter().position(|r| r.name == c)).unwrap_or(0);
        self.base_picker = Some(BasePicker { rows, cursor, query: String::new(), caret: 0 });
        self.mode = Mode::BasePick;
    }

    pub fn close_base_picker(&mut self) {
        if self.mode == Mode::BasePick {
            self.mode = Mode::Normal;
        }
        self.base_picker = None;
    }

    /// Move the highlight through the filtered view (`specs/input.md`).
    pub fn base_picker_move(&mut self, delta: isize) {
        let Some(bp) = self.base_picker.as_mut() else { return };
        let len = bp.filtered().len();
        if len > 0 {
            bp.cursor = step(bp.cursor.min(len - 1), delta, len);
        }
    }

    /// Move the highlight to filtered `row`, for a click. A row past the end is inert
    /// (`specs/input.md`).
    pub fn base_picker_goto(&mut self, row: usize) {
        if let Some(bp) = self.base_picker.as_mut()
            && row < bp.filtered().len()
        {
            bp.cursor = row;
        }
    }

    /// Pick the highlighted branch: persist it as the repo pick — or clear the pick when
    /// the highlight is the default branch — then rebuild the changeset against it, so the
    /// list and the header rename together (`specs/input.md`, `specs/review-model.md`).
    /// With no filter match there is no highlight, and `enter` does nothing.
    pub fn base_picker_pick(&mut self) -> Result<()> {
        let Some(bp) = &self.base_picker else { return Ok(()) };
        let Some(&row) = bp.filtered().get(bp.cursor) else { return Ok(()) };
        let choice = bp.rows[row].clone();
        self.close_base_picker();
        let write = if choice.is_default {
            git::clear_base_pick(&self.repo)
        } else {
            git::write_base_pick(&self.repo, &choice.name)
        };
        if let Err(e) = write {
            self.status = e.0;
            return Ok(());
        }
        // Epoch first: any build still in flight read the old pick, and the bump makes its
        // landing fail the input match instead of reverting this one
        // (`crate::world::WorldInput`).
        self.base_epoch = self.base_epoch.wrapping_add(1);
        self.rebase_changes()?;
        self.reveal_files = true;
        Ok(())
    }

    /// Export to one decided pane. Nothing re-resolves it, so a pane that closed while the
    /// picker was open fails here and keeps every comment (`specs/herdr-host.md`). Only a
    /// delivery arms the next picker's highlight, and the pane comes from the row this send
    /// addressed, so `last used` can never name a pane the export did not reach.
    fn export_to_agent(&mut self, agent: &AgentChoice) {
        let target = Agent { pane: agent.pane_id.clone(), name: agent.name.clone() };
        if self.export(&target) {
            self.last_sent_pane = Some(agent.pane_id.clone());
        }
    }

    /// Send/copy every written comment to `target`; consume the whole set only on
    /// success. A failed export leaves all comments in place (`specs/review-model.md`).
    /// Reports whether the comments were delivered.
    pub fn export(&mut self, target: &dyn ExportTarget) -> bool {
        if self.image_view_active() {
            return false;
        }
        if !self.is_git_review() {
            self.files_only_unavailable();
            return false;
        }
        if self.store.is_empty() {
            self.status = "no comments to send".to_string();
            return false;
        }
        let refs: Vec<&Comment> = self.store.iter().collect();
        let text = format_all(&refs);
        let n = refs.len();
        logln!("export ({n}) -> {} ::\n{text}", target.label());
        let delivered = match target.export(&text) {
            Ok(()) => {
                self.store.take_all();
                self.status = target.success_message(n);
                logln!("export OK");
                true
            }
            Err(e) => {
                self.status = if target.label() == "agent" {
                    format!("Agent not found — {n} comments kept")
                } else {
                    target.failure_message()
                };
                logln!("export ERR: {e:#}");
                false
            }
        };
        self.clamp_list_cursor();
        if self.store.is_empty() {
            self.close_list();
        }
        delivered
    }

    /// The number of files changed in the active scope — the header count, the same on both
    /// tabs (specs/tui.md), since `All files` lists the worktree but counts the changeset.
    pub fn changed_count(&self) -> usize {
        self.changed.len()
    }

    /// The scope's aggregate line stats, shown beside the header count (specs/tui.md).
    /// Saturating, so a pathological changeset pins at the cap instead of wrapping.
    pub fn changed_totals(&self) -> (u32, u32) {
        self.changed.values().fold((0, 0), |(added, removed), a| {
            (added.saturating_add(a.additions), removed.saturating_add(a.deletions))
        })
    }

    /// The compact reason an immutable anchor is detached, when it is known from the active
    /// authoritative view. This display state never mutates the stored anchor.
    pub fn stale_reason(&self, c: &Comment) -> Option<&'static str> {
        if c.diff_anchored && !self.changed.contains_key(&c.file) {
            Some("file left Changes")
        } else if !c.diff_anchored
            && (if self.repository_mode == RepositoryMode::FilesOnly {
                self.files_root.as_ref().is_none_or(|root| root.read_file(&c.file, 0).is_err())
            } else {
                !self.repo.join(&c.file).exists()
            })
        {
            Some("file deleted")
        } else if self.diff_path.as_deref() == Some(c.file.as_str())
            && self.comment_in_view(c)
            && resolve_comment_anchor(c, &self.visible).is_none()
        {
            Some("anchor no longer visible")
        } else {
            None
        }
    }

    /// Whether the immutable anchor is unavailable. Coordinates alone can never reattach a
    /// card after a refresh.
    /// Rebuild a diff for the comment's own file before publishing. This is intentionally
    /// independent of the currently open read pane, so a Comments-list selection cannot publish
    /// coordinates that became stale while another file is displayed.
    fn comment_anchor_is_current(&mut self, c: &Comment) -> bool {
        if !c.diff_anchored || c.side != Side::New || c.start != c.end {
            return false;
        }
        let (old, new) = self.content_sides(&c.file, None);
        let rebuilt = self.cache.get(c.file.clone(), None, &old, &new, &self.highlighter);
        resolve_comment_anchor(c, &rebuilt.rows).is_some()
    }

    /// GitHub positions are meaningful only when the tree and index exactly match PR head.
    /// Reject staged, unstaged, and untracked files before any forge mutation.
    fn github_publish_worktree_clean(&self) -> bool {
        Command::new("git")
            .args(["status", "--porcelain=v1", "--untracked-files=all"])
            .current_dir(&self.repo)
            .output()
            .is_ok_and(|output| output.status.success() && output.stdout.is_empty())
    }

    fn github_position_for_comment(&mut self, c: &Comment, pr: &forge::PrSnapshot) -> Option<u32> {
        if !c.diff_anchored || c.side != Side::New || c.start != c.end {
            return None;
        }
        // Keep the local stale-anchor gate, but derive the GitHub position only from the
        // immutable base..head patch. The rendered worktree diff is never a publish source.
        if !self.comment_anchor_is_current(c) {
            return None;
        }
        let marked = c.lines.lines().next()?;
        forge::canonical_patch_position(
            &self.repo,
            &pr.base_oid,
            &pr.head_oid,
            &c.file,
            c.start,
            marked,
        )
    }

    pub fn is_stale(&self, c: &Comment) -> bool {
        self.stale_reason(c).is_some()
    }

    pub fn stale_count(&self) -> usize {
        self.store.iter().filter(|comment| self.is_stale(comment)).count()
    }

    pub fn comment_ordinal(&self, id: CommentId) -> Option<usize> {
        self.store.position_of(id).map(|position| position + 1)
    }

    fn clamp_list_cursor(&mut self) {
        if self.list_cursor >= self.store.len() {
            self.list_cursor = self.store.len().saturating_sub(1);
        }
        if self.comment_focus.is_some_and(|id| self.store.get(id).is_none()) {
            self.comment_focus = self.store.id_at(self.list_cursor);
        }
    }
}

/// Step `cur` by `delta` within `0..n`, clamping at both ends.
fn step(cur: usize, delta: isize, n: usize) -> usize {
    if n == 0 {
        return 0;
    }
    let max = n - 1;
    if delta >= 0 {
        (cur + delta as usize).min(max)
    } else {
        cur.saturating_sub(delta.unsigned_abs())
    }
}

/// Move `scroll` the minimal amount so the row at `cursor` fits within a `viewport`-tall
/// window, given each row's display `heights`. Scrolls up when the cursor is above the top,
/// advances the top until the cursor's row fits, then pulls back so the bottom isn't left
/// blank — the shared "keep the cursor visible" rule for both panes (the file list passes
/// all-height-1 rows, where this degenerates to plain row arithmetic).
fn keep_in_view(cursor: usize, scroll: usize, heights: &[usize], viewport: usize) -> usize {
    if viewport == 0 || heights.is_empty() {
        return 0;
    }
    let cursor = cursor.min(heights.len() - 1);
    let mut top = scroll.min(cursor);
    while top < cursor && heights[top..=cursor].iter().sum::<usize>() > viewport {
        top += 1;
    }
    while top > 0 && heights[top - 1..].iter().sum::<usize>() <= viewport {
        top -= 1;
    }
    top
}

/// Clamp a scroll offset so a `viewport`-tall window over `total` rows shows no blank tail
/// (and 0 when the content fits). Called every frame after any reveal.
fn bound(scroll: usize, total: usize, viewport: usize) -> usize {
    scroll.min(total.saturating_sub(viewport))
}

/// The start of the logical line (after the previous `\n`, or 0) containing char `caret`.
fn line_start(v: &[char], caret: usize) -> usize {
    v[..caret].iter().rposition(|&c| c == '\n').map_or(0, |p| p + 1)
}

/// The end of the logical line (the next `\n`, or the end) containing char `caret`.
fn line_end(v: &[char], caret: usize) -> usize {
    v[caret..].iter().position(|&c| c == '\n').map_or(v.len(), |p| caret + p)
}

/// The start of the word before `caret`: skip trailing whitespace, then the word run.
fn word_start(v: &[char], caret: usize) -> usize {
    let mut i = caret;
    while i > 0 && v[i - 1].is_whitespace() {
        i -= 1;
    }
    while i > 0 && !v[i - 1].is_whitespace() {
        i -= 1;
    }
    i
}

/// The end of the word after `caret`: skip leading whitespace, then the word run.
fn word_end(v: &[char], caret: usize) -> usize {
    let mut i = caret;
    while i < v.len() && v[i].is_whitespace() {
        i += 1;
    }
    while i < v.len() && !v[i].is_whitespace() {
        i += 1;
    }
    i
}

/// Move a scroll offset by `delta` rows, saturating at 0. The upper bound is applied
/// separately by `bound` once the frame's viewport is known.
fn offset_by(scroll: usize, delta: isize) -> usize {
    if delta >= 0 {
        scroll.saturating_add(delta.unsigned_abs())
    } else {
        scroll.saturating_sub(delta.unsigned_abs())
    }
}

/// One scroll step against a per-frame maximum. The base clamps first, so a stale
/// over-max scroll (the pane grew, the content shrank, an entry alignment overshot)
/// still yields to the first upward input; the result stops at the bottom edge.
fn clamp_scroll(base: usize, delta: isize, max: usize) -> usize {
    base.min(max).saturating_add_signed(delta).min(max)
}

/// Whether `row` is one of a hunk's changed lines.
fn is_change(row: &Row) -> bool {
    matches!(row, Row::Deletion { .. } | Row::Insertion { .. })
}

/// The nearest hunk's first changed row in `forward`'s direction: strictly past `from` inside
/// the open file, or from the far end (`None`) in a file being crossed into. A hunk starts at a
/// change row whose predecessor is not one, since context lines or a fold always separate two
/// hunks (specs/diff-view.md).
fn hunk_row(rows: &[Row], from: Option<usize>, forward: bool) -> Option<usize> {
    let starts_hunk = |&i: &usize| is_change(&rows[i]) && (i == 0 || !is_change(&rows[i - 1]));
    if forward {
        (from.map_or(0, |i| i + 1)..rows.len()).find(starts_hunk)
    } else {
        (0..from.unwrap_or(rows.len()).min(rows.len())).rev().find(starts_hunk)
    }
}

/// Whether `path` names a markdown file: a `.md`/`.markdown` extension,
/// case-insensitive (specs/diff-view.md).
fn is_markdown_path(path: &str) -> bool {
    std::path::Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("md") || e.eq_ignore_ascii_case("markdown"))
}

/// Read at most `cap` bytes from a current Git-review worktree file. This is intentionally
/// separate from Files-only authority, whose descriptor-relative method is used above.
fn bounded_worktree_bytes(
    repo: &std::path::Path,
    path: &str,
    cap: usize,
) -> std::io::Result<Vec<u8>> {
    use std::io::Read;
    let file = std::fs::File::open(repo.join(path))?;
    let mut bytes = Vec::with_capacity(cap.min(64 * 1024).saturating_add(1));
    file.take((cap.saturating_add(1)) as u64).read_to_end(&mut bytes)?;
    if bytes.len() > cap {
        return Err(std::io::Error::other("file exceeds preview cap"));
    }
    Ok(bytes)
}

/// The working-tree content of `path`, lossily as UTF-8; empty when the file is
/// absent (a deletion) or unreadable.
fn worktree_content(repo: &std::path::Path, path: &str) -> String {
    std::fs::read(repo.join(path))
        .map(|b| String::from_utf8_lossy(&b).into_owned())
        .unwrap_or_default()
}

fn line_in(c: &Comment, row: &Row) -> bool {
    let no = match c.side {
        Side::New => row.new_no(),
        Side::Old => row.old_no(),
    };
    no.is_some_and(|n| c.start <= n && n <= c.end)
}

/// Resolve an immutable comment anchor against exactly the rows that still carry its original
/// side/range. The reconstructed authoritative snippet must be byte-for-byte equal. This never
/// searches elsewhere for similar text or updates coordinates: an insertion above, replacement
/// at the same line, fold, or owner-view mismatch is unresolved rather than silently rebound.
fn resolve_comment_anchor(c: &Comment, rows: &[Row]) -> Option<usize> {
    let count = c.lines.lines().count();
    if count == 0 {
        return None;
    }
    // The candidate keeps the original exact coordinate range *and* exact marker/text block.
    // Looking only at chunks of the stored length permits a new-side selection that also held
    // preceding deletion rows, without widening the anchor to nearby context.
    (0..=rows.len().saturating_sub(count)).find_map(|first| {
        let last = first + count - 1;
        let candidate = rows.get(first..=last)?;
        if candidate.iter().any(|row| !row.is_content()) {
            return None;
        }
        let (side, start, end, snippet) = anchor(candidate)?;
        (side == c.side && start == c.start && end == c.end && snippet == c.lines).then_some(last)
    })
}

/// Compute `(side, start, end, snippet)` for a selection of diff rows.
///
/// New-side numbers win when present (insertion/context rows); a pure deletion
/// anchors to the old side. The snippet keeps each row's `+`/`−`/space marker.
fn anchor(selected: &[Row]) -> Option<(Side, u32, u32, String)> {
    let mut new: Option<(u32, u32)> = None;
    let mut old: Option<(u32, u32)> = None;
    let mut snippet = String::new();
    for row in selected.iter().filter(|row| row.is_content()) {
        if !snippet.is_empty() {
            snippet.push('\n');
        }
        snippet.push_str(&row.marker_text());
        if let Some(line) = row.new_no() {
            new = Some(new.map_or((line, line), |(min, max)| (min.min(line), max.max(line))));
        }
        if let Some(line) = row.old_no() {
            old = Some(old.map_or((line, line), |(min, max)| (min.min(line), max.max(line))));
        }
    }
    let (side, (start, end)) =
        new.map(|range| (Side::New, range)).or_else(|| old.map(|range| (Side::Old, range)))?;
    Some((side, start, end, snippet))
}

#[cfg(test)]
mod tests {
    use super::{App, Band, FooterAction, Mode, RepositoryMode, Tab};
    use crate::config::NavigatorPosition;
    use crate::diff::{FileState, View};
    use crate::file_list::RowKind;
    use crate::model::{Comment, Scope, Side};
    use ratatui::{Terminal, backend::TestBackend};
    use std::path::PathBuf;

    fn github_input(owner: &str, head: &str) -> crate::forge::PrFetchInput {
        crate::forge::PrFetchInput {
            repository: crate::git::RepositoryIdentity::Repository(
                crate::git::RepoTarget::new("github.com", owner, "repo").unwrap(),
            ),
            origin_repository: None,
            local: crate::git::PrLocalState {
                head_oid: Some(head.into()),
                base_oid: Some("base".into()),
                names: vec!["feature".into()],
                detached: false,
            },
        }
    }

    fn open_pr(head: &str) -> crate::forge::PrSnapshot {
        crate::forge::PrSnapshot {
            number: 1,
            title: String::new(),
            url: String::new(),
            body: String::new(),
            state: crate::forge::PrState::Open,
            is_draft: false,
            head_ref: String::new(),
            head_is_fork: false,
            head_oid: head.into(),
            base_oid: "base".into(),
            base_ref: String::new(),
            merge: crate::forge::Merge::Clean,
            sync: crate::forge::Sync::InSync,
            checks: Vec::new(),
            comments: Vec::new(),
            truncated: false,
        }
    }

    #[test]
    fn submit_footer_uses_only_the_cached_identity_and_session_binding() {
        let mut app = App::blocked(PathBuf::from("."), Scope::Uncommitted, None);
        app.apply_pr(crate::forge::PrView::Pr(Box::new(open_pr("head"))));
        app.refresh_github_submit_availability(&github_input("owner", "head"));
        assert!(!app.github_submit_cached_available(), "a remote review is never adopted");

        let key = ("github.com".into(), "owner".into(), "repo".into(), 1, "head".into());
        app.pending_github_reviews.insert(
            key,
            crate::forge::PendingReviewBinding {
                host: "github.com".into(),
                owner: "owner".into(),
                repository: "repo".into(),
                number: 1,
                head_oid: "head".into(),
                review_id: "review".into(),
                comment_url: None,
            },
        );
        app.recompute_github_submit_availability();
        assert!(app.github_submit_cached_available());
        assert!(app.footer_bands().contains(&(FooterAction::SubmitReview, Band::Submit)));

        // A new PR identity invalidates only the cached affordance; it does not inspect Git and
        // it does not adopt the owner binding that happens to remain in this pane's session map.
        app.refresh_github_submit_availability(&github_input("other", "head"));
        assert!(!app.github_submit_cached_available());
    }

    fn submit_footer_app() -> App {
        let mut app = App::blocked(PathBuf::from("."), Scope::Uncommitted, None);
        app.apply_pr(crate::forge::PrView::Pr(Box::new(open_pr("head"))));
        app.refresh_github_submit_availability(&github_input("owner", "head"));
        app.pending_github_reviews.insert(
            ("github.com".into(), "owner".into(), "repo".into(), 1, "head".into()),
            crate::forge::PendingReviewBinding {
                host: "github.com".into(),
                owner: "owner".into(),
                repository: "repo".into(),
                number: 1,
                head_oid: "head".into(),
                review_id: "review".into(),
                comment_url: None,
            },
        );
        app.recompute_github_submit_availability();
        app
    }

    fn render_text(app: &App, width: u16, height: u16) -> String {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|frame| crate::ui::render(frame, app)).unwrap();
        let buffer = terminal.backend().buffer();
        (0..height)
            .map(|y| (0..width).map(|x| buffer.cell((x, y)).unwrap().symbol()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn submit_review_footer_uses_configured_key_outside_confirmation_in_all_layouts() {
        let app = submit_footer_app();
        let collapsed = render_text(&app, 140, 40);
        assert!(collapsed.contains("S submit review"), "{collapsed}");
        assert!(!collapsed.contains("enter submit"), "{collapsed}");

        let mut expanded_app = submit_footer_app();
        expanded_app.keys_expanded = true;
        let expanded = render_text(&expanded_app, 140, 40);
        assert!(expanded.contains("submit"), "{expanded}");
        assert!(expanded.contains("S submit review"), "{expanded}");
        assert!(!expanded.contains("enter submit"), "{expanded}");

        let narrow = render_text(&app, 30, 12);
        assert!(narrow.contains("S submit review"), "{narrow}");
        assert!(!narrow.contains("enter submit"), "{narrow}");
    }

    #[test]
    fn submit_review_footer_renders_a_non_default_configured_key() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("config.toml"),
            "[keybindings]\nsubmit-review = [\"ctrl+u\"]\n",
        )
        .unwrap();
        let mut app = submit_footer_app();
        app.set_plugin_config(crate::config::plugin_config_in(dir.path()).unwrap());

        let footer = render_text(&app, 140, 40);
        assert!(footer.contains("ctrl+u submit review"), "{footer}");
        assert!(!footer.contains("S submit review"), "{footer}");
    }

    #[test]
    fn submit_review_modal_footer_advertises_enter_submit_not_send() {
        let mut app = App::blocked(PathBuf::from("."), Scope::Uncommitted, None);
        app.mode = Mode::SubmitReview {
            key: ("github.com".into(), "owner".into(), "repo".into(), 1, "head".into()),
            event: crate::forge::ReviewEvent::Comment,
        };
        assert_eq!(
            app.footer_bands(),
            vec![(FooterAction::SubmitReview, Band::Primary), (FooterAction::Cancel, Band::Do)]
        );
        let modal = render_text(&app, 140, 40);
        assert!(modal.contains("enter submit"), "{modal}");
        assert!(!modal.contains("S submit review"), "{modal}");
    }

    #[test]
    fn files_only_folder_root_shows_a_tree_and_opens_nested_content() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("docs/guides")).unwrap();
        std::fs::write(root.path().join("docs/guides/start.md"), "nested content\n").unwrap();

        let mut app = App::new_with_mode(
            root.path().to_path_buf(),
            RepositoryMode::FilesOnly,
            Scope::Uncommitted,
            None,
        );
        app.reload().unwrap();

        assert!(
            matches!(app.file_rows.first().map(|row| &row.kind), Some(RowKind::Dir { path, .. }) if path == "docs"),
            "a root containing only folders must not render as no files: {:?}",
            app.file_rows
        );
        app.expand_dir();
        assert!(app.world_request.is_some(), "expanding queues worker work");
        assert!(
            !app.entries.iter().any(|entry| entry.path == "docs/guides"),
            "the event loop does not enumerate descendants"
        );
        app.reconcile_world(crate::world::build(&app.world_input()).unwrap());
        app.move_cursor(1).unwrap();
        assert!(
            matches!(app.file_rows[app.file_cursor].kind, RowKind::Dir { ref path, .. } if path == "docs/guides")
        );
        app.expand_dir();
        app.reconcile_world(crate::world::build(&app.world_input()).unwrap());
        app.move_cursor(1).unwrap();

        assert_eq!(app.diff_path.as_deref(), Some("docs/guides/start.md"));
        assert_eq!(app.diff.view, View::File);
        assert_eq!(app.diff.state, FileState::Normal);
        assert_eq!(app.visible[0].text(), "nested content");
    }

    #[test]
    fn image_views_leave_markdown_preview_toggle_inert() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("README.md"), "# source\n").unwrap();
        let mut app = App::new_with_mode(
            root.path().to_path_buf(),
            RepositoryMode::FilesOnly,
            Scope::Uncommitted,
            None,
        );
        app.reload().unwrap();
        app.diff_path = Some("README.md".to_owned());
        app.preview_text = "# source\n".to_owned();
        app.visible = vec![crate::diff::Row::Context {
            old_no: 1,
            new_no: 1,
            spans: vec![crate::diff::Span { text: "# source".to_owned(), color: (0, 0, 0) }],
        }];
        app.image_preview_note = Some("SVG preview unavailable");

        app.toggle_preview();

        assert!(!app.preview, "an image fallback cannot arm hidden markdown preview state");
    }

    #[test]
    fn files_only_global_search_is_unavailable_without_creating_a_search_job() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("result.md"), "safe content\n").unwrap();
        let mut app = App::new_with_mode(
            root.path().to_path_buf(),
            RepositoryMode::FilesOnly,
            Scope::Uncommitted,
            None,
        );
        app.reload().unwrap();

        app.open_search();

        assert!(!app.search_available());
        assert_eq!(app.mode, Mode::Normal);
        assert!(app.search.is_none());
        assert!(!app.search_dirty, "the event loop must not start fff-search");
        assert_eq!(app.status, "global search unavailable in Files-only mode");
        assert!(app.find_available(), "in-file find remains available for safe opened content");
    }

    #[test]
    fn files_only_collapse_rejects_an_inflight_directory_listing_and_keeps_cached_subtree_on_error()
    {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("docs")).unwrap();
        std::fs::write(root.path().join("docs/readme.md"), "cached\n").unwrap();
        let mut app = App::new_with_mode(
            root.path().to_path_buf(),
            RepositoryMode::FilesOnly,
            Scope::Uncommitted,
            None,
        );
        app.reload().unwrap();
        app.expand_dir();
        let input = app.world_input();
        let outcome = crate::world::build(&input).unwrap();
        app.collapse_dir();
        assert_ne!(app.world_input(), input, "collapse invalidates the completion tag");
        if app.world_input() == input {
            app.reconcile_world(outcome);
        }
        assert!(
            !app.entries.iter().any(|entry| entry.path == "docs/readme.md"),
            "a collapsed directory cannot paint a stale completion"
        );

        app.expand_dir();
        app.reconcile_world(crate::world::build(&app.world_input()).unwrap());
        assert!(app.entries.iter().any(|entry| entry.path == "docs/readme.md"));
        app.apply_raw_listings(vec![crate::world::DirectoryListing {
            path: "docs".to_string(),
            entries: Err("permission denied".to_string()),
        }]);
        assert!(
            app.raw_tree.listings.contains_key("docs"),
            "a failed refresh retains the previous direct listing"
        );
        assert!(app.entries.iter().any(|entry| entry.path == "docs/readme.md"));
    }

    #[cfg(unix)]
    #[test]
    fn files_only_worker_symlink_error_cannot_materialize_stale_directory_data() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("docs")).unwrap();
        std::fs::write(root.path().join("docs/visible.md"), "visible\n").unwrap();
        std::fs::write(outside.path().join("secret.md"), "secret\n").unwrap();

        let mut app = App::new_with_mode(
            root.path().to_path_buf(),
            RepositoryMode::FilesOnly,
            Scope::Uncommitted,
            None,
        );
        app.reload().unwrap();
        app.expand_dir();
        let input = app.world_input();

        std::fs::remove_dir_all(root.path().join("docs")).unwrap();
        symlink(outside.path(), root.path().join("docs")).unwrap();
        let snapshot = crate::world::build(&input).unwrap();
        assert!(
            snapshot
                .raw_listings
                .as_ref()
                .unwrap()
                .iter()
                .any(|listing| listing.path == "docs" && listing.entries.is_err()),
            "the stale worker request must become a recoverable directory error"
        );
        app.reconcile_world(snapshot);

        assert!(
            !app.entries.iter().any(|entry| entry.path == "docs/secret.md"),
            "a rejected listing cannot add data from the symlink target"
        );
        assert!(
            !app.raw_tree.listings.contains_key("docs"),
            "an invalid listing cannot become a cached subtree"
        );
    }

    #[cfg(unix)]
    #[test]
    fn files_only_file_view_rejects_a_listed_file_replaced_by_a_symlink() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("report.txt"), "local\n").unwrap();
        std::fs::write(outside.path().join("secret.txt"), "outside secret\n").unwrap();
        let mut app = App::new_with_mode(
            root.path().to_path_buf(),
            RepositoryMode::FilesOnly,
            Scope::Uncommitted,
            None,
        );
        app.reload().unwrap();
        assert!(app.entries.iter().any(|entry| entry.path == "report.txt"));

        std::fs::remove_file(root.path().join("report.txt")).unwrap();
        symlink(outside.path().join("secret.txt"), root.path().join("report.txt")).unwrap();
        let (diff, content) = app.file_view("report.txt");
        assert!(content.is_empty(), "Files-only must not read the symlink target");
        assert!(diff.rows.is_empty(), "the rejected read paints no external bytes");
    }

    #[test]
    fn the_read_pane_scroll_stops_at_the_bottom_edge() {
        let mut app = App::blocked(PathBuf::from("."), Scope::Uncommitted, None);
        app.note_pr_read_max_scroll(4);
        app.pr_scroll_read(100);
        assert_eq!(app.pr_read_scroll, 4, "scroll stops with the last line at the pane edge");
        app.pr_scroll_read(-1);
        assert_eq!(app.pr_read_scroll, 3, "no dead zone above the clamp");
        app.note_pr_read_max_scroll(0);
        app.pr_scroll_read(5);
        assert_eq!(app.pr_read_scroll, 0, "content that fits the pane does not scroll");
    }

    #[test]
    fn config_recovery_carries_an_open_preview() {
        let mut old = App::blocked(PathBuf::from("."), Scope::Uncommitted, None);
        old.mode = Mode::List;
        old.preview = true;
        old.preview_scroll = 7;
        old.preview_text = "# doc".to_string();

        let mut recovered = App::new(PathBuf::from("."), Scope::Uncommitted, None);
        recovered.carry_authored_state_from(&mut old);

        assert!(recovered.preview, "the preview choice survives config recovery");
        assert_eq!(recovered.preview_scroll, 7);
        assert_eq!(recovered.preview_text(), "# doc");
    }

    #[test]
    fn config_recovery_carries_the_last_sent_agent() {
        // The `last used` arming is session memory: a config error between two sends must
        // not move the next picker's default (`specs/herdr-host.md`).
        let mut old = App::blocked(PathBuf::from("."), Scope::Uncommitted, None);
        old.last_sent_pane = Some("w8:p2".to_string());
        old.mode = Mode::Picker;

        let mut recovered = App::new(PathBuf::from("."), Scope::Uncommitted, None);
        recovered.carry_authored_state_from(&mut old);
        assert_eq!(recovered.last_sent_pane.as_deref(), Some("w8:p2"));
        // A picker that was open when the config broke does not come back with it.
        assert_eq!(recovered.mode, Mode::Normal);
    }

    #[test]
    fn config_recovery_carries_the_base_picker_whole() {
        // The base picker survives recovery with its rows, filter, and highlight
        // (`specs/tui.md`).
        let mut old = App::blocked(PathBuf::from("."), Scope::Branch, None);
        old.mode = Mode::BasePick;
        old.base_picker = Some(super::BasePicker {
            rows: vec![super::BaseChoice {
                name: "dev".to_string(),
                starred: false,
                is_default: false,
            }],
            cursor: 0,
            query: "d".to_string(),
            caret: 1,
        });
        old.branch_base = crate::git::BaseStatus {
            winner: Some(crate::git::ResolvedBase {
                name: "main".to_string(),
                oid: "0".repeat(40),
            }),
            skipped: None,
        };

        let mut recovered = App::new(PathBuf::from("."), Scope::Branch, None);
        recovered.carry_authored_state_from(&mut old);
        assert_eq!(recovered.mode, Mode::BasePick);
        let bp = recovered.base_picker.as_ref().expect("the picker state is carried");
        assert_eq!(bp.query, "d");
        assert_eq!(bp.rows[0].name, "dev");
        // The header's base rides with the carried frame — recovery never paints
        // `no base` beside a populated list (`specs/tui.md`).
        let base = recovered.branch_base.winner.as_ref().expect("the resolved base is carried");
        assert_eq!(base.name, "main");
    }

    #[test]
    fn config_recovery_carries_the_footer_expansion() {
        // The `?` expansion is one global toggle, carried whatever mode recovery finds.
        let mut old = App::blocked(PathBuf::from("."), Scope::Uncommitted, None);
        old.keys_expanded = true;

        let mut recovered = App::new(PathBuf::from("."), Scope::Uncommitted, None);
        assert!(!recovered.keys_expanded, "a fresh app opens collapsed");
        recovered.carry_authored_state_from(&mut old);
        assert!(recovered.keys_expanded, "the expansion survives config recovery");
    }

    #[test]
    fn config_recovery_carries_saved_comments_and_the_live_draft() {
        let mut old = App::blocked(PathBuf::from("."), Scope::Uncommitted, None);
        old.store.add(Comment {
            file: "src/lib.rs".to_string(),
            side: Side::New,
            start: 1,
            end: 1,
            lines: "+line".to_string(),
            text: "saved".to_string(),
            diff_anchored: true,
            assignment: None,
            github: None,
        });
        old.mode = Mode::Composing { editing: None };
        old.resume_list = true;
        old.input = "draft".to_string();
        old.caret = 3;

        let mut recovered = App::new(PathBuf::from("."), Scope::Uncommitted, None);
        recovered.carry_authored_state_from(&mut old);

        assert_eq!(recovered.store.len(), 1);
        assert_eq!(recovered.input, "draft");
        assert_eq!(recovered.caret, 3);
        assert!(recovered.resume_list);
        assert!(matches!(recovered.mode, Mode::Composing { editing: None }));
    }

    #[test]
    fn config_recovery_keeps_the_comment_list_view_and_navigation() {
        let mut old = App::blocked(PathBuf::from("."), Scope::Branch, None);
        old.mode = Mode::List;
        old.file_cursor = 4;
        old.file_scroll = 2;
        old.diff_cursor = 8;
        old.diff_scroll = 5;
        old.input = "unsent".to_string();

        let mut recovered = App::new(PathBuf::from("."), Scope::Uncommitted, None);
        recovered.carry_authored_state_from(&mut old);

        assert!(matches!(recovered.mode, Mode::List));
        assert_eq!(recovered.scope, Scope::Branch);
        assert_eq!(recovered.file_cursor, 4);
        assert_eq!(recovered.file_scroll, 2);
        assert_eq!(recovered.diff_cursor, 8);
        assert_eq!(recovered.diff_scroll, 5);
        assert_eq!(recovered.input, "unsent");
    }

    #[test]
    fn config_recovery_carries_a_pending_world_request() {
        // A tab switch requested its refresh, then recovery landed first: the carried flags
        // are what make the recovered app dispatch that refresh instead of keeping the
        // stale stashed frame until the next poll.
        let mut old = App::blocked(PathBuf::from("."), Scope::Uncommitted, None);
        old.request_world_refresh(true, true);
        let mut recovered = App::new(PathBuf::from("."), Scope::Uncommitted, None);
        recovered.carry_authored_state_from(&mut old);
        let request = recovered.world_request.expect("the pending refresh survives the swap");
        assert!(request.sample_turn, "the poll's sample flag survives the recovery swap");
        assert!(request.reveal, "the switch's reveal flag survives the recovery swap");
    }

    #[test]
    fn config_recovery_keeps_both_shares_and_reapplies_the_configured_position() {
        let mut old = App::blocked(PathBuf::from("."), Scope::Uncommitted, None);
        old.navigator_position = NavigatorPosition::Top;
        old.navigator_side_pct = 41;
        old.navigator_stack_pct = 37;

        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("config.toml"), "navigator_position = \"left\"\n").unwrap();
        let config = crate::config::plugin_config_in(dir.path()).unwrap();
        let mut recovered = App::new(PathBuf::from("."), Scope::Uncommitted, None);
        recovered.set_plugin_config(config);
        recovered.carry_authored_state_from(&mut old);

        assert_eq!(recovered.navigator_position, NavigatorPosition::Left);
        assert_eq!(recovered.navigator_side_pct, 41);
        assert_eq!(recovered.navigator_stack_pct, 37);
    }

    #[test]
    fn config_recovery_keeps_the_hidden_navigator() {
        let mut old = App::blocked(PathBuf::from("."), Scope::Uncommitted, None);
        old.navigator_hidden = true;

        let mut recovered = App::new(PathBuf::from("."), Scope::Uncommitted, None);
        recovered.carry_authored_state_from(&mut old);

        assert!(recovered.navigator_hidden, "the hidden state survives config recovery");
        assert_eq!(recovered.focus, crate::Focus::Diff, "and focus lands on the read pane");
    }

    #[test]
    fn config_recovery_preserves_changes_hide_unchanged_and_reconciles_the_cursor() {
        use crate::diff::{FileDiff, FileState, Row, Span, View};
        let text = |s: &str| vec![Span { text: s.to_string(), color: (0, 0, 0) }];
        let rows = vec![
            Row::Context { old_no: 1, new_no: 1, spans: text("before") },
            Row::Insertion { new_no: 2, spans: text("changed"), emphasis: vec![] },
            Row::Context { old_no: 3, new_no: 3, spans: text("after") },
        ];
        let mut old = App::blocked(PathBuf::from("."), Scope::Uncommitted, None);
        // The active file tab is All files, so the Changes preference is in its stash.
        old.active_file_tab = Tab::AllFiles;
        old.stash.hide_unchanged = true;

        // A recovery worker has already rebuilt this current file under the normal projection.
        // Reapplying the user's Changes preference must fold its context without displacing the
        // cursor from the changed source row.
        let mut recovered = App::new(PathBuf::from("."), Scope::Uncommitted, None);
        recovered.diff = FileDiff {
            path: "a.rs".to_string(),
            previous_path: None,
            state: FileState::Normal,
            view: View::Diff,
            rows: rows.clone(),
        };
        recovered.visible = rows;
        recovered.diff_cursor = 1;

        recovered.carry_authored_state_from(&mut old);

        assert!(recovered.hide_unchanged);
        assert!(matches!(recovered.visible[0], Row::Fold { .. }));
        assert!(matches!(recovered.visible[1], Row::Insertion { .. }));
        assert!(matches!(recovered.visible[2], Row::Fold { .. }));
        assert_eq!(recovered.diff_cursor, 1, "the changed row survives projection by identity");
    }

    #[test]
    fn hide_unchanged_projects_context_into_expandable_folds_without_losing_rows() {
        use crate::diff::{FileDiff, FileState, Row, Span, View};
        let text = |s: &str| vec![Span { text: s.to_string(), color: (0, 0, 0) }];
        let mut app = App::blocked(PathBuf::from("."), Scope::Uncommitted, None);
        app.diff = FileDiff {
            path: "a.rs".to_string(),
            previous_path: None,
            state: FileState::Normal,
            view: View::Diff,
            rows: vec![
                Row::Context { old_no: 1, new_no: 1, spans: text("before") },
                Row::Deletion { old_no: 2, spans: text("old"), emphasis: vec![] },
                Row::Insertion { new_no: 2, spans: text("new"), emphasis: vec![] },
                Row::Context { old_no: 3, new_no: 3, spans: text("after") },
            ],
        };
        app.hide_unchanged = true;
        app.rebuild_visible();
        assert!(matches!(app.visible[0], Row::Fold { .. }));
        assert!(matches!(app.visible[1], Row::Deletion { .. }));
        assert!(matches!(app.visible[2], Row::Insertion { .. }));
        assert!(matches!(app.visible[3], Row::Fold { .. }));
        app.diff_cursor = 0;
        app.expand_fold(&[], 1);
        assert!(matches!(app.visible[0], Row::Context { .. }), "one fold expands locally");
        assert_eq!(app.visible[3].hidden(), 1, "the independent trailing context stays folded");
    }

    #[test]
    fn blocked_app_rejects_normal_repository_work_without_panicking() {
        let mut app = App::blocked(PathBuf::from("."), Scope::Uncommitted, None);
        app.set_config_error("bad config".to_string());

        assert!(app.reload().unwrap_err().to_string().contains("bad config"));
        assert!(app.set_scope(Scope::Branch).is_err());
        assert!(app.set_tab(super::Tab::AllFiles).is_err());
        assert!(app.move_cursor(1).is_err());
        assert!(app.select_file(0).is_err());
    }
}

//! In-memory review model: scopes, changed files, and comments.
//!
//! See `specs/review-model.md`. Comments live only for the session and are
//! removed by export or delete — never by a refresh.

/// Which set of changes the Changes view shows.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Scope {
    Uncommitted,
    Branch,
    LastTurn,
}

impl Scope {
    pub fn label(self) -> &'static str {
        match self {
            Self::Uncommitted => "uncommitted",
            Self::Branch => "branch",
            Self::LastTurn => "last turn",
        }
    }

    /// The `default_scope` spelling, unlike the header's spaced [`Self::label`].
    pub fn name(self) -> &'static str {
        match self {
            Self::Uncommitted => "uncommitted",
            Self::Branch => "branch",
            Self::LastTurn => "last-turn",
        }
    }

    #[must_use]
    pub fn cycle(self) -> Self {
        match self {
            Self::Uncommitted => Self::Branch,
            Self::Branch => Self::LastTurn,
            Self::LastTurn => Self::Uncommitted,
        }
    }
}

/// How a file changed within a scope.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ChangeKind {
    Added,
    Modified,
    Deleted,
    Renamed,
    Untracked,
}

impl ChangeKind {
    pub fn marker(self) -> char {
        match self {
            Self::Added => 'A',
            Self::Modified => 'M',
            Self::Deleted => 'D',
            Self::Renamed => 'R',
            Self::Untracked => '?',
        }
    }
}

/// A row in the Changes list.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ChangedFile {
    pub path: String,
    pub kind: ChangeKind,
    pub additions: u32,
    pub deletions: u32,
    pub previous_path: Option<String>,
}

/// Which side of the diff a comment's lines live on.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Side {
    New,
    Old,
}

/// Stable, session-local identity for one comment.
///
/// It is deliberately not a worktree or persisted identifier. A card, composer, list item, and
/// confirmation carry this value and resolve it through [`CommentStore`] when they act, so a
/// deletion cannot make a retained vector position refer to another comment.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
pub struct CommentId(u64);

impl CommentId {
    #[must_use]
    pub fn ordinal(self) -> u64 {
        self.0
    }
}

/// A reviewer comment anchored to a run of diff lines, carrying the snippet.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum DeliveryReceipt {
    Delivered { agent: String, tab: String },
    Failed { agent: String },
}

/// The session-local outcome of publishing a local comment into Preview's pending GitHub review.
/// It deliberately does not replace the snippet-authoritative local anchor or agent receipt.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum GitHubReceipt {
    Pending { review_id: String, url: Option<String> },
    Failed { message: String },
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Comment {
    pub file: String,
    pub side: Side,
    pub start: u32,
    pub end: u32,
    /// Verbatim diff lines the comment anchors to, each keeping its `+`/`-`/space marker.
    pub lines: String,
    pub text: String,
    /// True when anchored to a diff (the `Changes` tab); false for a File-view content comment.
    pub diff_anchored: bool,
    /// Per-comment agent handoff; session-local and never consumes the review note.
    pub assignment: Option<DeliveryReceipt>,
    /// Per-comment GitHub pending-review receipt. A publish never consumes this local comment.
    pub github: Option<GitHubReceipt>,
}

impl Comment {
    pub fn location(&self) -> String {
        let range = if self.start == self.end {
            format!("{}:{}", self.file, self.start)
        } else {
            format!("{}:{}-{}", self.file, self.start, self.end)
        };
        match self.side {
            Side::New => range,
            Side::Old => format!("{range} (removed)"),
        }
    }
}

#[derive(Debug)]
struct StoredComment {
    id: CommentId,
    comment: Comment,
}

/// A key accepted by the store's compatibility accessors. New UI state carries `CommentId`;
/// the `usize` arm exists for compact fixture inspection only and is never an interaction id.
#[derive(Debug)]
pub enum CommentKey {
    Id(CommentId),
    Position(usize),
}

impl From<CommentId> for CommentKey {
    fn from(value: CommentId) -> Self {
        Self::Id(value)
    }
}

impl From<usize> for CommentKey {
    fn from(value: usize) -> Self {
        Self::Position(value)
    }
}

/// The in-memory comment list for one worktree review session.
#[derive(Debug)]
pub struct CommentStore {
    items: Vec<StoredComment>,
    next_id: u64,
}

impl Default for CommentStore {
    fn default() -> Self {
        Self { items: Vec::new(), next_id: 1 }
    }
}

impl CommentStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = &Comment> {
        self.items.iter().map(|item| &item.comment)
    }

    pub fn iter_with_ids(&self) -> impl Iterator<Item = (CommentId, &Comment)> {
        self.items.iter().map(|item| (item.id, &item.comment))
    }

    pub fn id_at(&self, position: usize) -> Option<CommentId> {
        self.items.get(position).map(|item| item.id)
    }

    pub fn position_of(&self, id: CommentId) -> Option<usize> {
        self.items.iter().position(|item| item.id == id)
    }

    pub fn get(&self, key: impl Into<CommentKey>) -> Option<&Comment> {
        match key.into() {
            CommentKey::Id(id) => {
                self.items.iter().find(|item| item.id == id).map(|item| &item.comment)
            }
            CommentKey::Position(position) => self.items.get(position).map(|item| &item.comment),
        }
    }

    /// Append a comment and return its monotonically assigned, session-local identity.
    pub fn add(&mut self, comment: Comment) -> CommentId {
        let id = CommentId(self.next_id);
        self.next_id = self.next_id.checked_add(1).expect("comment id space exhausted");
        self.items.push(StoredComment { id, comment });
        id
    }

    pub fn edit(&mut self, id: CommentId, text: String) -> bool {
        if let Some(item) = self.items.iter_mut().find(|item| item.id == id) {
            item.comment.text = text;
            true
        } else {
            false
        }
    }

    pub fn get_mut(&mut self, id: CommentId) -> Option<&mut Comment> {
        self.items.iter_mut().find(|item| item.id == id).map(|item| &mut item.comment)
    }

    pub fn take(&mut self, id: CommentId) -> Option<Comment> {
        self.position_of(id).map(|position| self.items.remove(position).comment)
    }

    pub fn take_all(&mut self) -> Vec<Comment> {
        std::mem::take(&mut self.items).into_iter().map(|item| item.comment).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::{Comment, CommentStore, Scope, Side};

    fn comment(file: &str, start: u32, end: u32, text: &str) -> Comment {
        Comment {
            file: file.into(),
            side: Side::New,
            start,
            end,
            lines: "+x".into(),
            text: text.into(),
            diff_anchored: true,
            assignment: None,
            github: None,
        }
    }

    #[test]
    fn scope_cycles_and_labels() {
        assert_eq!(Scope::Uncommitted.cycle(), Scope::Branch);
        assert_eq!(Scope::Branch.cycle(), Scope::LastTurn);
        assert_eq!(Scope::Uncommitted.label(), "uncommitted");
        assert_eq!(Scope::LastTurn.label(), "last turn");
    }

    #[test]
    fn location_formats_range_single_and_removed() {
        let mut c = comment("a.rs", 40, 52, "x");
        assert_eq!(c.location(), "a.rs:40-52");
        c.end = 40;
        assert_eq!(c.location(), "a.rs:40");
        c.side = Side::Old;
        assert_eq!(c.location(), "a.rs:40 (removed)");
    }

    #[test]
    fn ids_do_not_retarget_after_deletion() {
        let mut s = CommentStore::new();
        let first = s.add(comment("a.rs", 1, 1, "first"));
        let second = s.add(comment("b.rs", 2, 2, "second"));
        assert!(second.ordinal() > first.ordinal());
        s.take(first);
        assert!(s.get(first).is_none());
        assert_eq!(s.get(second).unwrap().text, "second");
    }
}

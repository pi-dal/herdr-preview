//! The world snapshot: the derived state one refresh produces, built from Git review data or a
//! Files-only directory tree.
//!
//! `build` reads nothing from `App`, so the same call runs synchronously (startup, scope
//! switches, first visits) and behind the worker (polls, `r`, return visits)
//! (specs/tui.md Refresh). Reconciling a snapshot into place state stays
//! in `App::reconcile_world`, the one home for the Continuity rules (specs/overview.md).

use std::collections::{BTreeSet, HashMap, HashSet};
use std::ffi::OsStr;
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{
    Arc,
    mpsc::{Receiver, Sender},
};

use anyhow::Result;
use rustix::fs::{CWD, Dir, FileType, Mode, OFlags, fstat, openat, statat};

use crate::app::{RepositoryMode, Tab};
use crate::file_list::{Annotation, Entry};
use crate::git;
use crate::herdr::AgentSample;
use crate::model::{ChangedFile, Scope};
use crate::turn::{TurnTracker, WorktreeState};

/// Everything the build reads. A landed snapshot reconciles only while the view still
/// matches the input that produced it (specs/tui.md).
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct WorldInput {
    pub repo: PathBuf,
    pub repository_mode: RepositoryMode,
    pub tab: Tab,
    pub scope: Scope,
    /// The `--base` flag, resolved fresh per build. The pick is read from its ref at build
    /// time, so it is derived output, never input identity — a pick made in another pane
    /// must land here as newer content, not be discarded as a mismatch
    /// (specs/review-model.md).
    pub base: Option<String>,
    /// Bumped by this pane's own pick, so a build that read the previous pick fails the
    /// landing's input-equality gate instead of reverting the picked base. Another pane's
    /// pick leaves it alone — see `base` above.
    pub base_epoch: u64,
    /// The `last-turn` baseline tree the changed set diffs against; `None` before a turn.
    pub turn_baseline: Option<String>,
    /// Expanded ignored directories whose children the `All files` tree loads.
    pub toggled_dirs: HashSet<String>,
    /// Directories a Files-only worker job lists. These are slash-relative to the retained
    /// Files-only root capability; the root is the empty path. Git review always leaves this
    /// empty.
    pub raw_dirs: BTreeSet<String>,
    /// The retained descriptor for the selected Files-only root. It is intentionally separate
    /// from `repo`: display paths remain strings, but no Files-only filesystem operation uses
    /// that pathname as authority.
    pub files_root: Option<FilesRoot>,
    /// Invalidates a Files-only completion when authored expansion changes without changing
    /// the currently capped request batch.
    pub raw_tree_epoch: u64,
}

/// The derived state one refresh produces: the scope changeset, the navigator entries, and
/// the `branch` scope's resolved base. The base rides the snapshot so the header name and
/// the changeset it heads land whole, from one build (specs/tui.md).
#[derive(Debug)]
pub struct WorldSnapshot {
    pub changed: HashMap<String, Annotation>,
    pub entries: Vec<Entry>,
    pub branch_base: git::BaseStatus,
    /// One-level Files-only outcomes. `None` is Git review, where `entries` remains the
    /// complete existing tree contract.
    pub raw_listings: Option<Vec<DirectoryListing>>,
}

/// One direct-directory listing from a Files-only worker job. A failed read is per directory,
/// so a transient unreadable child never replaces another cached subtree with an empty tree.
#[derive(Debug)]
pub struct DirectoryListing {
    pub path: String,
    pub entries: Result<Vec<Entry>, String>,
}

/// Build the snapshot for `input`. The changeset is computed regardless of tab so the
/// header count and comment staleness stay correct while `All files` lists the whole
/// worktree. In `last-turn` with no baseline yet, the changeset is empty until a turn
/// start is observed (specs/review-model.md).
pub fn build(input: &WorldInput) -> Result<WorldSnapshot> {
    if input.repository_mode == RepositoryMode::FilesOnly {
        // Files-only has no synthetic changeset. It is a raw directory browser rooted exactly
        // at `repo`, so refresh performs filesystem enumeration only.
        let raw_listings = input
            .raw_dirs
            .iter()
            .map(|path| DirectoryListing {
                path: path.clone(),
                entries: input
                    .files_root
                    .as_ref()
                    .ok_or_else(|| "selected root is unavailable".to_string())
                    .and_then(|root| list_raw_dir(root, path)),
            })
            .collect();
        return Ok(WorldSnapshot {
            changed: HashMap::new(),
            entries: Vec::new(),
            branch_base: git::BaseStatus::default(),
            raw_listings: Some(raw_listings),
        });
    }
    // A Git worktree can disappear while a pane remains alive. Keep its stale frame rather
    // than turn that failure into a process error. Files-only never reaches this probe.
    if !git::is_repo(&input.repo) {
        return Ok(WorldSnapshot {
            changed: HashMap::new(),
            entries: Vec::new(),
            branch_base: git::BaseStatus::default(),
            raw_listings: None,
        });
    }
    let (branch_base, changed) = build_changed(input)?;
    let changed_map = annotate(&changed);
    let entries = match input.tab {
        // The whole worktree (ignored included), with expanded ignored dirs loaded lazily.
        Tab::AllFiles => all_files_entries(input, &changed_map)?,
        // `Changes` (the `PR` tab never builds a snapshot).
        _ => changed.iter().map(Entry::from_changed).collect(),
    };
    Ok(WorldSnapshot { changed: changed_map, entries, branch_base, raw_listings: None })
}

/// The active scope's changed files and, on the `branch` scope, the base they diff against —
/// the piece a scope switch rebuilds before its frame, so the header count and list never
/// wear another scope's label (specs/tui.md).
pub fn build_changed(input: &WorldInput) -> Result<(git::BaseStatus, Vec<ChangedFile>)> {
    let none = git::BaseStatus::default;
    if input.repository_mode == RepositoryMode::FilesOnly || !git::is_repo(&input.repo) {
        return Ok((none(), Vec::new()));
    }
    match input.scope {
        Scope::LastTurn => match input.turn_baseline.as_deref() {
            Some(t) => Ok((none(), git::changed_against_tree(&input.repo, t)?)),
            None => Ok((none(), Vec::new())),
        },
        Scope::Uncommitted => Ok((none(), git::changed_files(&input.repo, input.scope, None)?)),
        Scope::Branch => {
            // A resolve failure fails the build whole, so the landing keeps the stale
            // frame and reports — degrading to an empty snapshot would blank a populated
            // view over a transient error (specs/overview.md Continuity). A chain where
            // nothing resolves is not a failure: it returns the legible no-base state.
            let resolution = git::resolve_base(&input.repo, input.base.as_deref())
                .map_err(|e| anyhow::anyhow!("{}", e.0))?;
            let base_oid = resolution.status.winner.as_ref().map(|w| w.oid.clone());
            let changed = git::changed_files(&input.repo, input.scope, base_oid.as_deref())?;
            Ok((resolution.status, changed))
        }
    }
}

/// The changed-files map every consumer keys by path — one construction site, shared by
/// the worker build and the scope switch's synchronous rebuild.
pub fn annotate(changed: &[ChangedFile]) -> HashMap<String, Annotation> {
    changed.iter().map(|f| (f.path.clone(), Annotation::from(f))).collect()
}

/// The persisted turn baseline for `repo`, if any — the one seeding rule, shared by the
/// worker's tracker and the app's first-frame mirror (specs/herdr-host.md).
pub fn seed_baseline(repo: &std::path::Path) -> Option<String> {
    git::read_baseline_ref(repo, &git::worktree_key(repo))
}

/// Identity of a retained Files-only capability root or directory. The device/inode pair is
/// only compared between descriptors; it is never reconstructed into a pathname.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct DirectoryIdentity {
    device: u64,
    inode: u64,
}

/// The selected Files-only root as an open directory descriptor. This is the only filesystem
/// authority for Files-only: display paths are parsed into checked components and opened below
/// this descriptor with `openat(..., O_NOFOLLOW)`, never joined onto the launch pathname.
#[derive(Clone, Debug)]
pub struct FilesRoot {
    directory: Arc<File>,
    identity: DirectoryIdentity,
}

impl PartialEq for FilesRoot {
    fn eq(&self, other: &Self) -> bool {
        self.identity == other.identity
    }
}

impl Eq for FilesRoot {}

impl FilesRoot {
    /// Open the selected launch root once, refusing a symlink or non-directory before it can
    /// become Files-only authority. Subsequent operations retain this descriptor even if its
    /// launch pathname is renamed or replaced.
    pub(crate) fn open(path: &Path) -> Result<Self, String> {
        let fd = openat(
            CWD,
            path,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(|_| "selected root is unavailable".to_string())?;
        let directory = Arc::new(File::from(fd));
        let identity = directory_identity(&directory)?;
        Ok(Self { directory, identity })
    }

    fn open_directory(&self, relative: &str) -> Result<File, String> {
        let components = checked_components(relative)?;
        let mut directory =
            self.directory.try_clone().map_err(|_| "directory is unavailable".to_string())?;
        for name in components {
            let fd = openat(
                &directory,
                name,
                OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::empty(),
            )
            .map_err(|_| "directory is unavailable".to_string())?;
            directory = File::from(fd);
            directory_identity(&directory)?;
        }
        Ok(directory)
    }

    /// Verify that the requested target still names the descriptor about to be enumerated. The
    /// re-resolution is descriptor-relative and no-follow; if a test or hostile writer replaced
    /// it before enumeration, the operation fails rather than presenting either replacement.
    fn verify_current_directory(&self, relative: &str, directory: &File) -> Result<(), String> {
        let current = self.open_directory(relative)?;
        if directory_identity(&current)? == directory_identity(directory)? {
            Ok(())
        } else {
            Err("directory changed before enumeration".to_string())
        }
    }

    /// Open a listed file at click time through its retained root. Every parent is reopened
    /// relative to a verified descriptor, and the final open is no-follow before metadata or
    /// bytes are read, so a post-listing symlink replacement cannot escape the root.
    pub(crate) fn read_file(&self, relative: &str, max_bytes: usize) -> Result<RawFile, String> {
        // The descriptor is opened no-follow before bytes are read. Metadata is only a capacity
        // hint: a concurrent writer may grow the file after stat, so `take(cap + 1)` enforces the
        // budget from the already-authorized FD and proves an oversized result without a second
        // pathname lookup.
        let mut file = self.open_regular_file(relative)?;
        let hint = file
            .metadata()
            .map_err(|_| "file is unavailable".to_string())?
            .len()
            .min(max_bytes as u64) as usize;
        let mut bytes = Vec::with_capacity(hint.saturating_add(1));
        file.by_ref()
            .take(max_bytes.saturating_add(1) as u64)
            .read_to_end(&mut bytes)
            .map_err(|_| "file is unavailable".to_string())?;
        if bytes.len() > max_bytes {
            return Ok(RawFile::TooLarge);
        }
        Ok(RawFile::Content(bytes))
    }

    /// Resolve and open a regular file below the retained root without ever reconstructing an
    /// authority pathname. This is shared by the search-open preflight and File view read.
    fn open_regular_file(&self, relative: &str) -> Result<File, String> {
        let components = checked_components(relative)?;
        let (name, parents) =
            components.split_last().ok_or_else(|| "invalid file path".to_string())?;
        let mut directory =
            self.directory.try_clone().map_err(|_| "directory is unavailable".to_string())?;
        for parent in parents {
            let fd = openat(
                &directory,
                *parent,
                OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::empty(),
            )
            .map_err(|_| "file is unavailable".to_string())?;
            directory = File::from(fd);
            directory_identity(&directory)?;
        }
        let fd = openat(
            &directory,
            *name,
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(|_| "file is unavailable".to_string())?;
        let file = File::from(fd);
        let metadata = file.metadata().map_err(|_| "file is unavailable".to_string())?;
        if !metadata.file_type().is_file() {
            return Err("requested path is not a regular file".to_string());
        }
        Ok(file)
    }
}

/// A safely opened Files-only file, either read from the descriptor or declined before a
/// potentially expensive read.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum RawFile {
    Content(Vec<u8>),
    TooLarge,
}

fn directory_identity(directory: &File) -> Result<DirectoryIdentity, String> {
    let stat = fstat(directory).map_err(|_| "directory is unavailable".to_string())?;
    if !FileType::from_raw_mode(stat.st_mode).is_dir() {
        return Err("requested path is not a directory".to_string());
    }
    Ok(DirectoryIdentity { device: stat.st_dev as u64, inode: stat.st_ino as u64 })
}

/// Parse a display path without granting it pathname authority. The components are later passed
/// one at a time to `openat` below the retained root descriptor.
fn checked_components(relative: &str) -> Result<Vec<&OsStr>, String> {
    let path = Path::new(relative);
    if path.is_absolute() {
        return Err("invalid path".to_string());
    }
    path.components()
        .map(|component| match component {
            std::path::Component::Normal(name) if name != ".git" => Ok(name),
            std::path::Component::Normal(_) => Err("path includes .git".to_string()),
            std::path::Component::CurDir
            | std::path::Component::ParentDir
            | std::path::Component::RootDir
            | std::path::Component::Prefix(_) => Err("invalid path".to_string()),
        })
        .collect()
}

/// List exactly one Files-only directory through its retained descriptor. `relative` is parsed
/// into checked components; no parent, absolute, `.git`, or symlink component reaches the
/// directory stream. The caller controls recursion by issuing another worker request only after
/// the reviewer expands a returned real directory.
pub(crate) fn list_raw_dir(root: &FilesRoot, relative: &str) -> Result<Vec<Entry>, String> {
    list_raw_dir_after_open(root, relative, || {})
}

fn list_raw_dir_after_open(
    root: &FilesRoot,
    relative: &str,
    after_open: impl FnOnce(),
) -> Result<Vec<Entry>, String> {
    let directory = root.open_directory(relative)?;
    after_open();
    root.verify_current_directory(relative, &directory)?;
    let mut stream =
        Dir::read_from(&directory).map_err(|_| "directory is unavailable".to_string())?;
    let mut out = Vec::new();
    while let Some(entry) = stream.read() {
        let Ok(entry) = entry else { continue };
        let name = entry.file_name();
        if name.to_bytes() == b"." || name.to_bytes() == b".." || name.to_bytes() == b".git" {
            continue;
        }
        let Some(name) = std::str::from_utf8(name.to_bytes()).ok() else { continue };
        // Inspect the child relative to the verified directory without following it. A later
        // replacement is harmless: traversal and content reads each open it no-follow again.
        let Ok(stat) = statat(&directory, OsStr::new(name), rustix::fs::AtFlags::SYMLINK_NOFOLLOW)
        else {
            continue;
        };
        let kind = FileType::from_raw_mode(stat.st_mode);
        if kind.is_symlink() {
            continue;
        }
        let path =
            if relative.is_empty() { name.to_string() } else { format!("{relative}/{name}") };
        if kind.is_dir() {
            out.push(Entry {
                path,
                previous_path: None,
                annotation: None,
                ignored: false,
                is_dir: true,
                explicit_dir: true,
            });
        } else if kind.is_file() {
            out.push(Entry {
                path,
                previous_path: None,
                annotation: None,
                ignored: false,
                is_dir: false,
                explicit_dir: false,
            });
        }
    }
    out.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(out)
}

/// The `All files` entries: every worktree path (ignored dimmed), with the children of
/// expanded ignored directories loaded lazily (`specs/file-list.md`). Only directories the
/// user has expanded are walked, so the cost tracks what is on screen, not the whole tree.
pub(crate) fn all_files_entries(
    input: &WorldInput,
    changed: &HashMap<String, Annotation>,
) -> Result<Vec<Entry>> {
    if input.repository_mode == RepositoryMode::FilesOnly {
        return Ok(Vec::new());
    }
    let to_entry = |w: git::WorktreeEntry| Entry {
        annotation: changed.get(&w.path).cloned(),
        path: w.path,
        previous_path: None,
        ignored: w.ignored,
        is_dir: w.is_dir,
        explicit_dir: false,
    };
    let mut entries: Vec<Entry> = git::all_files(&input.repo)?.into_iter().map(&to_entry).collect();
    let mut i = 0;
    while i < entries.len() {
        if entries[i].is_dir && input.toggled_dirs.contains(&entries[i].path) {
            let path = entries[i].path.clone();
            let children = git::list_ignored_dir(&input.repo, &path).into_iter().map(&to_entry);
            entries.extend(children);
        }
        i += 1;
    }
    Ok(entries)
}

/// Turn tracking, owned by the worker: the sample, the snapshot capture, and the baseline
/// promotion happen on one thread, so the snapshot always rides the sample that observed the
/// edge (specs/herdr-host.md). The baseline ref stays reviewr's only git write.
#[derive(Debug)]
pub struct TurnHost {
    tracker: TurnTracker,
    repo: PathBuf,
    turn_key: String,
    /// Each agent `cwd` resolved to whether it is a member of the reviewed worktree. Only a
    /// resolved git top level is recorded, since a worktree root does not move, so a member is
    /// placed once and never re-queried. A cwd git reports outside every worktree is not cached:
    /// re-checking it is cheap, and a directory can become a worktree later. A cwd git could not
    /// run for is not cached either, and holds the poll rather than counting the agent out, so a
    /// transient failure never poisons a member for the session (specs/herdr-host.md).
    resolved: HashMap<String, bool>,
    files_only: bool,
}

/// One sample's outcome, sent back with the completion: whether it ended a turn (the `PR`
/// tab's refetch signal), and what this sample saw of the worktree's membership (the
/// `last-turn` empty state). The baseline itself rides the completion's input.
#[derive(Clone, Debug)]
pub struct TurnReport {
    pub ended: bool,
    /// `None` when the sample could not observe the whole worktree, so the reader keeps whatever
    /// it already knew: either the enumeration failed, or a member's directory would not resolve
    /// this poll. Membership is held on the one consumer that paints it, never mirrored here
    /// (specs/herdr-host.md).
    pub agents_present: Option<bool>,
}

/// An agent's relationship to the reviewed worktree, as [`TurnHost::membership`] resolves it.
/// `Unknown` is not `NotMember`: it means git could not resolve the cwd this poll, so the fold
/// holds on it rather than counting the agent out (specs/herdr-host.md).
enum Membership {
    Member,
    NotMember,
    Unknown,
}

/// The absolute cwd an agent names, or `None` for a blank or relative one. `git -C` resolves a
/// relative directory against reviewr's own cwd, which is normally the reviewed worktree, so a
/// relative cwd would be wrongly admitted as a member (specs/herdr-host.md).
fn worktree_cwd(cwd: Option<&str>) -> Option<&str> {
    cwd.filter(|c| Path::new(c).is_absolute())
}

/// Fold the members' statuses into the worktree's work state and whether any member is present,
/// or `None` if a member's membership was undetermined — the caller then holds the sample.
/// Pure over the `member` resolver so the fold-and-hold rule is unit-testable without git.
fn classify(
    samples: &[AgentSample],
    mut member: impl FnMut(&AgentSample) -> Membership,
) -> Option<(bool, WorktreeState)> {
    let mut members = Vec::new();
    for sample in samples {
        match member(sample) {
            Membership::Member => members.push(sample.status),
            Membership::NotMember => {}
            Membership::Unknown => return None,
        }
    }
    Some((!members.is_empty(), WorktreeState::fold(members)))
}

impl TurnHost {
    /// Resume any persisted turn baseline for this worktree, so `last-turn` keeps its
    /// anchor across a reviewr pane restart (specs/herdr-host.md).
    /// `repo` must already be the git top level, as [`crate::world::seed_baseline`] and
    /// membership both compare against it and `App` derives the same baseline-ref key from
    /// its own copy — normalizing here instead would key the two apart. `run` resolves it
    /// once for both (`src/lib.rs`).
    pub fn open(repo: PathBuf) -> Self {
        let tracker = TurnTracker::with_baseline(seed_baseline(&repo));
        let turn_key = git::worktree_key(&repo);
        Self { tracker, repo, turn_key, resolved: HashMap::new(), files_only: false }
    }

    /// A Files-only worker keeps the same request/completion topology without sampling Herdr or
    /// invoking Git. Its raw-directory snapshots still land latest-wins.
    pub fn open_files_only(repo: PathBuf) -> Self {
        Self {
            tracker: TurnTracker::default(),
            turn_key: String::new(),
            repo,
            resolved: HashMap::new(),
            files_only: true,
        }
    }

    pub fn baseline(&self) -> Option<&str> {
        self.tracker.baseline()
    }

    /// Sample the agents over the herdr CLI and advance the baseline. A missing herdr is
    /// normal, so a failed enumeration only logs and changes nothing.
    pub fn sample(&mut self) -> TurnReport {
        if self.files_only {
            return TurnReport { ended: false, agents_present: None };
        }
        self.observe_agents(crate::herdr::agent_samples().ok().as_deref())
    }

    /// Advance the baseline from one enumeration — the core [`Self::sample`] wraps, and the
    /// seam tests drive without herdr. `None` is a failed enumeration, which holds the
    /// previous membership rather than reporting an empty worktree (specs/herdr-host.md).
    pub fn observe_agents(&mut self, samples: Option<&[AgentSample]>) -> TurnReport {
        let Some(samples) = samples else {
            return TurnReport { ended: false, agents_present: None };
        };
        // A member whose membership git could not determine leaves the sample incomplete, so
        // hold it exactly as a failed enumeration rather than reading an unresolved member as
        // an empty worktree (specs/herdr-host.md).
        let Some((present, state)) = classify(samples, |s| self.membership(s.cwd.as_deref()))
        else {
            return TurnReport { ended: false, agents_present: None };
        };
        let ended = self.observe(state);
        TurnReport { ended, agents_present: Some(present) }
    }

    /// An agent's relationship to the reviewed worktree. The git top level is authoritative, so
    /// a subdirectory is a member and a second worktree of the same repository is not
    /// (specs/herdr-host.md).
    fn membership(&mut self, cwd: Option<&str>) -> Membership {
        let Some(cwd) = worktree_cwd(cwd) else {
            return Membership::NotMember;
        };
        if let Some(&member) = self.resolved.get(cwd) {
            return if member { Membership::Member } else { Membership::NotMember };
        }
        match git::worktree_of(Path::new(cwd)) {
            // A resolved root is stable, so record whether it is a member and never shell out
            // for this cwd again. git canonicalizes it, so the worktree root itself matches too.
            git::Worktree::Root(top) => {
                let member = top == self.repo;
                self.resolved.insert(cwd.to_string(), member);
                if member { Membership::Member } else { Membership::NotMember }
            }
            // git ran and found no worktree. A determination, but not a stable one, so it is
            // re-checked next poll rather than cached.
            git::Worktree::Outside => Membership::NotMember,
            // git could not run, so nothing is known this poll. Hold rather than count the agent
            // out, exactly as a failed enumeration does (specs/herdr-host.md).
            git::Worktree::Unknown => Membership::Unknown,
        }
    }

    /// Advance the baseline from one folded worktree state, returning whether a turn ended.
    /// On a turn start it snapshots the worktree as the candidate; while a candidate is
    /// pending it promotes once the worktree diverges from it, persisting the new baseline.
    /// Git errors only log, so a transient git failure never crashes the poll.
    fn observe(&mut self, state: WorktreeState) -> bool {
        let transition = self.tracker.observe(state);
        if transition.started {
            match git::snapshot_worktree(&self.repo) {
                // The candidate is this worktree as of a moment ago, so it cannot have
                // diverged from it yet. The next poll runs the check, which is what makes
                // this an early return rather than a second snapshot of the same tree.
                Ok(sha) => {
                    self.tracker.set_candidate(sha);
                    return transition.ended;
                }
                Err(e) => logln!("turn snapshot failed: {e}"),
            }
        }
        // Promote the pending candidate once the turn has changed a file. Compare full
        // snapshots so a new untracked file counts as a change (specs/herdr-host.md).
        let Some(candidate) = self.tracker.candidate().map(str::to_string) else {
            return transition.ended;
        };
        match git::snapshot_worktree(&self.repo) {
            Ok(now) if now != candidate => {
                self.tracker.promote();
                if let Err(e) = git::write_baseline_ref(&self.repo, &self.turn_key, &candidate) {
                    logln!("turn baseline ref write failed: {e}");
                }
            }
            Ok(_) => {}
            Err(e) => logln!("turn divergence check failed: {e}"),
        }
        transition.ended
    }
}

/// One queued refresh's attributes, accumulated on `App` until the loop dispatches it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct WorldRequest {
    /// Sample the agents in the worktree — set by the poll alone (specs/tui.md).
    pub sample_turn: bool,
    /// Re-reveal the cursor when the result lands — user-initiated switches only.
    pub reveal: bool,
}

/// One refresh request. The worker builds against `input`, refreshing its `turn_baseline`
/// from the sample first, and echoes the tag back with the completion.
#[derive(Debug)]
pub struct WorldJob {
    pub generation: u64,
    pub input: WorldInput,
    /// Poll-driven requests sample the agents in the worktree; tab entry and `r` do not,
    /// so the herdr CLI call count tracks the poll alone (specs/tui.md).
    pub sample_turn: bool,
    /// A user-initiated switch re-reveals the cursor when its result lands; a poll never
    /// does (specs/tui.md).
    pub reveal: bool,
}

/// A finished job: the tag it was built for, the sample's outcome (`None` when the job
/// didn't sample — a tab entry or `r`, not a poll), and the snapshot — `None` when the
/// input's tab builds no file tree (the `PR` tab).
#[derive(Debug)]
pub struct WorldCompletion {
    pub generation: u64,
    pub input: WorldInput,
    pub reveal: bool,
    pub turn: Option<TurnReport>,
    pub snapshot: Option<Result<WorldSnapshot>>,
}

/// Run the world worker until the request channel closes. The latest request wins: queued
/// requests coalesce into the newest, keeping any superseded job's sample and reveal flags
/// so a poll's status sample is never skipped.
pub fn spawn(
    mut host: TurnHost,
    rx: Receiver<WorldJob>,
    tx: Sender<WorldCompletion>,
) -> std::thread::JoinHandle<()> {
    std::thread::Builder::new()
        .name("world".into())
        .spawn(move || {
            while let Ok(mut job) = rx.recv() {
                while let Ok(next) = rx.try_recv() {
                    job = WorldJob {
                        sample_turn: job.sample_turn || next.sample_turn,
                        reveal: job.reveal || next.reveal,
                        ..next
                    };
                }
                let turn = job.sample_turn.then(|| host.sample());
                job.input.turn_baseline = host.baseline().map(str::to_string);
                let snapshot = job.input.tab.is_file_tab().then(|| build(&job.input));
                let completion = WorldCompletion {
                    generation: job.generation,
                    input: job.input,
                    reveal: job.reveal,
                    turn,
                    snapshot,
                };
                if tx.send(completion).is_err() {
                    break;
                }
            }
        })
        .expect("spawn world worker")
}

#[cfg(test)]
mod tests {
    use super::{
        FilesRoot, Membership, WorldInput, build, classify, list_raw_dir as list_raw_dir_from_cap,
        list_raw_dir_after_open, worktree_cwd,
    };
    use crate::app::{RepositoryMode, Tab};
    use crate::herdr::AgentSample;
    use crate::model::Scope;
    use crate::turn::{Status, WorktreeState};
    use std::collections::HashSet;

    fn files_only_input(root: &std::path::Path) -> WorldInput {
        WorldInput {
            repo: root.to_path_buf(),
            repository_mode: RepositoryMode::FilesOnly,
            tab: Tab::AllFiles,
            scope: Scope::Uncommitted,
            base: None,
            base_epoch: 0,
            turn_baseline: None,
            toggled_dirs: HashSet::new(),
            raw_dirs: [String::new()].into_iter().collect(),
            files_root: FilesRoot::open(root).ok(),
            raw_tree_epoch: 0,
        }
    }

    fn list_raw_dir(
        root: &std::path::Path,
        relative: &str,
    ) -> Result<Vec<crate::file_list::Entry>, String> {
        FilesRoot::open(root).and_then(|root| list_raw_dir_from_cap(&root, relative))
    }

    #[test]
    fn files_only_lists_only_the_requested_direct_directory() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("docs/guides")).unwrap();
        std::fs::create_dir(root.path().join("empty")).unwrap();
        std::fs::write(root.path().join("docs/guides/start.md"), "start\n").unwrap();

        let root_entries = list_raw_dir(root.path(), "").unwrap();
        let root_paths: Vec<(&str, bool)> =
            root_entries.iter().map(|entry| (entry.path.as_str(), entry.is_dir)).collect();
        assert_eq!(root_paths, [("docs", true), ("empty", true)]);
        let nested = list_raw_dir(root.path(), "docs/guides").unwrap();
        assert_eq!(nested[0].path, "docs/guides/start.md");
    }

    #[test]
    fn files_only_resolves_a_nested_real_directory() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("docs/guides")).unwrap();
        std::fs::write(root.path().join("docs/guides/start.md"), "start\n").unwrap();

        let entries = list_raw_dir(root.path(), "docs/guides").unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].path, "docs/guides/start.md");
        assert!(!entries[0].is_dir);
    }

    #[test]
    fn files_only_root_listing_does_not_materialize_a_deep_fixture() {
        let root = tempfile::tempdir().unwrap();
        let mut deep = root.path().join("project");
        for segment in 0..64 {
            deep.push(format!("l{segment}"));
        }
        std::fs::create_dir_all(&deep).unwrap();
        std::fs::write(deep.join("end.txt"), "end\n").unwrap();

        let entries = list_raw_dir(root.path(), "").unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].path, "project");
        assert!(entries[0].is_dir);
    }

    #[test]
    fn files_only_excludes_nested_git_directories_and_descendants() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("project/.git/objects")).unwrap();
        std::fs::write(root.path().join("project/.git/config"), "private\n").unwrap();
        std::fs::write(root.path().join("project/readme.md"), "visible\n").unwrap();

        let root_paths: Vec<String> =
            list_raw_dir(root.path(), "").unwrap().into_iter().map(|entry| entry.path).collect();
        let paths: Vec<String> = list_raw_dir(root.path(), "project")
            .unwrap()
            .into_iter()
            .map(|entry| entry.path)
            .collect();
        assert_eq!(root_paths, ["project"]);
        assert_eq!(paths, ["project/readme.md"]);
    }

    #[test]
    fn files_only_rejects_git_in_every_requested_path_component() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("project/.git/objects")).unwrap();

        assert!(list_raw_dir(root.path(), ".git").is_err(), "reject a final .git component");
        assert!(
            list_raw_dir(root.path(), ".git/objects").is_err(),
            "reject .git before a descendant"
        );
        assert!(
            list_raw_dir(root.path(), "project/.git/objects").is_err(),
            "reject an intermediate nested .git component"
        );
    }

    #[cfg(unix)]
    #[test]
    fn files_only_never_follows_file_or_directory_symlinks() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("local.txt"), "local\n").unwrap();
        std::fs::write(outside.path().join("secret.txt"), "secret\n").unwrap();
        symlink(outside.path().join("secret.txt"), root.path().join("linked-file")).unwrap();
        symlink(outside.path(), root.path().join("linked-dir")).unwrap();

        let paths: Vec<String> =
            list_raw_dir(root.path(), "").unwrap().into_iter().map(|entry| entry.path).collect();
        assert_eq!(paths, ["local.txt"]);
        assert!(
            list_raw_dir(root.path(), "linked-dir").is_err(),
            "a requested directory symlink must not be followed"
        );
    }

    #[cfg(unix)]
    #[test]
    fn files_only_rejects_a_root_or_listed_directory_replaced_by_a_symlink() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let listed = root.path().join("listed");
        std::fs::create_dir(&listed).unwrap();
        std::fs::write(outside.path().join("secret.txt"), "secret\n").unwrap();

        std::fs::remove_dir(&listed).unwrap();
        symlink(outside.path(), &listed).unwrap();
        assert!(
            list_raw_dir(root.path(), "listed").is_err(),
            "a stale worker request cannot follow a replacement symlink"
        );

        let linked_root = root.path().join("linked-root");
        symlink(root.path(), &linked_root).unwrap();
        assert!(
            list_raw_dir(&linked_root, "").is_err(),
            "the launch root itself must be a real directory"
        );
    }

    #[cfg(unix)]
    #[test]
    fn files_only_rejects_a_directory_replaced_after_descriptor_resolution() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("a/b")).unwrap();
        std::fs::write(root.path().join("a/b/local.txt"), "local\n").unwrap();
        std::fs::create_dir(outside.path().join("b")).unwrap();
        std::fs::write(outside.path().join("b/secret.txt"), "secret\n").unwrap();
        let capability = FilesRoot::open(root.path()).unwrap();

        let result = list_raw_dir_after_open(&capability, "a/b", || {
            std::fs::remove_dir_all(root.path().join("a")).unwrap();
            symlink(outside.path(), root.path().join("a")).unwrap();
        });
        assert!(result.is_err(), "an intermediate replacement before enumeration is rejected");
    }

    #[cfg(unix)]
    #[test]
    fn files_only_rejects_a_target_directory_replaced_before_enumeration() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("docs")).unwrap();
        std::fs::write(root.path().join("docs/local.txt"), "local\n").unwrap();
        std::fs::write(outside.path().join("secret.txt"), "secret\n").unwrap();
        let capability = FilesRoot::open(root.path()).unwrap();

        let result = list_raw_dir_after_open(&capability, "docs", || {
            std::fs::remove_dir_all(root.path().join("docs")).unwrap();
            symlink(outside.path(), root.path().join("docs")).unwrap();
        });
        assert!(result.is_err(), "a target replacement before enumeration is rejected");
    }

    #[cfg(unix)]
    #[test]
    fn files_only_rejects_a_listed_file_replaced_by_a_symlink_before_reading() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("report.txt"), "local\n").unwrap();
        std::fs::write(outside.path().join("secret.txt"), "secret\n").unwrap();
        let capability = FilesRoot::open(root.path()).unwrap();
        assert!(
            list_raw_dir_from_cap(&capability, "")
                .unwrap()
                .iter()
                .any(|entry| entry.path == "report.txt"),
            "the regular file is listed before its replacement"
        );

        std::fs::remove_file(root.path().join("report.txt")).unwrap();
        symlink(outside.path().join("secret.txt"), root.path().join("report.txt")).unwrap();
        assert!(
            capability.read_file("report.txt", 1024).is_err(),
            "the later read uses O_NOFOLLOW rather than the listed pathname"
        );
    }

    #[test]
    fn files_only_build_has_no_git_side_effects() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("folder")).unwrap();
        std::fs::write(root.path().join("folder/note.txt"), "note\n").unwrap();

        let snapshot = build(&files_only_input(root.path())).unwrap();
        assert!(snapshot.changed.is_empty());
        let listings = snapshot.raw_listings.expect("Files-only returns raw listings");
        assert_eq!(listings.len(), 1);
        assert!(listings[0].entries.as_ref().unwrap().iter().any(|entry| entry.path == "folder"));
        assert!(
            !root.path().join(".git").exists(),
            "a Files-only build must not create Git metadata or private refs"
        );
    }

    fn working_at(cwd: &str) -> AgentSample {
        AgentSample { cwd: Some(cwd.into()), status: Status::Working }
    }

    #[test]
    fn only_an_absolute_cwd_can_name_a_worktree() {
        // A blank or relative cwd would resolve against reviewr's own cwd (the reviewed
        // worktree), so membership must reject it before any git call (specs/herdr-host.md).
        assert_eq!(worktree_cwd(Some("/abs/path")), Some("/abs/path"));
        assert_eq!(worktree_cwd(Some("relative/path")), None);
        assert_eq!(worktree_cwd(Some("")), None);
        assert_eq!(worktree_cwd(None), None);
    }

    #[test]
    fn membership_decides_the_fold_and_undetermined_holds() {
        // One working agent, resolved three ways. `Unknown` holds the sample (the caller reads
        // this `None` exactly as a failed enumeration, never as an empty worktree); a determined
        // verdict folds normally (specs/herdr-host.md).
        let samples = [working_at("/w")];
        assert_eq!(classify(&samples, |_| Membership::Unknown), None);
        assert_eq!(
            classify(&samples, |_| Membership::Member),
            Some((true, WorktreeState::Working))
        );
        assert_eq!(
            classify(&samples, |_| Membership::NotMember),
            Some((false, WorktreeState::Resting))
        );
    }

    #[test]
    fn one_undetermined_member_holds_even_beside_a_resolved_one() {
        // A resolved working member does not rescue a sample that also holds an unknown one: an
        // incomplete view of the worktree is held whole, not folded from the part that resolved.
        let samples = [working_at("/a"), working_at("/b")];
        let held = classify(&samples, |s| match s.cwd.as_deref() {
            Some("/b") => Membership::Unknown,
            _ => Membership::Member,
        });
        assert_eq!(held, None);
    }

    #[test]
    fn a_non_members_status_never_reaches_the_fold() {
        // A member resting and a non-member (a sibling worktree) working. Only the member's
        // status folds, so the worktree reads Resting, never the sibling's Working.
        let samples = [
            AgentSample { cwd: Some("/mine".into()), status: Status::Idle },
            AgentSample { cwd: Some("/sibling".into()), status: Status::Working },
        ];
        let folded = classify(&samples, |s| match s.cwd.as_deref() {
            Some("/sibling") => Membership::NotMember,
            _ => Membership::Member,
        });
        assert_eq!(folded, Some((true, WorktreeState::Resting)));
    }
}

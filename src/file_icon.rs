//! Lexical navigator file-kind decorations.
//!
//! Resolution is deliberately pure: it only examines the basename of the canonical display
//! path. It does not read metadata, probe MIME types, invoke Git, or touch the filesystem.
//! The compact built-in association baseline is project-owned. `tools/import_file_icons.py`
//! documents the deliberately manual seam for a future pinned upstream association snapshot.

use std::collections::BTreeMap;

/// The configured representation for file-kind decoration in the navigator.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FileIconMode {
    /// One font-safe ASCII kind code.
    #[default]
    Plain,
    /// A best-effort Unicode emoji. Terminal font and advance-width behaviour is not detectable.
    Emoji,
    /// A compatibility best-effort Nerd Font glyph; never the implicit default.
    Nerd,
    /// No file-kind decoration.
    None,
}

impl FileIconMode {
    /// The normalized configuration spelling.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Plain => "plain",
            Self::Emoji => "emoji",
            Self::Nerd => "nerd",
            Self::None => "none",
        }
    }
}

/// Stable, safe identities usable by the bundled association baseline and user overrides.
/// Values name a semantic file kind, never a glyph, colour, filesystem object, or external theme.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum IconId {
    Rust,
    Javascript,
    Typescript,
    Node,
    Python,
    Go,
    Java,
    Kotlin,
    Swift,
    Shell,
    Html,
    Css,
    Vue,
    Json,
    Yaml,
    Toml,
    Xml,
    Config,
    Docker,
    Git,
    Markdown,
    Document,
    Image,
    Media,
    Package,
    Binary,
    Generic,
}

impl IconId {
    /// Every built-in identity accepted by configuration. Kept ordered for tests and docs.
    pub const ALL: [Self; 27] = [
        Self::Rust,
        Self::Javascript,
        Self::Typescript,
        Self::Node,
        Self::Python,
        Self::Go,
        Self::Java,
        Self::Kotlin,
        Self::Swift,
        Self::Shell,
        Self::Html,
        Self::Css,
        Self::Vue,
        Self::Json,
        Self::Yaml,
        Self::Toml,
        Self::Xml,
        Self::Config,
        Self::Docker,
        Self::Git,
        Self::Markdown,
        Self::Document,
        Self::Image,
        Self::Media,
        Self::Package,
        Self::Binary,
        Self::Generic,
    ];

    /// Parse the documented, closed configuration identifier.
    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "rust" => Self::Rust,
            "javascript" => Self::Javascript,
            "typescript" => Self::Typescript,
            "node" => Self::Node,
            "python" => Self::Python,
            "go" => Self::Go,
            "java" => Self::Java,
            "kotlin" => Self::Kotlin,
            "swift" => Self::Swift,
            "shell" => Self::Shell,
            "html" => Self::Html,
            "css" => Self::Css,
            "vue" => Self::Vue,
            "json" => Self::Json,
            "yaml" => Self::Yaml,
            "toml" => Self::Toml,
            "xml" => Self::Xml,
            "config" => Self::Config,
            "docker" => Self::Docker,
            "git" => Self::Git,
            "markdown" => Self::Markdown,
            "document" => Self::Document,
            "image" => Self::Image,
            "media" => Self::Media,
            "package" => Self::Package,
            "binary" => Self::Binary,
            "generic" => Self::Generic,
            _ => return None,
        })
    }

    /// The canonical configuration identifier.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Rust => "rust",
            Self::Javascript => "javascript",
            Self::Typescript => "typescript",
            Self::Node => "node",
            Self::Python => "python",
            Self::Go => "go",
            Self::Java => "java",
            Self::Kotlin => "kotlin",
            Self::Swift => "swift",
            Self::Shell => "shell",
            Self::Html => "html",
            Self::Css => "css",
            Self::Vue => "vue",
            Self::Json => "json",
            Self::Yaml => "yaml",
            Self::Toml => "toml",
            Self::Xml => "xml",
            Self::Config => "config",
            Self::Docker => "docker",
            Self::Git => "git",
            Self::Markdown => "markdown",
            Self::Document => "document",
            Self::Image => "image",
            Self::Media => "media",
            Self::Package => "package",
            Self::Binary => "binary",
            Self::Generic => "generic",
        }
    }

    const fn presentation(self) -> FileIcon {
        match self {
            Self::Rust => icon("s", "🦀", ""),
            Self::Javascript => icon("s", "📜", ""),
            Self::Typescript => icon("s", "📘", ""),
            Self::Node => icon("c", "🟢", ""),
            Self::Python => icon("s", "🐍", ""),
            Self::Go => icon("s", "🐹", ""),
            Self::Java => icon("s", "☕", ""),
            Self::Kotlin => icon("s", "🅺", ""),
            Self::Swift => icon("s", "🕊", ""),
            Self::Shell => icon("s", "🐚", ""),
            Self::Html => icon("t", "🌐", ""),
            Self::Css => icon("t", "🎨", ""),
            Self::Vue => icon("s", "🟩", ""),
            Self::Json => icon("j", "🧾", ""),
            Self::Yaml => icon("j", "⚙", ""),
            Self::Toml => icon("j", "🧾", ""),
            Self::Xml => icon("j", "📰", "󰗀"),
            Self::Config => icon("c", "⚙", ""),
            Self::Docker => icon("c", "🐳", ""),
            Self::Git => icon("c", "🌿", ""),
            Self::Markdown => icon("d", "📝", "󰍔"),
            Self::Document => icon("d", "📄", "󰈙"),
            Self::Image => icon("i", "🖼", "󰋩"),
            Self::Media => icon("m", "🎵", "󰎆"),
            Self::Package => icon("p", "📦", "󰏖"),
            Self::Binary => icon("b", "⚙", "󰆍"),
            Self::Generic => icon(".", "📄", "󰈔"),
        }
    }
}

/// A lexical file-kind presentation. The `emoji` field is a string because an emoji can contain
/// multiple Unicode scalar values. Plain output is the deterministic default; Nerd is retained
/// only for explicit backwards compatibility.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FileIcon {
    pub plain: &'static str,
    pub emoji: &'static str,
    pub nerd: &'static str,
}

const fn icon(plain: &'static str, emoji: &'static str, nerd: &'static str) -> FileIcon {
    FileIcon { plain, emoji, nerd }
}

/// User-owned association overlay. Keys are normalized ASCII-lowercase basenames or suffixes.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FileIconOverrides {
    pub names: BTreeMap<String, IconId>,
    pub extensions: BTreeMap<String, IconId>,
}

/// The stable fallback taxonomy. Exact names and suffixes resolve before these categories.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IconCategory {
    Source,
    ConfigBuild,
    Document,
    Data,
    StyleTemplate,
    Image,
    Media,
    ArchivePackage,
    ExecutableBinary,
    Generic,
}

impl IconCategory {
    const fn icon_id(self) -> IconId {
        match self {
            Self::Source => IconId::Shell,
            Self::ConfigBuild => IconId::Config,
            Self::Document => IconId::Document,
            Self::Data => IconId::Json,
            Self::StyleTemplate => IconId::Css,
            Self::Image => IconId::Image,
            Self::Media => IconId::Media,
            Self::ArchivePackage => IconId::Package,
            Self::ExecutableBinary => IconId::Binary,
            Self::Generic => IconId::Generic,
        }
    }
}

fn exact_icon(name: &str) -> Option<IconId> {
    Some(match name {
        "cargo.toml" | "cargo.lock" => IconId::Rust,
        "package.json"
        | "package-lock.json"
        | "npm-shrinkwrap.json"
        | "yarn.lock"
        | "pnpm-lock.yaml"
        | "pnpm-workspace.yaml"
        | "bun.lockb"
        | "deno.json"
        | "deno.jsonc" => IconId::Node,
        "dockerfile"
        | "compose.yaml"
        | "compose.yml"
        | "docker-compose.yaml"
        | "docker-compose.yml" => IconId::Docker,
        ".gitignore" | ".gitattributes" | ".gitmodules" | ".gitconfig" => IconId::Git,
        "makefile" | "gnumakefile" | "justfile" | ".editorconfig" | ".prettierrc" | ".eslintrc"
        | ".npmrc" | ".nvmrc" | ".tool-versions" | "mise.toml" => IconId::Config,
        "readme" | "readme.md" | "readme.markdown" | "changelog.md" | "license" | "license.md"
        | "copying" => IconId::Markdown,
        _ => return None,
    })
}

fn bundled_compound_icon(name: &str) -> Option<IconId> {
    if name.ends_with(".d.ts") || name.ends_with(".d.mts") || name.ends_with(".d.cts") {
        Some(IconId::Typescript)
    } else if [".tar.gz", ".tar.bz2", ".tar.xz", ".tar.zst"].iter().any(|s| name.ends_with(s)) {
        Some(IconId::Package)
    } else {
        None
    }
}

fn extension_icon(extension: &str) -> Option<IconId> {
    Some(match extension {
        "rs" => IconId::Rust,
        "js" | "mjs" | "cjs" => IconId::Javascript,
        "ts" | "mts" | "cts" | "tsx" => IconId::Typescript,
        "vue" => IconId::Vue,
        "py" | "pyi" => IconId::Python,
        "go" => IconId::Go,
        "java" => IconId::Java,
        "kt" | "kts" => IconId::Kotlin,
        "swift" => IconId::Swift,
        "sh" | "bash" | "zsh" | "fish" | "ps1" | "bat" | "cmd" => IconId::Shell,
        "html" | "htm" | "hbs" | "mustache" | "jinja" | "j2" => IconId::Html,
        "css" | "scss" | "sass" | "less" => IconId::Css,
        "json" | "jsonc" => IconId::Json,
        "yaml" | "yml" => IconId::Yaml,
        "toml" => IconId::Toml,
        "xml" | "svg" => IconId::Xml,
        "md" | "markdown" | "mdx" | "rst" | "adoc" => IconId::Markdown,
        "txt" | "pdf" | "doc" | "docx" => IconId::Document,
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "ico" | "bmp" | "tiff" | "avif" => IconId::Image,
        "mp3" | "wav" | "ogg" | "flac" | "aac" | "mp4" | "mov" | "mkv" | "webm" | "avi" => {
            IconId::Media
        }
        "zip" | "tar" | "gz" | "bz2" | "xz" | "zst" | "7z" | "rar" | "crate" | "deb" | "rpm"
        | "whl" => IconId::Package,
        "exe" | "dll" | "so" | "dylib" | "bin" | "wasm" | "class" | "o" | "a" => IconId::Binary,
        _ => return None,
    })
}

/// Classify a file from its canonical slash-separated entry path without filesystem I/O.
#[must_use]
pub fn classify_file(path: &str) -> IconCategory {
    let lower = basename(path).to_ascii_lowercase();
    if matches!(
        lower.as_str(),
        "cargo.toml"
            | "cargo.lock"
            | "package.json"
            | "package-lock.json"
            | "npm-shrinkwrap.json"
            | "yarn.lock"
            | "pnpm-lock.yaml"
            | "pnpm-workspace.yaml"
            | "bun.lockb"
            | "deno.json"
            | "deno.jsonc"
            | "dockerfile"
            | "compose.yaml"
            | "compose.yml"
            | "docker-compose.yaml"
            | "docker-compose.yml"
            | ".gitignore"
            | ".gitattributes"
            | ".gitmodules"
            | ".gitconfig"
            | "makefile"
            | "gnumakefile"
            | "justfile"
            | ".editorconfig"
            | ".prettierrc"
            | ".eslintrc"
            | ".npmrc"
            | ".nvmrc"
            | ".tool-versions"
            | "mise.toml"
    ) || lower.starts_with(".env")
        || lower.starts_with("dockerfile.")
    {
        return IconCategory::ConfigBuild;
    }
    if bundled_compound_icon(&lower).is_some() {
        return IconCategory::ArchivePackage;
    }
    match final_extension(&lower).unwrap_or("") {
        "rs" | "c" | "h" | "cpp" | "cxx" | "cc" | "hpp" | "py" | "pyi" | "go" | "java" | "kt"
        | "kts" | "swift" | "js" | "mjs" | "cjs" | "ts" | "mts" | "cts" | "tsx" | "jsx" | "vue"
        | "rb" | "php" | "sh" | "bash" | "zsh" | "fish" | "pl" | "lua" | "ps1" => {
            IconCategory::Source
        }
        "md" | "markdown" | "mdx" | "rst" | "txt" | "adoc" | "pdf" | "doc" | "docx" => {
            IconCategory::Document
        }
        "json" | "jsonc" | "toml" | "yaml" | "yml" | "xml" | "csv" | "tsv" | "sql" | "graphql"
        | "gql" => IconCategory::Data,
        "css" | "scss" | "sass" | "less" | "html" | "htm" | "hbs" | "mustache" | "jinja" | "j2" => {
            IconCategory::StyleTemplate
        }
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "svg" | "ico" | "bmp" | "tiff" | "avif" => {
            IconCategory::Image
        }
        "mp3" | "wav" | "ogg" | "flac" | "aac" | "mp4" | "mov" | "mkv" | "webm" | "avi" => {
            IconCategory::Media
        }
        "zip" | "tar" | "gz" | "bz2" | "xz" | "zst" | "7z" | "rar" | "crate" | "deb" | "rpm"
        | "whl" => IconCategory::ArchivePackage,
        "exe" | "dll" | "so" | "dylib" | "bin" | "wasm" | "class" | "o" | "a" => {
            IconCategory::ExecutableBinary
        }
        "" if lower.starts_with('.') => IconCategory::ConfigBuild,
        _ => IconCategory::Generic,
    }
}

/// Resolve a semantic identity. User rules always dominate bundled rules: exact basename,
/// longest compound suffix, and final extension precede bundled exact, compound, and final
/// extension rules. Filename prefixes and categories are fallback-only.
#[must_use]
pub fn resolve_icon_id(path: &str, overrides: &FileIconOverrides) -> IconId {
    let lower = basename(path).to_ascii_lowercase();
    overrides
        .names
        .get(&lower)
        .copied()
        .or_else(|| longest_compound_suffix(&lower, &overrides.extensions))
        .or_else(|| {
            final_extension(&lower)
                .and_then(|extension| overrides.extensions.get(extension))
                .copied()
        })
        .or_else(|| exact_icon(&lower))
        .or_else(|| bundled_compound_icon(&lower))
        .or_else(|| final_extension(&lower).and_then(extension_icon))
        .or_else(|| prefix_icon(&lower))
        .unwrap_or_else(|| classify_file(path).icon_id())
}

fn prefix_icon(name: &str) -> Option<IconId> {
    name.starts_with(".env")
        .then_some(IconId::Config)
        .or_else(|| name.starts_with("dockerfile.").then_some(IconId::Docker))
}

fn basename(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

fn final_extension(name: &str) -> Option<&str> {
    name.rsplit_once('.').map(|(_, extension)| extension).filter(|extension| !extension.is_empty())
}

/// The user compound-suffix tier excludes a final extension, which has its own lower-priority
/// user tier. `types.d.ts` can therefore match `d.ts` before `ts`, while `main.rs` matches only
/// the final-extension tier.
fn longest_compound_suffix(name: &str, rules: &BTreeMap<String, IconId>) -> Option<IconId> {
    name.match_indices('.').find_map(|(at, _)| {
        let suffix = &name[at + 1..];
        suffix.contains('.').then(|| rules.get(suffix).copied()).flatten()
    })
}

/// Resolve presentation for a file and selected renderer. `None` is the only absent mode.
#[must_use]
pub fn file_icon(
    path: &str,
    mode: FileIconMode,
    overrides: &FileIconOverrides,
) -> Option<FileIcon> {
    (mode != FileIconMode::None).then(|| resolve_icon_id(path, overrides).presentation())
}

#[cfg(test)]
mod tests {
    use super::{
        FileIconMode, FileIconOverrides, IconCategory, IconId, classify_file, file_icon,
        resolve_icon_id,
    };
    use unicode_width::UnicodeWidthStr;

    #[test]
    fn lexical_taxonomy_covers_basenames_extensions_case_and_paths() {
        let cases = [
            ("src/main.RS", IconCategory::Source),
            ("Cargo.toml", IconCategory::ConfigBuild),
            ("infra/.env.production", IconCategory::ConfigBuild),
            ("README.MD", IconCategory::Document),
            ("config/settings.YAML", IconCategory::Data),
            ("templates/page.HTML", IconCategory::StyleTemplate),
            ("assets/logo.SVG", IconCategory::Image),
            ("audio/theme.ogg", IconCategory::Media),
            ("dist/app.tar.gz", IconCategory::ArchivePackage),
            ("bin/tool.DYLIB", IconCategory::ExecutableBinary),
            ("nested/no-extension", IconCategory::Generic),
        ];
        for (path, expected) in cases {
            assert_eq!(classify_file(path), expected, "{path}");
        }
    }

    #[test]
    fn resolver_precedence_is_explicit_and_case_insensitive() {
        let mut overrides = FileIconOverrides::default();
        overrides.names.insert("cargo.toml".into(), IconId::Docker);
        overrides.extensions.insert("d.ts".into(), IconId::Json);
        overrides.extensions.insert("toml".into(), IconId::Git);
        assert_eq!(resolve_icon_id("nested/CARGO.TOML", &overrides), IconId::Docker, "user exact");
        assert_eq!(resolve_icon_id("pnpm-lock.yaml", &overrides), IconId::Node, "bundled exact");
        assert_eq!(resolve_icon_id("src/types.d.ts", &overrides), IconId::Json, "user compound");
        assert_eq!(
            resolve_icon_id("archives/source.tar.gz", &overrides),
            IconId::Package,
            "bundled compound"
        );
        assert_eq!(
            resolve_icon_id("config/settings.toml", &overrides),
            IconId::Git,
            "user extension"
        );
        overrides.extensions.insert("rs".into(), IconId::Git);
        assert_eq!(
            resolve_icon_id("Dockerfile.rs", &overrides),
            IconId::Git,
            "user extension beats Dockerfile prefix fallback"
        );
        assert_eq!(
            resolve_icon_id(".env.rs", &overrides),
            IconId::Git,
            "user extension beats .env prefix fallback"
        );
        let no_rules = FileIconOverrides::default();
        assert_eq!(resolve_icon_id("Dockerfile.dev", &no_rules), IconId::Docker, "prefix fallback");
        assert_eq!(resolve_icon_id("src/main.rs", &no_rules), IconId::Rust, "bundled extension");
    }

    #[test]
    fn plain_and_emoji_are_safe_and_nerd_is_explicit_only() {
        let rules = FileIconOverrides::default();
        let icon = file_icon("main.rs", FileIconMode::Emoji, &rules).unwrap();
        assert_eq!(icon.plain, "s");
        assert_eq!(icon.emoji, "🦀");
        assert_eq!(UnicodeWidthStr::width(icon.plain), 1);
        assert_eq!(UnicodeWidthStr::width(icon.emoji), 2);
        for id in IconId::ALL {
            let icon = id.presentation();
            assert_eq!(UnicodeWidthStr::width(icon.plain), 1, "{} plain", id.as_str());
            assert!(UnicodeWidthStr::width(icon.emoji) > 0, "{} emoji", id.as_str());
            assert!(!icon.plain.chars().any(is_private_use));
            assert!(!icon.emoji.chars().any(is_private_use));
        }
        assert_eq!(file_icon("main.rs", FileIconMode::None, &rules), None);
        assert_eq!(file_icon("main.rs", FileIconMode::Nerd, &rules).unwrap().nerd, "");
    }

    fn is_private_use(ch: char) -> bool {
        matches!(ch as u32, 0xE000..=0xF8FF | 0xF_0000..=0xF_FFFD | 0x10_0000..=0x10_FFFD)
    }
}

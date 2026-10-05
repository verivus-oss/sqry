//! One resolver for the Rust macro build options, used by every persisting
//! builder (surface parity W4, design W4-D7).
//!
//! `sqry index --cfg X --expand-cache DIR` records what it was given in the
//! graph manifest (`Manifest::macro_options`, design W4-D6). Before this
//! module existed the options lived for exactly that one build: `sqry
//! update`, `sqry watch`, the auto-rebuild leg, the daemon, the LSP and the
//! MCP all rebuilt with `MacroBuildOptions::default()`, so the next rebuild
//! silently dropped the cfg activation and the materialised cache symbols,
//! and the manifest no longer described what was built.
//!
//! [`resolve_macro_options`] reads the record and overlays a
//! [`MacroOptionsRequest`]: `reset` clears both components, an explicit
//! component replaces the recorded one, an absent component keeps the
//! recorded one. The result says where the options came from
//! ([`MacroOptionsSource`]), the way the plugin roster says where it came
//! from, so every surface can print it. A recorded (or requested) expand
//! cache directory that does not exist is a refusal
//! ([`MacroOptionsError::ExpandCacheMissing`]): building without it would
//! silently narrow the graph, which is the defect class this program exists
//! to close; the way out is explicit (`reset`, or a new directory). A
//! directory whose canonical path is not valid UTF-8 is refused the same
//! way ([`MacroOptionsError::ExpandCachePathNotUtf8`], design W4-D11): the
//! manifest records paths as JSON text, so it could not be reused as
//! recorded.
//!
//! A request is checked on its own first ([`MacroOptionsRequest::validate`],
//! [`MacroRequestError`]): an empty or blank cfg flag, a cfg flag with
//! leading or trailing whitespace, an empty expand cache directory, and a
//! relative expand cache directory that carries a drive prefix or a root
//! (`C:cache`, `\cache` on Windows) name nothing a build can use as given,
//! so they are refused before anything is read or written.

use std::path::{Component, Path, PathBuf};

use crate::graph::unified::persistence::{GraphStorage, ManifestCheck};

use super::entrypoint::MacroBuildOptions;

/// The refusal text for an empty expand cache directory, shared by
/// [`MacroOptionsError::ExpandCacheEmpty`] and
/// [`MacroRequestError::ExpandCacheEmpty`] so both name it the same way.
const EXPAND_CACHE_EMPTY: &str = "the expand cache directory is empty; pass --expand-cache <DIR> \
     (or the expand_cache field) naming a directory, or omit it to keep the recorded one";

/// What a caller asks of the recorded macro options: replace a component,
/// keep it, or drop the record.
///
/// `Default` (and [`MacroOptionsRequest::empty`]) is "reuse whatever the
/// manifest records", which is what every surface without its own flags
/// passes (`sqry update`, `sqry watch`, the auto-rebuild leg, the LSP, the
/// MCP auto-index, a watcher-driven daemon rebuild).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MacroOptionsRequest {
    /// `Some(flags)`: replace the recorded cfg flags with these (an empty
    /// vector clears them). `None`: keep the recorded flags.
    pub cfg_flags: Option<Vec<String>>,
    /// `Some(dir)`: replace the recorded expand cache with this directory.
    /// `None`: keep the recorded one.
    pub expand_cache_dir: Option<PathBuf>,
    /// Drop the recorded options before applying the two fields above
    /// (`sqry index --no-macro-options`, `daemon/rebuild
    /// reset_macro_options`, `rebuild_index reset_macro_options`).
    pub reset: bool,
}

impl MacroOptionsRequest {
    /// Reuse the recorded options unchanged.
    #[must_use]
    pub const fn empty() -> Self {
        Self {
            cfg_flags: None,
            expand_cache_dir: None,
            reset: false,
        }
    }

    /// The request the CLI flags express: `--cfg` values (an explicit
    /// component only when at least one was given, because clap yields an
    /// empty vector for an absent repeatable flag), `--expand-cache`, and
    /// `--no-macro-options`.
    ///
    /// A relative `--expand-cache` is made absolute here, against the
    /// caller's working directory, so it means what it means in the user's
    /// shell even when the request travels to a daemon whose working
    /// directory is elsewhere (`sqry daemon rebuild`).
    ///
    /// This does not check the flags; [`Self::try_from_flags`] does, and is
    /// what a surface taking the flags from a user calls.
    #[must_use]
    pub fn from_flags(cfg_flags: &[String], expand_cache_dir: Option<&Path>, reset: bool) -> Self {
        Self {
            cfg_flags: if cfg_flags.is_empty() {
                None
            } else {
                Some(cfg_flags.to_vec())
            },
            expand_cache_dir: expand_cache_dir
                .map(|dir| std::path::absolute(dir).unwrap_or_else(|_| dir.to_path_buf())),
            reset,
        }
    }

    /// [`Self::from_flags`], after [`Self::validate`] has checked the flags
    /// as the user gave them (before the directory is made absolute, which
    /// would hide a drive prefix or a root behind a resolved path).
    ///
    /// # Errors
    ///
    /// The [`MacroRequestError`] [`Self::validate`] gives for the flags.
    pub fn try_from_flags(
        cfg_flags: &[String],
        expand_cache_dir: Option<&Path>,
        reset: bool,
    ) -> Result<Self, MacroRequestError> {
        let given = Self {
            cfg_flags: (!cfg_flags.is_empty()).then(|| cfg_flags.to_vec()),
            expand_cache_dir: expand_cache_dir.map(Path::to_path_buf),
            reset,
        };
        given.validate()?;
        Ok(Self::from_flags(cfg_flags, expand_cache_dir, reset))
    }

    /// `true` when the request changes nothing (reuse the record).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.cfg_flags.is_none() && self.expand_cache_dir.is_none() && !self.reset
    }

    /// Check the request on its own, before anything is read or written.
    ///
    /// Any existing directory is an acceptable expand cache, the workspace
    /// root included: `.` and `..` name a directory and are resolved and
    /// recorded like any other relative path. What is refused names
    /// nothing a build can use.
    ///
    /// # Errors
    ///
    /// - [`MacroRequestError::CfgFlagEmpty`] when a cfg flag is empty or
    ///   only whitespace.
    /// - [`MacroRequestError::CfgFlagPadded`] when a cfg flag has leading or
    ///   trailing whitespace (` test`): the predicate it was meant to name is
    ///   not the text that would be recorded and matched.
    /// - [`MacroRequestError::ExpandCacheEmpty`] when the expand cache
    ///   directory is the empty path.
    /// - [`MacroRequestError::ExpandCacheNotAnchorable`] when the expand
    ///   cache directory is relative but carries a drive prefix or a root
    ///   (`C:cache`, `\cache`; only Windows has such paths).
    pub fn validate(&self) -> Result<(), MacroRequestError> {
        for flag in self.cfg_flags.as_deref().unwrap_or_default() {
            check_cfg_flag(flag)?;
        }
        if let Some(dir) = &self.expand_cache_dir {
            if dir.as_os_str().is_empty() {
                return Err(MacroRequestError::ExpandCacheEmpty);
            }
            if is_unanchorable_relative(dir) {
                return Err(MacroRequestError::ExpandCacheNotAnchorable { dir: dir.clone() });
            }
        }
        Ok(())
    }
}

/// Check one cfg flag as a user gave it, the rule every surface that takes
/// flags applies ([`MacroOptionsRequest::validate`], and the CLI's `--cfg`
/// value parser, which calls this so clap refuses both shapes alike): an
/// empty or blank flag names no predicate, and a flag with leading or
/// trailing whitespace would be recorded and matched as given, so neither
/// names the predicate the user meant.
///
/// # Errors
///
/// [`MacroRequestError::CfgFlagEmpty`] for an empty or blank flag, and
/// [`MacroRequestError::CfgFlagPadded`] for a flag with leading or trailing
/// whitespace.
pub fn check_cfg_flag(flag: &str) -> Result<(), MacroRequestError> {
    if flag.trim().is_empty() {
        Err(MacroRequestError::CfgFlagEmpty)
    } else if flag.trim() != flag {
        Err(MacroRequestError::CfgFlagPadded {
            flag: flag.to_string(),
        })
    } else {
        Ok(())
    }
}

/// The facts about a path that decide whether a relative directory can be
/// anchored to the directory being indexed. They are read from a real path
/// by [`PathShape::of`]; [`PathShape::is_unanchorable`] decides from the
/// facts alone, so the decision is tested on every platform even though only
/// Windows has paths that are relative and start at a prefix or a root.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PathShape {
    /// [`Path::is_relative`].
    relative: bool,
    /// The first component is a [`Component::Prefix`] (`C:`) or a
    /// [`Component::RootDir`] (`\`, `/`).
    starts_at_prefix_or_root: bool,
}

impl PathShape {
    /// Read the two facts from `dir`. Why the first component alone is the
    /// right one to read:
    ///
    /// - [`Path::components`] yields a prefix, when the path has one, as its
    ///   first component, and a root directory, when it has one, right after
    ///   the prefix and before anything else. So a path has a prefix or a
    ///   root exactly when its first component is one of the two, and no
    ///   later component can be either.
    /// - [`PathBuf::push`], which [`Path::join`] uses, keeps the base only
    ///   when the pushed path starts with neither: one that starts at a root
    ///   replaces all of the base but its prefix, and one that starts at a
    ///   prefix replaces the base whole. The first component is therefore
    ///   all that `join` decides on.
    /// - [`Path::is_relative`] stays a separate fact: an absolute path also
    ///   starts at a prefix or a root, and it is used as it is, never
    ///   anchored.
    fn of(dir: &Path) -> Self {
        Self {
            relative: dir.is_relative(),
            starts_at_prefix_or_root: matches!(
                dir.components().next(),
                Some(Component::Prefix(_) | Component::RootDir)
            ),
        }
    }

    /// `true` for a relative path that starts at a drive prefix or a root,
    /// which only Windows has: `C:cache` is relative to the working
    /// directory of drive `C:`, and `\cache` to the root of the current
    /// drive. `Path::join` replaces the base with such a path instead of
    /// appending it, so joined to the directory being indexed it would still
    /// name a directory found through this process's working directory.
    fn is_unanchorable(self) -> bool {
        self.relative && self.starts_at_prefix_or_root
    }
}

/// [`PathShape::is_unanchorable`] for `dir`.
fn is_unanchorable_relative(dir: &Path) -> bool {
    PathShape::of(dir).is_unanchorable()
}

/// The directory being indexed as an absolute, canonical path: the anchor
/// for a relative expand cache directory. A caller may name the root
/// relative to its working directory (`.` is the CLI default, and
/// `outer/ws` or `link` are as valid); joined to such a root a relative
/// directory would still be relative, and [`overlay`] refuses a relative
/// directory unread. The root is canonical when it exists (a symlinked root
/// anchors in its target, where the canonical record points anyway) and
/// made absolute against this process's working directory otherwise, which
/// is where a relative root was given. A root that cannot be made absolute
/// (an empty path, a working directory that is gone) is returned unchanged,
/// so the directory stays relative and is refused by name.
fn absolute_root(root: &Path) -> PathBuf {
    root.canonicalize()
        .or_else(|_| std::path::absolute(root))
        .unwrap_or_else(|_| root.to_path_buf())
}

/// A relative directory joined to `root`, the directory being indexed, which
/// [`absolute_root`] made absolute. The directory the record or the request
/// names is anchored there, never to this process's working directory. An
/// empty path and a relative path that carries a drive prefix or a root are
/// returned unchanged: joining either would name the root itself or replace
/// it, so the resolver refuses them instead (they stay relative, and a
/// relative directory never reaches the filesystem).
fn anchor_to_root(root: &Path, dir: PathBuf) -> PathBuf {
    if dir.is_relative() && !dir.as_os_str().is_empty() && !is_unanchorable_relative(&dir) {
        root.join(dir)
    } else {
        dir
    }
}

/// Where the resolved options came from, so a surface can say so.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MacroOptionsSource {
    /// At least one component came from the request (a flag or a wire
    /// field); the rest, if any, from the record.
    Explicit,
    /// Every component came from the manifest's record and the request
    /// changed nothing.
    Recorded,
    /// No record and no request (or a reset with nothing explicit): the
    /// build carries no macro options.
    None,
}

impl MacroOptionsSource {
    /// Stable wire spelling, for JSON output.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Explicit => "explicit",
            Self::Recorded => "recorded",
            Self::None => "none",
        }
    }
}

/// The options a build will run with and where they came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedMacroOptions {
    /// The options to put in `BuildConfig::macro_options`.
    pub options: MacroBuildOptions,
    /// Where `options` came from.
    pub source: MacroOptionsSource,
}

impl ResolvedMacroOptions {
    /// No options at all, from nowhere.
    #[must_use]
    pub fn none() -> Self {
        Self {
            options: MacroBuildOptions::default(),
            source: MacroOptionsSource::None,
        }
    }

    /// One human-readable line: the components and their source, for the
    /// `sqry index` source line beside the plugin-selection line. A cfg flag
    /// that is empty, blank, or has leading or trailing whitespace is shown
    /// quoted, so `[""]` is never printed as an empty list and `[" test"]`
    /// never as `[test]`. [`MacroOptionsRequest::validate`] refuses such a
    /// flag, so it reaches a record only through a hand-edited manifest or
    /// a surface that records a request without validating it.
    #[must_use]
    pub fn describe(&self) -> String {
        let mut parts = Vec::new();
        if !self.options.cfg_flags.is_empty() {
            let flags: Vec<String> = self
                .options
                .cfg_flags
                .iter()
                .map(|flag| {
                    if check_cfg_flag(flag).is_err() {
                        format!("{flag:?}")
                    } else {
                        flag.clone()
                    }
                })
                .collect();
            parts.push(format!("cfg [{}]", flags.join(", ")));
        }
        if let Some(dir) = &self.options.expand_cache_dir {
            parts.push(format!("expand cache {}", dir.display()));
        }
        let what = if parts.is_empty() {
            "none".to_string()
        } else {
            parts.join(", ")
        };
        let from = match self.source {
            MacroOptionsSource::Explicit => "from the flags",
            MacroOptionsSource::Recorded => "recorded in the manifest",
            MacroOptionsSource::None => "no record, no flags",
        };
        format!("{what} ({from})")
    }
}

/// What to do when the manifest exists but cannot be read.
///
/// The plugin roster owns the unreadable-manifest policy (surface parity W1,
/// design D9): an explicit rebuild falls back and records the fallback, every
/// other write refuses. The macro options follow the same decision, so a
/// caller passes the rule its roster resolution used.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnreadableManifestRule {
    /// Refuse with [`MacroOptionsError::ManifestUnreadable`]; nothing is
    /// built.
    Refuse,
    /// Treat the unreadable manifest as no record (the roster fell back the
    /// same way and records the fallback).
    TreatAsNoRecord,
}

/// Why a [`MacroOptionsRequest`] is refused on its own, before the manifest
/// is read ([`MacroOptionsRequest::validate`]). Nothing was written.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum MacroRequestError {
    /// A cfg flag is empty or only whitespace, so it names no predicate.
    /// Recorded, it would be reused by every rebuild while activating
    /// nothing.
    #[error(
        "a cfg flag is empty; an empty flag names no predicate, so pass --cfg <PREDICATE> (or a \
         non-empty cfg_flags item), or omit it to keep the recorded flags"
    )]
    CfgFlagEmpty,
    /// A cfg flag has leading or trailing whitespace (` test`). Recorded as
    /// given it would be matched as given, so it would not name the
    /// predicate it was meant to; the user trims it.
    #[error(
        "cfg flag {flag:?} has leading or trailing whitespace, so it is not the predicate it \
         names; pass --cfg <PREDICATE> (or a cfg_flags item) without the surrounding whitespace"
    )]
    CfgFlagPadded {
        /// The flag as the request carried it.
        flag: String,
    },
    /// The expand cache directory is the empty path, which names no
    /// directory; joined to the directory being indexed it would name that
    /// directory itself.
    #[error("{}", EXPAND_CACHE_EMPTY)]
    ExpandCacheEmpty,
    /// The expand cache directory is relative but carries a drive prefix or
    /// a root (`C:cache`, `\cache`; only Windows has such paths). It is
    /// neither absolute nor relative to the directory being indexed (or the
    /// caller's working directory), so it could only be found through this
    /// process's per-drive working directory.
    #[error(
        "expand cache directory {} is relative but carries a drive prefix or a root, so the \
         directory it names depends on this process's drive and its working directory there; \
         pass an absolute directory, or a relative one without a drive prefix or a root",
        dir.display()
    )]
    ExpandCacheNotAnchorable {
        /// The directory as the request named it.
        dir: PathBuf,
    },
}

/// Why the macro options could not be resolved. Nothing was written.
#[derive(Debug, thiserror::Error)]
pub enum MacroOptionsError {
    /// The expand cache directory the build would read is not a directory
    /// this build can use: it does not exist, it is not a directory (a file,
    /// a dangling link), or it could not be anchored to the directory being
    /// indexed (an empty path, or a relative path that carries a drive prefix
    /// or a root, which only a hand-edited manifest or an unchecked request
    /// can hold). The manifest (or the request) asks for an input the process
    /// cannot honour; building without it would silently drop every
    /// macro-generated symbol, so the build is refused. Name a directory
    /// that exists, or reset the recorded options. The message is chosen by
    /// the shape of `dir` alone; nothing is read to render it.
    #[error("{}", expand_cache_missing_message(dir))]
    ExpandCacheMissing {
        /// The directory the record or the request named, anchored to the
        /// directory being indexed when it was relative.
        dir: PathBuf,
    },
    /// The expand cache directory exists, but its canonical path is not
    /// valid UTF-8 (surface parity W4, design W4-D11). The manifest records
    /// the canonical path as JSON text, so it could only be recorded
    /// lossily, and the next rebuild would refuse the altered path. The
    /// directory is refused here, where it is accepted, and nothing is
    /// written.
    #[error(
        "expand cache directory {} is not valid UTF-8; the graph manifest records the \
         directory as JSON text, so it cannot be recorded and reused as given; pass \
         --expand-cache <DIR> (or the expand_cache field) naming a directory whose path is \
         valid UTF-8, or --no-macro-options (reset_macro_options) to drop the recorded macro \
         options",
        dir.display()
    )]
    ExpandCachePathNotUtf8 {
        /// The canonical directory, the form the manifest would record.
        dir: PathBuf,
    },
    /// The request named an empty expand cache directory (integration of W1
    /// and W4). An empty path names no directory, and joined to the
    /// directory being indexed it would name that directory itself, so it is
    /// refused before anything is resolved or written.
    #[error("{}", EXPAND_CACHE_EMPTY)]
    ExpandCacheEmpty,
    /// The manifest exists but cannot be read, under
    /// [`UnreadableManifestRule::Refuse`].
    #[error(
        "manifest at {} cannot be read ({reason}); repair with: sqry index --force <root>",
        manifest_path.display()
    )]
    ManifestUnreadable {
        /// The manifest file.
        manifest_path: PathBuf,
        /// The parse or I/O reason.
        reason: String,
    },
    /// The manifest records a cfg flag that names no predicate: empty,
    /// blank, or with leading or trailing whitespace. No surface records
    /// such a flag (each refuses it in the request check), so the record was
    /// edited by hand; reused, it would activate nothing (or not the
    /// predicate meant) on every rebuild. A request that replaces the
    /// recorded flags, or drops the record, is not refused.
    #[error(
        "the index manifest records cfg flag {flag:?}, which {}; pass --cfg <PREDICATE> (or \
         cfg_flags) to replace the recorded flags, or drop the record with --no-macro-options \
         (reset_macro_options)",
        recorded_cfg_flag_problem(flag)
    )]
    RecordedCfgFlagInvalid {
        /// The flag as the manifest records it.
        flag: String,
    },
}

/// What is wrong with a recorded cfg flag [`check_cfg_flag`] refuses, for
/// [`MacroOptionsError::RecordedCfgFlagInvalid`]'s message.
fn recorded_cfg_flag_problem(flag: &str) -> &'static str {
    match check_cfg_flag(flag) {
        Err(MacroRequestError::CfgFlagPadded { .. }) => {
            "has leading or trailing whitespace, so it is not the predicate it names"
        }
        _ => "is empty or blank, so it names no predicate",
    }
}

/// Why the directory a [`MacroOptionsError::ExpandCacheMissing`] names
/// cannot be used, chosen by its shape alone (nothing is read): an empty
/// path names no directory; a relative path with a drive prefix or a root
/// depends on this process's drive; any other relative path could not be
/// anchored because the directory being indexed could not be made absolute;
/// an absolute path does not exist or is not a directory (a file, a
/// dangling link). No remedy: each surface appends the one its callers can
/// act on ([`MacroOptionsError`]'s `Display` appends one naming both the CLI
/// flags and the wire fields).
#[must_use]
pub fn expand_cache_missing_reason(dir: &Path) -> String {
    if dir.as_os_str().is_empty() {
        "the expand cache directory is empty, so it names no directory".to_string()
    } else if is_unanchorable_relative(dir) {
        format!(
            "expand cache directory {} is relative but carries a drive prefix or a root, so the \
             directory it names depends on this process's drive and its working directory there",
            dir.display()
        )
    } else if dir.is_relative() {
        format!(
            "expand cache directory {} is relative and the directory being indexed could not be \
             made absolute, so it cannot be anchored there",
            dir.display()
        )
    } else {
        format!(
            "expand cache directory {} does not exist or is not a directory",
            dir.display()
        )
    }
}

/// The [`MacroOptionsError::ExpandCacheMissing`] message for `dir`: the
/// shape-aware [`expand_cache_missing_reason`] and a remedy that names both
/// spellings (the CLI flag and the wire field) and holds whether the
/// directory came from the request or from the manifest.
fn expand_cache_missing_message(dir: &Path) -> String {
    const REMEDY: &str = "name a directory that exists (--expand-cache <DIR>, or the \
                          expand_cache field), or drop the macro options the manifest records \
                          (--no-macro-options, or reset_macro_options)";
    format!("{}; {REMEDY}", expand_cache_missing_reason(dir))
}

/// Resolve the macro options a build at `root` will run with: the manifest's
/// record (no manifest, or no `macro_options` key, is no record) overlaid by
/// `request` (`reset` clears both components; an explicit component replaces
/// the recorded one; an absent component keeps it). The expand cache
/// directory, when set, must be an existing directory and is canonicalised so
/// the record is absolute.
///
/// A relative directory, requested or recorded, resolves against `root`,
/// never against this process's working directory: a request can only carry
/// one from a surface with no working directory of its own (MCP, daemon IPC;
/// the CLI makes its flag absolute in [`MacroOptionsRequest::from_flags`]),
/// and a manifest only holds one when it was edited by hand, so the answer
/// does not depend on where a daemon was started. `root` is the directory
/// being indexed: the MCP `rebuild_index` passes the `path` argument's
/// directory (a subdirectory of the workspace, or a file's parent
/// directory), the daemon the workspace root it rebuilds. `root` itself may
/// be relative to this process's working directory (the CLI's default root
/// is `.`): the anchor is `root` made absolute and canonical, so the joined
/// directory is absolute however the root was spelled, a symlinked root
/// included. Any existing directory is accepted, the root included: `.`
/// names the root and `..` its parent, and each is recorded as that
/// directory's canonical path.
///
/// This refuses an empty requested directory itself, but it does not run
/// [`MacroOptionsRequest::validate`]; a caller taking a request from a user
/// calls that first.
///
/// The record is read through [`GraphStorage::try_load_manifest`], which
/// waits out another writer's persist rather than reading its moved-aside
/// manifest as no record (decision D-i8-1). A caller that persists what this
/// returns publishes the record current at its publication only if it holds
/// the index's persist lock from this call to the commit (D-i8-2).
///
/// # Errors
///
/// - [`MacroOptionsError::ExpandCacheEmpty`] when the request names the
///   empty path as its expand cache directory.
/// - [`MacroOptionsError::ExpandCacheMissing`] when the resolved directory
///   does not exist or is not a directory, or (recorded, or requested
///   without [`MacroOptionsRequest::validate`]) is empty or relative with a
///   drive prefix or a root.
/// - [`MacroOptionsError::ExpandCachePathNotUtf8`] when the resolved
///   directory's canonical path is not valid UTF-8, so the manifest could
///   not record it as given.
/// - [`MacroOptionsError::ManifestUnreadable`] when the manifest exists but
///   cannot be read and `rule` is [`UnreadableManifestRule::Refuse`].
pub fn resolve_macro_options(
    root: &Path,
    request: &MacroOptionsRequest,
    rule: UnreadableManifestRule,
) -> Result<ResolvedMacroOptions, MacroOptionsError> {
    // An empty directory names none: `root.join("")` is the root, so without
    // this an empty request would record the indexed directory as the cache.
    if request
        .expand_cache_dir
        .as_deref()
        .is_some_and(|dir| dir.as_os_str().is_empty())
    {
        return Err(MacroOptionsError::ExpandCacheEmpty);
    }
    // The anchor is absolute even when `root` is not (`.`, `outer/ws`): a
    // relative root would leave the joined directory relative, and a
    // relative directory is refused unread.
    let anchor = absolute_root(root);
    let anchored;
    let request = match &request.expand_cache_dir {
        Some(dir) if dir.is_relative() => {
            anchored = MacroOptionsRequest {
                expand_cache_dir: Some(anchor_to_root(&anchor, dir.clone())),
                ..request.clone()
            };
            &anchored
        }
        _ => request,
    };
    let storage = GraphStorage::new(root);
    let recorded = match storage.try_load_manifest() {
        ManifestCheck::Present(manifest) => manifest.macro_options.as_ref().map(|record| {
            let mut options = record.to_build_options();
            options.expand_cache_dir = options
                .expand_cache_dir
                .map(|dir| anchor_to_root(&anchor, dir));
            options
        }),
        ManifestCheck::Missing => None,
        ManifestCheck::Corrupt(err) => match rule {
            UnreadableManifestRule::Refuse => {
                return Err(MacroOptionsError::ManifestUnreadable {
                    manifest_path: storage.manifest_path().to_path_buf(),
                    reason: err.to_string(),
                });
            }
            UnreadableManifestRule::TreatAsNoRecord => None,
        },
    };
    overlay(recorded, request)
}

/// The overlay rule on its own (no manifest read), shared with the tests.
/// `recorded` and `request` arrive anchored to the directory being indexed,
/// so a directory still relative here is one [`anchor_to_root`] left
/// alone.
fn overlay(
    recorded: Option<MacroBuildOptions>,
    request: &MacroOptionsRequest,
) -> Result<ResolvedMacroOptions, MacroOptionsError> {
    let base = if request.reset {
        None
    } else {
        recorded.filter(|options| !options.is_empty())
    };
    let explicit = request.cfg_flags.is_some() || request.expand_cache_dir.is_some();
    // Flags the build would take from the record are checked as a request's
    // are: a hand-edited record can hold one no surface would record.
    if request.cfg_flags.is_none()
        && let Some(recorded) = &base
        && let Some(flag) = recorded
            .cfg_flags
            .iter()
            .find(|flag| check_cfg_flag(flag).is_err())
    {
        return Err(MacroOptionsError::RecordedCfgFlagInvalid { flag: flag.clone() });
    }
    let mut options = base.clone().unwrap_or_default();
    if let Some(flags) = &request.cfg_flags {
        options.cfg_flags.clone_from(flags);
    }
    if let Some(dir) = &request.expand_cache_dir {
        options.expand_cache_dir = Some(dir.clone());
    }
    if let Some(dir) = options.expand_cache_dir.take() {
        // A relative directory here is empty or carries a drive prefix or a
        // root. Asking the filesystem about it would answer for this
        // process's working directory, so it is refused unread.
        if dir.is_relative() || !dir.is_dir() {
            return Err(MacroOptionsError::ExpandCacheMissing { dir });
        }
        let canonical = dir.canonicalize().unwrap_or(dir);
        // The canonical form is what the manifest records, so that is the
        // form checked: a UTF-8 request can canonicalise to a path that is
        // not (a symlink into a directory whose name is not valid UTF-8).
        if canonical.to_str().is_none() {
            return Err(MacroOptionsError::ExpandCachePathNotUtf8 { dir: canonical });
        }
        options.expand_cache_dir = Some(canonical);
    }
    let source = if options.is_empty() {
        MacroOptionsSource::None
    } else if explicit {
        MacroOptionsSource::Explicit
    } else {
        MacroOptionsSource::Recorded
    };
    Ok(ResolvedMacroOptions { options, source })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::unified::persistence::{BuildProvenance, MacroOptionsManifest, Manifest};

    fn write_manifest(root: &Path, record: Option<MacroOptionsManifest>) {
        let storage = GraphStorage::new(root);
        std::fs::create_dir_all(storage.graph_dir()).expect("graph dir");
        Manifest::new(
            root.display().to_string(),
            0,
            0,
            "sha",
            BuildProvenance::new("test", "test"),
        )
        .with_macro_options(record)
        .save(storage.manifest_path())
        .expect("manifest written");
    }

    fn cache_dir(root: &Path, name: &str) -> PathBuf {
        let dir = root.join(name);
        std::fs::create_dir_all(&dir).expect("cache dir");
        dir.canonicalize().expect("canonical cache dir")
    }

    /// A hand-edited record holding a cfg flag no surface would record (an
    /// empty, a blank or a padded one) is refused, naming the flag and both
    /// ways out; a request that replaces the recorded flags, or drops the
    /// record, resolves (integration round 7, DAEMON_FOLLOWUP). Before, the
    /// flag was reused, and recorded again, by every rebuild.
    #[test]
    fn a_recorded_cfg_flag_that_names_no_predicate_is_refused() {
        for (flag, problem) in [
            ("", "is empty or blank"),
            ("  ", "is empty or blank"),
            (" test", "has leading or trailing whitespace"),
        ] {
            let tmp = tempfile::tempdir().expect("tempdir");
            write_manifest(
                tmp.path(),
                Some(MacroOptionsManifest {
                    cfg_flags: vec!["unix".to_string(), flag.to_string()],
                    expand_cache_dir: None,
                }),
            );
            let refused = resolve_macro_options(
                tmp.path(),
                &MacroOptionsRequest::empty(),
                UnreadableManifestRule::Refuse,
            )
            .expect_err("a recorded flag that names no predicate is refused");
            let rendered = refused.to_string();
            assert!(
                matches!(&refused, MacroOptionsError::RecordedCfgFlagInvalid { flag: recorded } if recorded == flag),
                "{flag:?}: {refused:?}"
            );
            assert!(
                rendered.contains(problem)
                    && rendered.contains("--no-macro-options")
                    && rendered.contains("cfg_flags"),
                "{rendered}"
            );
            let replaced = resolve_macro_options(
                tmp.path(),
                &MacroOptionsRequest {
                    cfg_flags: Some(vec!["test".to_string()]),
                    ..MacroOptionsRequest::empty()
                },
                UnreadableManifestRule::Refuse,
            )
            .expect("replacing the recorded flags resolves");
            assert_eq!(replaced.options.cfg_flags, vec!["test".to_string()]);
            let reset = resolve_macro_options(
                tmp.path(),
                &MacroOptionsRequest {
                    reset: true,
                    ..MacroOptionsRequest::empty()
                },
                UnreadableManifestRule::Refuse,
            )
            .expect("dropping the record resolves");
            assert_eq!(reset.options, MacroBuildOptions::default());
        }
    }

    /// T9: no manifest gives `None` from nowhere.
    #[test]
    fn no_manifest_resolves_to_none() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let resolved = resolve_macro_options(
            tmp.path(),
            &MacroOptionsRequest::empty(),
            UnreadableManifestRule::Refuse,
        )
        .expect("resolves");
        assert_eq!(resolved.source, MacroOptionsSource::None);
        assert_eq!(resolved.options, MacroBuildOptions::default());
        assert_eq!(resolved.describe(), "none (no record, no flags)");
    }

    /// T9: a manifest without the key is no record either.
    #[test]
    fn manifest_without_the_key_resolves_to_none() {
        let tmp = tempfile::tempdir().expect("tempdir");
        write_manifest(tmp.path(), None);
        let resolved = resolve_macro_options(
            tmp.path(),
            &MacroOptionsRequest::empty(),
            UnreadableManifestRule::Refuse,
        )
        .expect("resolves");
        assert_eq!(resolved.source, MacroOptionsSource::None);
    }

    /// T9: the record alone gives `Recorded` with both components.
    #[test]
    fn record_only_resolves_to_recorded() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let cache = cache_dir(tmp.path(), "expand");
        write_manifest(
            tmp.path(),
            Some(MacroOptionsManifest {
                cfg_flags: vec!["test".to_string()],
                expand_cache_dir: Some(cache.display().to_string()),
            }),
        );
        let resolved = resolve_macro_options(
            tmp.path(),
            &MacroOptionsRequest::empty(),
            UnreadableManifestRule::Refuse,
        )
        .expect("resolves");
        assert_eq!(resolved.source, MacroOptionsSource::Recorded);
        assert_eq!(resolved.options.cfg_flags, vec!["test".to_string()]);
        assert_eq!(
            resolved.options.expand_cache_dir.as_deref(),
            Some(cache.as_path())
        );
        assert_eq!(
            resolved.describe(),
            format!(
                "cfg [test], expand cache {} (recorded in the manifest)",
                cache.display()
            )
        );
    }

    /// T9: an explicit component replaces only that component.
    #[test]
    fn explicit_component_replaces_only_that_component() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let cache = cache_dir(tmp.path(), "expand");
        write_manifest(
            tmp.path(),
            Some(MacroOptionsManifest {
                cfg_flags: vec!["test".to_string()],
                expand_cache_dir: Some(cache.display().to_string()),
            }),
        );
        let resolved = resolve_macro_options(
            tmp.path(),
            &MacroOptionsRequest::from_flags(&["feature=serde".to_string()], None, false),
            UnreadableManifestRule::Refuse,
        )
        .expect("resolves");
        assert_eq!(resolved.source, MacroOptionsSource::Explicit);
        assert_eq!(
            resolved.options.cfg_flags,
            vec!["feature=serde".to_string()]
        );
        assert_eq!(
            resolved.options.expand_cache_dir.as_deref(),
            Some(cache.as_path()),
            "the recorded cache is kept when only the flags are explicit"
        );

        let other = cache_dir(tmp.path(), "other");
        let resolved = resolve_macro_options(
            tmp.path(),
            &MacroOptionsRequest::from_flags(&[], Some(&other), false),
            UnreadableManifestRule::Refuse,
        )
        .expect("resolves");
        assert_eq!(resolved.source, MacroOptionsSource::Explicit);
        assert_eq!(
            resolved.options.cfg_flags,
            vec!["test".to_string()],
            "the recorded flags are kept when only the cache is explicit"
        );
        assert_eq!(
            resolved.options.expand_cache_dir.as_deref(),
            Some(other.as_path())
        );
    }

    /// T9: `reset` clears both components; with nothing explicit the result
    /// is `None`, with an explicit flag it is `Explicit` over an empty base.
    #[test]
    fn reset_clears_the_record() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let cache = cache_dir(tmp.path(), "expand");
        write_manifest(
            tmp.path(),
            Some(MacroOptionsManifest {
                cfg_flags: vec!["test".to_string()],
                expand_cache_dir: Some(cache.display().to_string()),
            }),
        );
        let resolved = resolve_macro_options(
            tmp.path(),
            &MacroOptionsRequest::from_flags(&[], None, true),
            UnreadableManifestRule::Refuse,
        )
        .expect("resolves");
        assert_eq!(resolved.source, MacroOptionsSource::None);
        assert_eq!(resolved.options, MacroBuildOptions::default());

        let resolved = resolve_macro_options(
            tmp.path(),
            &MacroOptionsRequest::from_flags(&["unix".to_string()], None, true),
            UnreadableManifestRule::Refuse,
        )
        .expect("resolves");
        assert_eq!(resolved.source, MacroOptionsSource::Explicit);
        assert_eq!(resolved.options.cfg_flags, vec!["unix".to_string()]);
        assert_eq!(
            resolved.options.expand_cache_dir, None,
            "reset dropped the cache"
        );
    }

    /// T9: a recorded cache directory that no longer exists is refused,
    /// naming the directory; reset is the way out.
    #[test]
    fn missing_recorded_cache_dir_is_refused_by_name() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let gone = tmp.path().join("gone-cache");
        write_manifest(
            tmp.path(),
            Some(MacroOptionsManifest {
                cfg_flags: vec![],
                expand_cache_dir: Some(gone.display().to_string()),
            }),
        );
        let err = resolve_macro_options(
            tmp.path(),
            &MacroOptionsRequest::empty(),
            UnreadableManifestRule::Refuse,
        )
        .expect_err("a missing recorded cache must refuse");
        match &err {
            MacroOptionsError::ExpandCacheMissing { dir } => assert_eq!(dir, &gone),
            other => panic!("expected ExpandCacheMissing, got {other:?}"),
        }
        let rendered = err.to_string();
        assert!(
            rendered.contains(&gone.display().to_string())
                && rendered.contains("--no-macro-options"),
            "the refusal names the directory and the way out: {rendered}"
        );

        let resolved = resolve_macro_options(
            tmp.path(),
            &MacroOptionsRequest::from_flags(&[], None, true),
            UnreadableManifestRule::Refuse,
        )
        .expect("reset drops the missing record");
        assert_eq!(resolved.source, MacroOptionsSource::None);

        // An explicit directory that does not exist is refused the same way.
        let err = resolve_macro_options(
            tmp.path(),
            &MacroOptionsRequest::from_flags(&[], Some(&gone), true),
            UnreadableManifestRule::Refuse,
        )
        .expect_err("an explicit missing cache must refuse");
        assert!(matches!(err, MacroOptionsError::ExpandCacheMissing { .. }));
    }

    /// A relative directory in a request names a directory under the workspace
    /// root. The name exists only under the root, so a resolver that asked the
    /// process working directory would answer missing.
    #[test]
    fn a_relative_expand_cache_resolves_against_the_workspace_root() {
        let root = tempfile::tempdir().expect("root");
        let name = "w4-relative-cache-under-the-root-only";
        std::fs::create_dir(root.path().join(name)).expect("cache dir");
        let request = MacroOptionsRequest {
            cfg_flags: None,
            expand_cache_dir: Some(PathBuf::from(name)),
            reset: false,
        };
        let resolved = resolve_macro_options(root.path(), &request, UnreadableManifestRule::Refuse)
            .expect("a relative directory under the root resolves");
        assert_eq!(
            resolved.options.expand_cache_dir,
            Some(root.path().join(name).canonicalize().expect("canonical"))
        );
    }

    /// The other direction: `src` exists in this test's working directory (the
    /// crate root) and not under the workspace root, so it must be refused as
    /// missing rather than found where the process happens to stand.
    #[test]
    fn a_relative_expand_cache_never_resolves_against_the_process_directory() {
        assert!(
            Path::new("src").is_dir(),
            "precondition: the test runs from the crate root, which holds src"
        );
        let root = tempfile::tempdir().expect("root");
        let request = MacroOptionsRequest {
            cfg_flags: None,
            expand_cache_dir: Some(PathBuf::from("src")),
            reset: false,
        };
        let err = resolve_macro_options(root.path(), &request, UnreadableManifestRule::Refuse)
            .expect_err("src is not under the workspace root");
        assert!(
            matches!(err, MacroOptionsError::ExpandCacheMissing { ref dir } if dir == &canonical(root.path()).join("src")),
            "refused as missing under the root, got {err:?}"
        );
    }

    /// An empty directory is refused, not joined to the root (which would
    /// record the root itself as the cache).
    #[test]
    fn an_empty_expand_cache_is_refused() {
        let root = tempfile::tempdir().expect("root");
        let request = MacroOptionsRequest {
            cfg_flags: None,
            expand_cache_dir: Some(PathBuf::new()),
            reset: false,
        };
        let err = resolve_macro_options(root.path(), &request, UnreadableManifestRule::Refuse)
            .expect_err("an empty expand cache directory names none");
        assert!(
            matches!(err, MacroOptionsError::ExpandCacheEmpty),
            "refused as empty, got {err:?}"
        );
    }

    /// The CLI's flag is absolute before it leaves the process, against the
    /// caller's working directory, so a daemon elsewhere sees the same path.
    #[test]
    fn from_flags_makes_a_relative_expand_cache_absolute_against_the_caller() {
        let request = MacroOptionsRequest::from_flags(&[], Some(Path::new("rel/cache")), false);
        let cwd = std::env::current_dir().expect("cwd");
        assert_eq!(
            request.expand_cache_dir,
            Some(cwd.join("rel").join("cache"))
        );
        // An absolute directory passes through unchanged. It is built from the
        // working directory so it is absolute on every platform: `/abs/cache`
        // has no drive prefix on Windows, where `absolute` would complete it.
        let absolute_dir = cwd.join("abs").join("cache");
        let absolute = MacroOptionsRequest::from_flags(&[], Some(&absolute_dir), false);
        assert_eq!(absolute.expand_cache_dir, Some(absolute_dir));
    }

    /// U2u (surface parity W4 round 2, design W4-D11): an explicit expand
    /// cache directory whose name is not valid UTF-8 is refused where it is
    /// accepted, naming the canonical directory, and the message says why
    /// and names both ways out.
    #[cfg(unix)]
    #[test]
    fn a_non_utf8_expand_cache_directory_is_refused_by_name() {
        use std::os::unix::ffi::OsStringExt;

        let tmp = tempfile::tempdir().expect("tempdir");
        let mut name = b"expand-cache-".to_vec();
        name.push(0xff);
        let dir = tmp.path().join(std::ffi::OsString::from_vec(name));
        std::fs::create_dir_all(&dir).expect("non-UTF-8 dir");
        assert!(dir.to_str().is_none(), "instrument: the name is not UTF-8");

        let err = resolve_macro_options(
            tmp.path(),
            &MacroOptionsRequest::from_flags(&[], Some(&dir), false),
            UnreadableManifestRule::Refuse,
        )
        .expect_err("a directory the manifest cannot record must be refused");
        let canonical = dir.canonicalize().expect("canonical");
        match &err {
            MacroOptionsError::ExpandCachePathNotUtf8 { dir: refused } => {
                assert_eq!(refused, &canonical, "the canonical form is named");
            }
            other => panic!("expected ExpandCachePathNotUtf8, got {other:?}"),
        }
        let rendered = err.to_string();
        println!("{rendered}");
        assert!(rendered.contains("not valid UTF-8"), "{rendered}");
        assert!(rendered.contains("JSON text"), "{rendered}");
        assert!(
            rendered.contains("--expand-cache") && rendered.contains("--no-macro-options"),
            "{rendered}"
        );
    }

    /// U2u: the canonical form is what is checked. A UTF-8 symlink that
    /// resolves into a directory whose name is not valid UTF-8 is refused,
    /// because the canonical path is what the manifest would record.
    #[cfg(unix)]
    #[test]
    fn a_utf8_symlink_into_a_non_utf8_directory_is_refused() {
        use std::os::unix::ffi::OsStringExt;

        let tmp = tempfile::tempdir().expect("tempdir");
        let mut name = b"target-".to_vec();
        name.push(0xfe);
        let target = tmp.path().join(std::ffi::OsString::from_vec(name));
        std::fs::create_dir_all(&target).expect("non-UTF-8 target");
        let link = tmp.path().join("expand-cache-link");
        std::os::unix::fs::symlink(&target, &link).expect("symlink");
        assert!(link.to_str().is_some(), "instrument: the request is UTF-8");

        let err = resolve_macro_options(
            tmp.path(),
            &MacroOptionsRequest::from_flags(&[], Some(&link), false),
            UnreadableManifestRule::Refuse,
        )
        .expect_err("the canonical path is not UTF-8");
        assert!(
            matches!(err, MacroOptionsError::ExpandCachePathNotUtf8 { .. }),
            "expected ExpandCachePathNotUtf8, got {err:?}"
        );
    }

    /// U2u, the accepted side of the predicate (invariant I10): a directory
    /// whose name is valid UTF-8 but not ASCII is accepted, and the resolved
    /// directory is the canonical path.
    #[test]
    fn a_non_ascii_utf8_expand_cache_directory_is_accepted() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = cache_dir(tmp.path(), "expand-cach\u{e9}-\u{3a9}");
        assert!(
            !dir.to_str().expect("UTF-8").is_ascii(),
            "instrument: the name is not ASCII"
        );
        let resolved = resolve_macro_options(
            tmp.path(),
            &MacroOptionsRequest::from_flags(&[], Some(&dir), false),
            UnreadableManifestRule::Refuse,
        )
        .expect("a non-ASCII UTF-8 directory is accepted");
        assert_eq!(resolved.source, MacroOptionsSource::Explicit);
        assert_eq!(
            resolved.options.expand_cache_dir.as_deref(),
            Some(dir.as_path())
        );
    }

    /// T9: the unreadable-manifest rule follows the roster's policy.
    #[test]
    fn unreadable_manifest_follows_the_rule() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let storage = GraphStorage::new(tmp.path());
        std::fs::create_dir_all(storage.graph_dir()).expect("graph dir");
        std::fs::write(storage.manifest_path(), b"{").expect("unparseable manifest");

        let err = resolve_macro_options(
            tmp.path(),
            &MacroOptionsRequest::empty(),
            UnreadableManifestRule::Refuse,
        )
        .expect_err("refuse");
        match &err {
            MacroOptionsError::ManifestUnreadable { manifest_path, .. } => {
                assert_eq!(manifest_path, storage.manifest_path());
            }
            other => panic!("expected ManifestUnreadable, got {other:?}"),
        }

        let resolved = resolve_macro_options(
            tmp.path(),
            &MacroOptionsRequest::empty(),
            UnreadableManifestRule::TreatAsNoRecord,
        )
        .expect("fallback");
        assert_eq!(resolved.source, MacroOptionsSource::None);
    }

    #[test]
    fn from_flags_treats_an_empty_cfg_list_as_absent() {
        let request = MacroOptionsRequest::from_flags(&[], None, false);
        assert!(request.is_empty());
        assert_eq!(request, MacroOptionsRequest::empty());
        let request = MacroOptionsRequest::from_flags(&["a".to_string()], None, false);
        assert_eq!(request.cfg_flags, Some(vec!["a".to_string()]));
        assert!(!request.is_empty());
    }

    #[test]
    fn source_wire_spellings_are_stable() {
        assert_eq!(MacroOptionsSource::Explicit.as_str(), "explicit");
        assert_eq!(MacroOptionsSource::Recorded.as_str(), "recorded");
        assert_eq!(MacroOptionsSource::None.as_str(), "none");
    }

    fn request_for(dir: &str) -> MacroOptionsRequest {
        MacroOptionsRequest {
            cfg_flags: None,
            expand_cache_dir: Some(PathBuf::from(dir)),
            reset: false,
        }
    }

    fn resolved_dir(root: &Path, request: &MacroOptionsRequest) -> PathBuf {
        resolve_macro_options(root, request, UnreadableManifestRule::Refuse)
            .expect("resolves")
            .options
            .expand_cache_dir
            .expect("an expand cache directory")
    }

    /// An empty or blank cfg flag names no predicate and is refused by the
    /// request check, alone or beside a real flag; a real flag and an
    /// explicit empty list (which clears the record) are accepted.
    #[test]
    fn validate_refuses_an_empty_or_blank_cfg_flag() {
        for flags in [
            vec![String::new()],
            vec!["  ".to_string()],
            vec!["test".to_string(), String::new()],
        ] {
            let request = MacroOptionsRequest {
                cfg_flags: Some(flags.clone()),
                ..MacroOptionsRequest::empty()
            };
            assert_eq!(
                request.validate(),
                Err(MacroRequestError::CfgFlagEmpty),
                "{flags:?}"
            );
        }
        let rendered = MacroRequestError::CfgFlagEmpty.to_string();
        assert!(
            rendered.contains("cfg flag is empty") && rendered.contains("cfg_flags"),
            "{rendered}"
        );
        for flags in [
            vec!["test".to_string()],
            vec![],
            vec!["feature=serde".to_string()],
        ] {
            let request = MacroOptionsRequest {
                cfg_flags: Some(flags.clone()),
                ..MacroOptionsRequest::empty()
            };
            assert_eq!(request.validate(), Ok(()), "{flags:?}");
        }
        assert_eq!(MacroOptionsRequest::empty().validate(), Ok(()));
    }

    /// A cfg flag with leading or trailing whitespace is refused by name
    /// (round 7), on its own and beside a good flag; the trimmed flag, a
    /// flag with inner whitespace (`feature = "x"` style spacing is the
    /// user's), and a blank flag's own refusal are the other side.
    #[test]
    fn validate_refuses_a_cfg_flag_with_surrounding_whitespace() {
        for (flags, padded) in [
            (vec![" test".to_string()], " test"),
            (vec!["test ".to_string()], "test "),
            (vec!["\ttest".to_string()], "\ttest"),
            (vec!["unix".to_string(), "test\n".to_string()], "test\n"),
        ] {
            let request = MacroOptionsRequest {
                cfg_flags: Some(flags.clone()),
                ..MacroOptionsRequest::empty()
            };
            assert_eq!(
                request.validate(),
                Err(MacroRequestError::CfgFlagPadded {
                    flag: padded.to_string()
                }),
                "{flags:?}"
            );
            assert_eq!(
                check_cfg_flag(padded),
                Err(MacroRequestError::CfgFlagPadded {
                    flag: padded.to_string()
                })
            );
        }
        let rendered = MacroRequestError::CfgFlagPadded {
            flag: " test".to_string(),
        }
        .to_string();
        assert!(
            rendered.contains("\" test\"") && rendered.contains("leading or trailing whitespace"),
            "{rendered}"
        );
        for flag in ["test", "feature=serde", "feature = \"x\""] {
            assert_eq!(check_cfg_flag(flag), Ok(()), "{flag:?}");
        }
        for flag in ["", "  ", "\t"] {
            assert_eq!(
                check_cfg_flag(flag),
                Err(MacroRequestError::CfgFlagEmpty),
                "{flag:?}"
            );
        }
    }

    /// The anchoring decision on every platform (S9, round 7): a relative
    /// path with a drive prefix or a root cannot be anchored; an absolute
    /// path, and a relative one with neither, can. Only Windows has the
    /// refused shapes, so the decision is tested on the facts; the `cfg
    /// (windows)` test below drives the same refusal with real paths.
    #[test]
    fn the_anchoring_decision_refuses_a_relative_path_with_a_prefix_or_a_root() {
        let shape = |relative, starts_at_prefix_or_root| PathShape {
            relative,
            starts_at_prefix_or_root,
        };
        assert!(shape(true, true).is_unanchorable(), r"C:cache, \cache");
        assert!(!shape(true, false).is_unanchorable(), "cache");
        assert!(!shape(false, true).is_unanchorable(), r"/cache, C:\cache");

        // The facts as read from paths every platform has: a relative path
        // starts at a name, `.` or `..`; an absolute one at a root (Unix) or
        // a prefix (Windows), which only the first component shows.
        assert_eq!(PathShape::of(Path::new("cache")), shape(true, false));
        assert_eq!(PathShape::of(Path::new("../cache")), shape(true, false));
        assert_eq!(PathShape::of(Path::new("./cache/sub")), shape(true, false));
        assert_eq!(PathShape::of(Path::new("")), shape(true, false));
        let absolute = std::env::current_dir().expect("cwd").join("cache");
        assert_eq!(PathShape::of(&absolute), shape(false, true), "{absolute:?}");
        assert!(!is_unanchorable_relative(Path::new("cache")));
        assert!(!is_unanchorable_relative(&absolute));
    }

    /// Windows (S9, round 7): the facts as read from the shapes only Windows
    /// has. `C:cache` starts at a prefix and `\cache` at a root, and both
    /// are relative; `C:\cache` and a UNC path start at a prefix and are
    /// absolute. Reading the prefix from any component but the first, or
    /// computing "relative" from the root alone, reads `\cache` wrong.
    #[cfg(windows)]
    #[test]
    fn path_shapes_are_read_from_windows_paths() {
        let shape = |relative, starts_at_prefix_or_root| PathShape {
            relative,
            starts_at_prefix_or_root,
        };
        assert_eq!(PathShape::of(Path::new("C:cache")), shape(true, true));
        assert_eq!(PathShape::of(Path::new(r"\cache")), shape(true, true));
        assert_eq!(PathShape::of(Path::new(r"\cache\sub")), shape(true, true));
        assert_eq!(PathShape::of(Path::new(r"C:\cache")), shape(false, true));
        assert_eq!(
            PathShape::of(Path::new(r"\\server\share\cache")),
            shape(false, true)
        );
        assert_eq!(PathShape::of(Path::new(r"cache\sub")), shape(true, false));
        for unanchorable in ["C:cache", r"\cache"] {
            assert!(
                is_unanchorable_relative(Path::new(unanchorable)),
                "{unanchorable}"
            );
        }
        for anchorable in [r"C:\cache", "cache", r"..\cache"] {
            assert!(
                !is_unanchorable_relative(Path::new(anchorable)),
                "{anchorable}"
            );
        }
    }

    /// The reason names the shape of the directory it was given, and carries
    /// no remedy (each surface appends its own); the error's message is the
    /// reason plus the remedy naming both spellings.
    #[test]
    fn the_missing_reason_is_chosen_by_shape() {
        assert_eq!(
            expand_cache_missing_reason(Path::new("")),
            "the expand cache directory is empty, so it names no directory"
        );
        let relative = expand_cache_missing_reason(Path::new("cache"));
        assert!(
            relative.contains("cache is relative")
                && relative.contains("could not be made absolute"),
            "{relative}"
        );
        let absolute = std::env::current_dir().expect("cwd").join("no-such-cache");
        assert_eq!(
            expand_cache_missing_reason(&absolute),
            format!(
                "expand cache directory {} does not exist or is not a directory",
                absolute.display()
            )
        );
        for dir in [Path::new(""), Path::new("cache"), absolute.as_path()] {
            let reason = expand_cache_missing_reason(dir);
            assert!(!reason.contains("--no-macro-options"), "{reason}");
            let message = MacroOptionsError::ExpandCacheMissing {
                dir: dir.to_path_buf(),
            }
            .to_string();
            assert!(
                message.starts_with(&reason) && message.contains("--no-macro-options"),
                "{message}"
            );
        }
    }

    /// The request check refuses the empty expand cache directory with the
    /// resolver's own text, and accepts an ordinary relative directory,
    /// `.` and `..`.
    #[test]
    fn validate_refuses_an_empty_expand_cache_and_accepts_ordinary_ones() {
        assert_eq!(
            request_for("").validate(),
            Err(MacroRequestError::ExpandCacheEmpty)
        );
        assert_eq!(
            MacroRequestError::ExpandCacheEmpty.to_string(),
            MacroOptionsError::ExpandCacheEmpty.to_string(),
            "both refusals of the empty directory read the same"
        );
        for dir in ["cache", "./cache", "cache/", ".", "..", "a/../b"] {
            assert_eq!(request_for(dir).validate(), Ok(()), "{dir}");
        }
    }

    /// `try_from_flags` checks the flags as given and then makes the
    /// directory absolute exactly as `from_flags` does.
    #[test]
    fn try_from_flags_checks_then_makes_absolute() {
        assert_eq!(
            MacroOptionsRequest::try_from_flags(&[String::new()], None, false),
            Err(MacroRequestError::CfgFlagEmpty)
        );
        assert_eq!(
            MacroOptionsRequest::try_from_flags(&[], Some(Path::new("")), false),
            Err(MacroRequestError::ExpandCacheEmpty)
        );
        let accepted = MacroOptionsRequest::try_from_flags(
            &["test".to_string()],
            Some(Path::new("rel/cache")),
            true,
        )
        .expect("ordinary flags are accepted");
        assert_eq!(
            accepted,
            MacroOptionsRequest::from_flags(
                &["test".to_string()],
                Some(Path::new("rel/cache")),
                true
            )
        );
    }

    /// Windows: a relative directory with a drive prefix (`C:cache`) or a
    /// root (`\cache`) names no directory under the workspace root. The
    /// request check refuses it by name, and the resolver refuses it unread
    /// (from the request and from a hand-edited record) instead of letting
    /// `root.join` replace the root with it.
    #[cfg(windows)]
    #[test]
    fn a_relative_expand_cache_with_a_prefix_or_root_is_refused() {
        let root = tempfile::tempdir().expect("root");
        for dir in ["C:cache", r"\cache"] {
            let request = request_for(dir);
            match request.validate() {
                Err(MacroRequestError::ExpandCacheNotAnchorable { dir: named }) => {
                    assert_eq!(named, PathBuf::from(dir));
                }
                other => panic!("{dir}: expected ExpandCacheNotAnchorable, got {other:?}"),
            }
            let rendered = request.validate().unwrap_err().to_string();
            assert!(
                rendered.contains(dir) && rendered.contains("drive prefix or a root"),
                "{rendered}"
            );
            let err = resolve_macro_options(root.path(), &request, UnreadableManifestRule::Refuse)
                .expect_err("the resolver never resolves it through the working directory");
            match &err {
                MacroOptionsError::ExpandCacheMissing { dir: named } => {
                    assert_eq!(named, &PathBuf::from(dir), "left unanchored");
                }
                other => panic!("{dir}: expected ExpandCacheMissing, got {other:?}"),
            }
            assert!(err.to_string().contains("drive prefix or a root"), "{err}");

            write_manifest(
                root.path(),
                Some(MacroOptionsManifest {
                    cfg_flags: vec![],
                    expand_cache_dir: Some(dir.to_string()),
                }),
            );
            let err = resolve_macro_options(
                root.path(),
                &MacroOptionsRequest::empty(),
                UnreadableManifestRule::Refuse,
            )
            .expect_err("a recorded prefixed directory is refused too");
            assert!(
                matches!(&err, MacroOptionsError::ExpandCacheMissing { dir: named } if named == &PathBuf::from(dir)),
                "{err:?}"
            );
        }
        // The accepted side: an absolute directory with a drive prefix.
        let cache = cache_dir(root.path(), "abs-cache");
        assert_eq!(request_for(&cache.display().to_string()).validate(), Ok(()));
    }

    /// A relative directory recorded in the manifest (only a hand edit
    /// writes one) resolves against the workspace root, as a request's
    /// does. `src` exists in this test's working directory (the crate root)
    /// and not under the workspace root, so a resolver that asked the
    /// process working directory would accept it.
    #[test]
    fn a_relative_recorded_expand_cache_resolves_against_the_workspace_root() {
        assert!(Path::new("src").is_dir(), "precondition: cwd holds src");
        let root = tempfile::tempdir().expect("root");
        let name = "w4-recorded-relative-cache";
        std::fs::create_dir(root.path().join(name)).expect("cache dir");
        write_manifest(
            root.path(),
            Some(MacroOptionsManifest {
                cfg_flags: vec![],
                expand_cache_dir: Some(name.to_string()),
            }),
        );
        let resolved = resolve_macro_options(
            root.path(),
            &MacroOptionsRequest::empty(),
            UnreadableManifestRule::Refuse,
        )
        .expect("a relative record under the root resolves");
        assert_eq!(resolved.source, MacroOptionsSource::Recorded);
        assert_eq!(
            resolved.options.expand_cache_dir,
            Some(root.path().join(name).canonicalize().expect("canonical"))
        );

        write_manifest(
            root.path(),
            Some(MacroOptionsManifest {
                cfg_flags: vec![],
                expand_cache_dir: Some("src".to_string()),
            }),
        );
        let err = resolve_macro_options(
            root.path(),
            &MacroOptionsRequest::empty(),
            UnreadableManifestRule::Refuse,
        )
        .expect_err("src is not under the workspace root");
        assert!(
            matches!(&err, MacroOptionsError::ExpandCacheMissing { dir } if dir == &canonical(root.path()).join("src")),
            "refused as missing under the root, got {err:?}"
        );
    }

    /// A recorded empty directory is refused as empty, never joined to the
    /// root (which would record the root itself as the cache).
    #[test]
    fn an_empty_recorded_expand_cache_is_refused_not_rooted() {
        let root = tempfile::tempdir().expect("root");
        write_manifest(
            root.path(),
            Some(MacroOptionsManifest {
                cfg_flags: vec![],
                expand_cache_dir: Some(String::new()),
            }),
        );
        let err = resolve_macro_options(
            root.path(),
            &MacroOptionsRequest::empty(),
            UnreadableManifestRule::Refuse,
        )
        .expect_err("an empty record names no directory");
        match &err {
            MacroOptionsError::ExpandCacheMissing { dir } => {
                assert!(dir.as_os_str().is_empty(), "left empty: {dir:?}");
            }
            other => panic!("expected ExpandCacheMissing, got {other:?}"),
        }
        let rendered = err.to_string();
        assert!(rendered.contains("is empty"), "{rendered}");
    }

    /// A file is not reported as missing: the refusal says the path does not
    /// exist or is not a directory, and names both ways out.
    #[test]
    fn a_file_is_refused_as_not_a_directory() {
        let root = tempfile::tempdir().expect("root");
        let file = root.path().join("cache-file");
        std::fs::write(&file, b"not a directory").expect("file");
        let err = resolve_macro_options(
            root.path(),
            &request_for("cache-file"),
            UnreadableManifestRule::Refuse,
        )
        .expect_err("a file is no expand cache");
        assert!(
            matches!(&err, MacroOptionsError::ExpandCacheMissing { dir } if dir == &file),
            "{err:?}"
        );
        let rendered = err.to_string();
        assert!(
            rendered.contains("does not exist or is not a directory")
                && rendered.contains("expand_cache field")
                && rendered.contains("reset_macro_options"),
            "{rendered}"
        );
    }

    /// Any existing directory is an expand cache, the workspace root
    /// included: `.` records the root's canonical path and `..` its
    /// parent's. Every ordinary spelling (`./x`, `x/`, `x/../x`) records
    /// the one canonical path, so the resolver's canonicalisation is
    /// pinned for inputs a user types.
    #[test]
    fn ordinary_relative_spellings_record_the_canonical_directory() {
        let outer = tempfile::tempdir().expect("outer");
        let root = outer.path().join("ws");
        std::fs::create_dir_all(root.join("x")).expect("cache dir");
        let canonical_root = root.canonicalize().expect("canonical root");
        let canonical_x = canonical_root.join("x");
        for (input, expected) in [
            (".", canonical_root.clone()),
            ("..", outer.path().canonicalize().expect("canonical outer")),
            ("./x", canonical_x.clone()),
            ("x/", canonical_x.clone()),
            ("x/../x", canonical_x.clone()),
        ] {
            // Compared as bytes: `Path` equality skips `.` components and a
            // trailing separator, and the manifest records the text.
            assert_eq!(
                resolved_dir(&root, &request_for(input)).as_os_str(),
                expected.as_os_str(),
                "{input}"
            );
        }
        // An absolute spelling with a `.` component records the same path.
        assert_eq!(
            resolved_dir(
                &root,
                &request_for(&root.join(".").join("x").display().to_string())
            )
            .as_os_str(),
            canonical_x.as_os_str()
        );
    }

    /// A UTF-8 symlink to a directory records the directory it names.
    #[cfg(unix)]
    #[test]
    fn a_utf8_symlink_records_its_target() {
        let root = tempfile::tempdir().expect("root");
        let target = cache_dir(root.path(), "real-cache");
        std::os::unix::fs::symlink(&target, root.path().join("link-cache")).expect("symlink");
        assert_eq!(
            resolved_dir(root.path(), &request_for("link-cache")).as_os_str(),
            target.as_os_str()
        );
    }

    /// `describe` quotes a blank cfg flag, so a record holding `[""]` is not
    /// printed as an empty list.
    #[test]
    fn describe_quotes_a_blank_cfg_flag() {
        let resolved = ResolvedMacroOptions {
            options: MacroBuildOptions {
                cfg_flags: vec![String::new(), "test".to_string(), " unix".to_string()],
                expand_cache_dir: None,
            },
            source: MacroOptionsSource::Recorded,
        };
        assert_eq!(
            resolved.describe(),
            "cfg [\"\", test, \" unix\"] (recorded in the manifest)"
        );
    }

    fn canonical(path: &Path) -> PathBuf {
        path.canonicalize().expect("canonical")
    }

    /// `target` spelled relative to this process's working directory (the
    /// crate root under `cargo test`), with as many `..` as it takes. The
    /// tests below pass such a path as the root, the way the CLI passes `.`
    /// or `outer/ws`, without changing the process's working directory,
    /// which every other test shares.
    fn relative_to_cwd(target: &Path) -> PathBuf {
        let cwd = canonical(&std::env::current_dir().expect("cwd"));
        let target = canonical(target);
        let cwd_parts: Vec<Component<'_>> = cwd.components().collect();
        let target_parts: Vec<Component<'_>> = target.components().collect();
        let common = cwd_parts
            .iter()
            .zip(&target_parts)
            .take_while(|(a, b)| a == b)
            .count();
        let mut relative = PathBuf::new();
        for _ in common..cwd_parts.len() {
            relative.push("..");
        }
        for part in &target_parts[common..] {
            relative.push(part.as_os_str());
        }
        assert!(relative.is_relative(), "{}", relative.display());
        relative
    }

    /// S1 (round 7): a relative root anchors a relative directory, recorded
    /// or requested, at the root's absolute canonical path. Joined to the
    /// relative root itself the directory stayed relative and was refused
    /// unread as missing, which broke `sqry update` from the workspace (the
    /// CLI's root is `.`). The control from the other side: a name that is
    /// not under the root is refused, naming the absolute directory.
    #[test]
    fn a_relative_root_anchors_a_relative_directory_absolutely() {
        let root = tempfile::tempdir().expect("root");
        let name = "w4-relative-root-cache";
        std::fs::create_dir(root.path().join(name)).expect("cache dir");
        let relative_root = relative_to_cwd(root.path());
        let expected = canonical(&root.path().join(name));

        write_manifest(
            root.path(),
            Some(MacroOptionsManifest {
                cfg_flags: vec![],
                expand_cache_dir: Some(name.to_string()),
            }),
        );
        let resolved = resolve_macro_options(
            &relative_root,
            &MacroOptionsRequest::empty(),
            UnreadableManifestRule::Refuse,
        )
        .expect("a relative record under a relative root resolves");
        assert_eq!(resolved.source, MacroOptionsSource::Recorded);
        assert_eq!(resolved.options.expand_cache_dir.as_ref(), Some(&expected));

        let resolved = resolve_macro_options(
            &relative_root,
            &request_for(name),
            UnreadableManifestRule::Refuse,
        )
        .expect("a relative request under a relative root resolves");
        assert_eq!(resolved.options.expand_cache_dir.as_ref(), Some(&expected));

        let err = resolve_macro_options(
            &relative_root,
            &request_for("no-such-cache"),
            UnreadableManifestRule::Refuse,
        )
        .expect_err("a name not under the root is refused");
        match &err {
            MacroOptionsError::ExpandCacheMissing { dir } => {
                assert!(dir.is_absolute(), "{}", dir.display());
                assert_eq!(dir, &canonical(root.path()).join("no-such-cache"));
            }
            other => panic!("expected ExpandCacheMissing, got {other:?}"),
        }
    }

    /// A relative root that does not exist is made absolute against this
    /// process's working directory, where it was given, so a relative
    /// directory under it is refused by its absolute name as missing. The
    /// CLI refuses such a root before it gets here, but the resolver is a
    /// library entry point; without the fallback the root stayed relative,
    /// the directory was refused as one that could not be anchored, and the
    /// message named no directory a caller could look for.
    #[test]
    fn a_missing_relative_root_is_made_absolute_against_the_working_directory() {
        let root = Path::new("r7s3-no-such-root");
        assert!(!root.exists(), "precondition: the root does not exist");
        let expected = std::env::current_dir()
            .expect("cwd")
            .join(root)
            .join("cache");
        let err =
            resolve_macro_options(root, &request_for("cache"), UnreadableManifestRule::Refuse)
                .expect_err("a directory under a missing root is missing");
        match &err {
            MacroOptionsError::ExpandCacheMissing { dir } => {
                assert_eq!(dir, &expected, "anchored at the absolute root");
            }
            other => panic!("expected ExpandCacheMissing, got {other:?}"),
        }
        let rendered = err.to_string();
        assert!(
            rendered.contains("does not exist or is not a directory")
                && !rendered.contains("could not be made absolute"),
            "{rendered}"
        );
    }

    /// S1: a nested relative root (`outer/ws`, `../ws`) anchors the same way.
    #[test]
    fn a_nested_relative_root_anchors_a_relative_directory_absolutely() {
        let parent = tempfile::tempdir().expect("parent");
        let ws = parent.path().join("outer").join("ws");
        std::fs::create_dir_all(ws.join("nested-cache")).expect("cache dir");
        let expected = canonical(&ws.join("nested-cache"));
        let nested = relative_to_cwd(&ws);
        let via_parent = relative_to_cwd(parent.path())
            .join("outer")
            .join("ws")
            .join("..")
            .join("ws");
        for root in [&nested, &via_parent] {
            let resolved = resolve_macro_options(
                root,
                &request_for("nested-cache"),
                UnreadableManifestRule::Refuse,
            )
            .unwrap_or_else(|err| panic!("{}: {err}", root.display()));
            assert_eq!(
                resolved.options.expand_cache_dir.as_ref(),
                Some(&expected),
                "{}",
                root.display()
            );
        }
    }

    /// S1: a root given through a symlink anchors in its target, absolute or
    /// relative, recorded or requested; the record is the canonical
    /// directory either way.
    #[cfg(unix)]
    #[test]
    fn a_symlinked_root_anchors_a_relative_directory_in_its_target() {
        let parent = tempfile::tempdir().expect("parent");
        let ws = parent.path().join("ws");
        std::fs::create_dir_all(ws.join("link-root-cache")).expect("cache dir");
        let link = parent.path().join("link");
        std::os::unix::fs::symlink(&ws, &link).expect("symlink");
        let expected = canonical(&ws.join("link-root-cache"));
        write_manifest(
            &ws,
            Some(MacroOptionsManifest {
                cfg_flags: vec![],
                expand_cache_dir: Some("link-root-cache".to_string()),
            }),
        );
        for root in [link.clone(), relative_to_cwd(parent.path()).join("link")] {
            let resolved = resolve_macro_options(
                &root,
                &MacroOptionsRequest::empty(),
                UnreadableManifestRule::Refuse,
            )
            .unwrap_or_else(|err| panic!("{}: {err}", root.display()));
            assert_eq!(resolved.options.expand_cache_dir.as_ref(), Some(&expected));
            let resolved = resolve_macro_options(
                &root,
                &request_for("link-root-cache"),
                UnreadableManifestRule::Refuse,
            )
            .unwrap_or_else(|err| panic!("{}: {err}", root.display()));
            assert_eq!(resolved.options.expand_cache_dir.as_ref(), Some(&expected));
        }
    }
}

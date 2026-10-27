//! The archive window: one window with an Export and an Import mode (like Settings), over the
//! pure modules that build (`archive_build`), seal and open (`fbterm_secrets::archive`) and
//! plan and apply (`archive_import`) profile archives.
//!
//! Only the user opens it (the palette, the profile editor's buttons, a bound chord); nothing a
//! program prints can. Passwords are typed into secure fields and kept in wiped strings,
//! cleared once used and when the window closes. Sealing and opening (scrypt: about half a
//! second and up to 512 MiB) run on tokio's blocking pool while the window shows it is busy;
//! a result for a job the window no longer waits for is dropped.
//!
//! Everything here is plain data and functions, testable without a GUI; `glue.rs` holds the
//! app side (dialogs, background work, writes) and `view.rs` the look.

use std::collections::{BTreeMap, HashSet};
use std::fs::OpenOptions;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use fbterm_config::schemes::SchemeSet;
use fbterm_config::{Profile, ProfileId, ProfileKind, ProfileStore};
use fbterm_secrets::SecretStore;
use fbterm_secrets::archive::{self, Archive, ArchiveKind, MAX_ARCHIVE_BYTES};
use zeroize::Zeroizing;

use crate::app::Delivered;
use crate::archive_import::{self, Choice, ImportPlan, NewKeyFile, SchemeOutcome};
use crate::redact::Redacted;

mod glue;
#[cfg(test)]
mod tests;
pub mod view;

pub const KIND_FULL: &str = "Full (for your own machines)";
pub const KIND_SHARED: &str = "Shared (no secrets, for teammates)";
pub const INCLUDE_KEYS: &str = "Include private key files";
pub const INCLUDED_FOR_JUMPS: &str = "Included for jump hosts";
pub const NOTHING_SELECTED: &str = "Choose at least one profile to export.";
pub const NO_PASSWORD: &str = "Type a password for the archive.";
pub const MISMATCH: &str = "The two passwords do not match.";
pub const NO_FILE: &str = "Choose an archive file first.";
pub const NO_IMPORT_PASSWORD: &str = "Type the archive's password.";
pub const READ_ONLY: &str =
    "profiles.toml could not be read, so nothing can be imported until it is fixed.";
pub const EDITOR_DIRTY: &str =
    "The profile editor has unsaved changes. Save or discard them before importing.";
pub const EDITOR_SAVING: &str =
    "The profile editor is still saving. Wait for it to finish before importing.";
pub const NOTHING_TO_IMPORT: &str = "Nothing to import: every profile is skipped or blocked.";
pub const SCHEMES_STALE: &str =
    "The color schemes changed since the archive was opened. Open it again to import.";
pub const SEALING: &str = "Encrypting the archive\u{2026}";
pub const OPENING: &str = "Opening the archive\u{2026}";
pub const STORING: &str = "Storing saved secrets\u{2026}";
pub const CHECKING: &str = "Checking where saved secrets are kept\u{2026}";
/// What an unmatched profile's row says in the match column.
pub const NEW: &str = "New";

/// The archive file's extension.
pub const EXTENSION: &str = "fbprofiles";
/// The file dialogs' filter label.
pub const FILTER_LABEL: &str = "fbterm profiles (.fbprofiles)";

/// Which half of the window shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArchiveMode {
    Export,
    Import,
}

/// A choice in a preview row's segmented control. A profile with a local match offers Skip,
/// Overwrite and Keep both; a new one offers Import and Skip (adding it is encoded as
/// [`Choice::KeepBoth`], which for a profile without a match keeps its own id and name).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowChoice {
    Import,
    Skip,
    Overwrite,
    KeepBoth,
}

impl RowChoice {
    pub fn label(self) -> &'static str {
        match self {
            Self::Import => "Import",
            Self::Skip => "Skip",
            Self::Overwrite => "Overwrite",
            Self::KeepBoth => "Keep both",
        }
    }

    fn choice(self) -> Choice {
        match self {
            Self::Import | Self::KeepBoth => Choice::KeepBoth,
            Self::Skip => Choice::Skip,
            Self::Overwrite => Choice::Overwrite,
        }
    }
}

/// What the archive window asks for.
#[derive(Debug, Clone)]
pub enum ArchiveMsg {
    Close,
    Mode(ArchiveMode),

    /// A profile's checkbox in the export list.
    Toggle(ProfileId, bool),
    Kind(ArchiveKind),
    IncludeKeys(bool),
    Password(Redacted),
    Confirm(Redacted),
    /// `Export…`: check the form, then the save dialog.
    Export,
    /// The save dialog closed (its job number, the chosen path).
    SaveChosen(u64, Option<PathBuf>),
    /// Sealing and writing finished.
    Exported(u64, Result<ExportSummary, String>),

    /// `Choose file…`.
    ChooseFile,
    FileChosen(u64, Option<PathBuf>),
    ImportPassword(Redacted),
    /// Decrypt the chosen file with the typed password.
    Open,
    Opened(u64, Delivered<Result<Archive, String>>),
    /// A preview row's choice.
    Choice(usize, RowChoice),
    /// `Choose folder…` for the key files.
    ChooseKeyFolder,
    KeyFolderChosen(u64, Option<PathBuf>),
    /// `Import`: apply the preview.
    Import,
    /// The imported saved secrets were stored (how many), or why not.
    SecretsStored(u64, Result<usize, String>),
    /// Automatic storage made its check before an import (see `App::import_opened`).
    Probed(u64),
}

/// What the window is waiting for in the background.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Busy {
    Sealing,
    Opening,
    /// Storing the imported saved secrets.
    Storing,
    /// Automatic storage checks the system storage before an import's secrets.
    Checking,
}

/// A line under the form: the outcome of the last export or import.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notice {
    pub error: bool,
    pub text: String,
}

impl Notice {
    pub fn done(text: impl Into<String>) -> Self {
        Self {
            error: false,
            text: text.into(),
        }
    }
    pub fn error(text: impl Into<String>) -> Self {
        Self {
            error: true,
            text: text.into(),
        }
    }
}

/// The export half. `Debug` never shows the passwords.
pub struct ExportForm {
    /// Profiles the user unchecked (every profile starts checked, new ones too).
    pub unchecked: HashSet<ProfileId>,
    pub kind: ArchiveKind,
    pub include_keys: bool,
    pub password: Zeroizing<String>,
    pub confirm: Zeroizing<String>,
    /// What building the archive now would leave out (warnings), or why it cannot be built.
    pub preview: Result<Vec<String>, String>,
    /// Why `Export…` did nothing (an empty selection, the passwords).
    pub problem: Option<String>,
    pub notice: Option<Notice>,
}

/// An opened archive and its preview.
pub struct Opened {
    pub archive: Archive,
    pub plan: ImportPlan,
}

impl std::fmt::Debug for Opened {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Opened")
            .field("entries", &self.plan.entries.len())
            .finish()
    }
}

/// The import half. `Debug` never shows the password.
pub struct ImportForm {
    pub file: Option<PathBuf>,
    pub password: Zeroizing<String>,
    pub opened: Option<Opened>,
    /// Where key files go; `None`: `~/.ssh`.
    pub key_dir: Option<PathBuf>,
    /// Why the last step failed (open, plan, apply).
    pub problem: Option<String>,
    pub notice: Option<Notice>,
    /// An import whose files are written, waiting for its saved secrets to be stored: what
    /// it wrote, and whether color schemes were among it.
    pub storing: Option<(ImportSummary, bool)>,
}

impl std::fmt::Debug for ExportForm {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExportForm")
            .field("unchecked", &self.unchecked)
            .field("kind", &self.kind)
            .field("include_keys", &self.include_keys)
            .field("password", &Redacted::default())
            .field("confirm", &Redacted::default())
            .field("preview", &self.preview)
            .field("problem", &self.problem)
            .field("notice", &self.notice)
            .finish()
    }
}

impl std::fmt::Debug for ImportForm {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ImportForm")
            .field("file", &self.file)
            .field("password", &Redacted::default())
            .field("opened", &self.opened)
            .field("key_dir", &self.key_dir)
            .field("problem", &self.problem)
            .field("notice", &self.notice)
            .finish()
    }
}

/// The open window's state.
#[derive(Debug)]
pub struct ArchiveState {
    pub mode: ArchiveMode,
    pub export: ExportForm,
    pub import: ImportForm,
    /// The background job the window waits for, with its number.
    pub busy: Option<(u64, Busy)>,
    /// The file dialog the window waits for, by number (one at a time).
    pub dialog: Option<u64>,
    pub password_input: iced::widget::Id,
    pub import_password_input: iced::widget::Id,
}

impl ArchiveState {
    pub fn new(mode: ArchiveMode) -> Self {
        Self {
            mode,
            export: ExportForm {
                unchecked: HashSet::new(),
                kind: ArchiveKind::Full,
                include_keys: false,
                password: Zeroizing::default(),
                confirm: Zeroizing::default(),
                preview: Ok(Vec::new()),
                problem: None,
                notice: None,
            },
            import: ImportForm {
                file: None,
                password: Zeroizing::default(),
                opened: None,
                key_dir: None,
                problem: None,
                notice: None,
                storing: None,
            },
            busy: None,
            dialog: None,
            password_input: iced::widget::Id::unique(),
            import_password_input: iced::widget::Id::unique(),
        }
    }

    /// The checked profiles, in the store's order.
    pub fn selected(&self, store: &ProfileStore) -> Vec<ProfileId> {
        store
            .profiles
            .iter()
            .map(|p| p.id)
            .filter(|id| !self.export.unchecked.contains(id))
            .collect()
    }

    /// Why `Export…` cannot go on, if it cannot.
    pub fn export_problem(&self, store: &ProfileStore) -> Option<String> {
        if self.selected(store).is_empty() {
            return Some(NOTHING_SELECTED.into());
        }
        if let Err(e) = &self.export.preview {
            return Some(e.clone());
        }
        if self.export.password.is_empty() {
            return Some(NO_PASSWORD.into());
        }
        if *self.export.password != *self.export.confirm {
            return Some(MISMATCH.into());
        }
        None
    }
}

/// One profile in the export list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportRow {
    pub id: ProfileId,
    pub name: String,
    /// `user@host` or the program.
    pub detail: String,
    pub checked: bool,
    /// Unchecked, but a checked profile jumps through it, so it travels anyway.
    pub via_jump: bool,
}

/// The export list: named groups alphabetically (ignoring case), ungrouped profiles last under
/// an empty name, the store's order within a group (as the profile editor lists them).
pub fn export_groups(
    store: &ProfileStore,
    unchecked: &HashSet<ProfileId>,
) -> Vec<(String, Vec<ExportRow>)> {
    let selected: Vec<ProfileId> = store
        .profiles
        .iter()
        .map(|p| p.id)
        .filter(|id| !unchecked.contains(id))
        .collect();
    let reached = crate::archive_build::closure(store, &selected).unwrap_or_default();
    let mut groups: BTreeMap<(bool, String), (String, Vec<ExportRow>)> = BTreeMap::new();
    for p in &store.profiles {
        let name = p.group.as_deref().unwrap_or_default().trim();
        let checked = !unchecked.contains(&p.id);
        groups
            .entry((name.is_empty(), name.to_lowercase()))
            .or_insert_with(|| (name.to_owned(), Vec::new()))
            .1
            .push(ExportRow {
                id: p.id,
                name: p.name.clone(),
                detail: detail(p),
                checked,
                via_jump: !checked && reached.contains(&p.id),
            });
    }
    groups.into_values().collect()
}

/// `user@host` (with `:port` when not 22) for an SSH profile, the program for a local one.
fn detail(p: &Profile) -> String {
    match &p.kind {
        ProfileKind::Ssh(s) if s.port == 22 => format!("{}@{}", s.user, s.host),
        ProfileKind::Ssh(s) => format!("{}@{}:{}", s.user, s.host, s.port),
        ProfileKind::Local(l) => l.program.display().to_string(),
    }
}

/// The save dialog's suggested name: `fbterm-profiles-2026-10-27.fbprofiles`.
pub fn default_file_name(date: chrono::NaiveDate) -> String {
    format!("fbterm-profiles-{}.{EXTENSION}", date.format("%Y-%m-%d"))
}

/// What an export wrote.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportSummary {
    pub path: PathBuf,
    pub profiles: usize,
    pub secrets: usize,
    pub keys: usize,
    pub schemes: usize,
    pub warnings: Vec<String>,
}

/// `1 profile`, `2 profiles`.
fn count(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

/// `a`, `a and b`, `a, b and c`.
fn and_list(parts: &[String]) -> String {
    match parts {
        [] => String::new(),
        [one] => one.clone(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

fn file_name(path: &Path) -> String {
    path.file_name().map_or_else(
        || path.display().to_string(),
        |n| n.to_string_lossy().into_owned(),
    )
}

/// The notice after an export: `Exported 3 profiles, 2 saved secrets and 1 key file to
/// fbterm-profiles-2026-10-27.fbprofiles.`
pub fn exported_text(s: &ExportSummary) -> String {
    let mut parts = vec![count(s.profiles, "profile", "profiles")];
    if s.secrets > 0 {
        parts.push(count(s.secrets, "saved secret", "saved secrets"));
    }
    if s.keys > 0 {
        parts.push(count(s.keys, "key file", "key files"));
    }
    if s.schemes > 0 {
        parts.push(count(s.schemes, "color scheme", "color schemes"));
    }
    format!("Exported {} to {}.", and_list(&parts), file_name(&s.path))
}

/// What one export does, handed to the blocking pool whole.
pub(crate) struct ExportJob {
    pub store: ProfileStore,
    pub selected: Vec<ProfileId>,
    pub kind: ArchiveKind,
    pub include_keys: bool,
    pub secrets: Arc<dyn SecretStore>,
    pub schemes: SchemeSet,
    pub password: Zeroizing<String>,
    pub path: PathBuf,
}

/// Build, seal and write an archive (slow: run it off the UI thread).
pub(crate) fn run_export(job: ExportJob) -> Result<ExportSummary, String> {
    let (archive, warnings) = crate::archive_build::build(
        &job.store,
        &job.selected,
        job.kind,
        job.include_keys,
        job.secrets.as_ref(),
        &job.schemes,
    )?;
    let sealed = archive::seal(&archive, &job.password).map_err(|e| e.to_string())?;
    write_private(&job.path, &sealed)
        .map_err(|e| format!("Could not write {}: {e}", file_name(&job.path)))?;
    Ok(ExportSummary {
        path: job.path,
        profiles: archive.profiles.len(),
        secrets: archive.secrets.len(),
        keys: archive.keys.len(),
        schemes: archive.schemes.len(),
        warnings,
    })
}

/// Write `bytes` to `path` through a temporary file next to it (readable by the user only on
/// Unix), renamed over `path` at the end: a failed write never leaves half a file there.
fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let part = path.with_file_name(format!(".{}.fbterm-part", file_name(path)));
    let _ = std::fs::remove_file(&part);
    let result = (|| {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&part)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        std::fs::rename(&part, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&part);
    }
    result
}

/// Read and decrypt an archive file (slow: run it off the UI thread).
pub(crate) fn run_open(path: &Path, password: &str) -> Result<Archive, String> {
    let unreadable = |e: std::io::Error| format!("Could not read {}: {e}", file_name(path));
    let file = std::fs::File::open(path).map_err(unreadable)?;
    let mut bytes = Vec::new();
    // One byte past the cap is enough for `open` to refuse the file as too large.
    file.take(MAX_ARCHIVE_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(unreadable)?;
    archive::open(&bytes, password).map_err(|e| e.to_string())
}

/// One archived profile in the import preview.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreviewRow {
    pub name: String,
    /// `user@host[:port]`, or the program of a local profile.
    pub host: String,
    /// The jump hops by name, `bastion > edge`; empty without jumps.
    pub jumps: String,
    pub tunnels: usize,
    /// `New`, or `Matches {name}` with the local profile it matches.
    pub matched: String,
    /// The choices the row offers, and the one made.
    pub choices: Vec<RowChoice>,
    pub choice: RowChoice,
    pub error: Option<String>,
    /// The key path the profile takes from the archive, when it keeps one (no key file in
    /// the archive takes its place and Overwrite keeps no local path).
    pub key_path: Option<String>,
}

/// The preview rows of `plan` against the local profiles.
pub fn preview_rows(plan: &ImportPlan, local: &ProfileStore) -> Vec<PreviewRow> {
    let archived_name = |id: ProfileId| {
        plan.entries
            .iter()
            .find(|e| e.incoming.id == id)
            .map(|e| e.incoming.name.clone())
            .or_else(|| local.get(id).map(|p| p.name.clone()))
            .unwrap_or_else(|| "unknown".into())
    };
    plan.entries
        .iter()
        .enumerate()
        .map(|(i, e)| {
            let (jumps, tunnels) = match &e.incoming.kind {
                ProfileKind::Ssh(s) => (
                    s.jump
                        .iter()
                        .map(|j| archived_name(*j))
                        .collect::<Vec<_>>()
                        .join(" > "),
                    s.tunnels.len(),
                ),
                ProfileKind::Local(_) => (String::new(), 0),
            };
            let matched_name = e.matched.and_then(|m| local.get(m)).map(|p| p.name.clone());
            let (choices, choice) = match matched_name {
                Some(_) => (
                    vec![RowChoice::Skip, RowChoice::Overwrite, RowChoice::KeepBoth],
                    match e.choice {
                        Choice::Skip => RowChoice::Skip,
                        Choice::Overwrite => RowChoice::Overwrite,
                        Choice::KeepBoth => RowChoice::KeepBoth,
                    },
                ),
                None => (
                    vec![RowChoice::Import, RowChoice::Skip],
                    if e.choice == Choice::Skip {
                        RowChoice::Skip
                    } else {
                        RowChoice::Import
                    },
                ),
            };
            PreviewRow {
                name: e.incoming.name.clone(),
                host: detail(&e.incoming),
                jumps,
                tunnels,
                matched: matched_name.map_or_else(|| NEW.into(), |n| format!("Matches {n}")),
                choices,
                choice,
                error: e.error.clone(),
                key_path: plan.kept_key_path(i).map(|p| p.display().to_string()),
            }
        })
        .collect()
}

/// What the archive carries, in one line: `Full archive. Includes saved secrets and private
/// key files.`
pub fn archive_summary(plan: &ImportPlan, kind: ArchiveKind) -> String {
    let mut s = match kind {
        ArchiveKind::Full => "Full archive.".to_string(),
        ArchiveKind::Shared => "Shared archive: no saved secrets or key files.".to_string(),
    };
    match (plan.has_secrets, plan.has_keys) {
        (true, true) => s.push_str(" Includes saved secrets and private key files."),
        (true, false) => s.push_str(" Includes saved secrets."),
        (false, true) => s.push_str(" Includes private key files."),
        (false, false) => {}
    }
    s
}

/// The color schemes the import adds, as lines for the preview.
pub fn scheme_lines(plan: &ImportPlan) -> Vec<String> {
    plan.schemes
        .iter()
        .filter_map(|s| match s.outcome {
            SchemeOutcome::Reused => None,
            SchemeOutcome::Imported => Some(format!("Color scheme {} is added.", s.scheme.name)),
            SchemeOutcome::Renamed => Some(format!(
                "A different color scheme has the same id here, so it is added as {}.",
                s.scheme.name
            )),
        })
        .collect()
}

/// Set row `i`'s choice.
pub fn set_row_choice(plan: &mut ImportPlan, i: usize, choice: RowChoice) {
    archive_import::set_choice(plan, i, choice.choice());
}

/// Whether any entry would be imported with the current choices.
pub fn anything_to_import(plan: &ImportPlan) -> bool {
    plan.entries
        .iter()
        .any(|e| e.choice != Choice::Skip && e.error.is_none())
}

/// What an import wrote.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ImportSummary {
    pub added: usize,
    pub replaced: usize,
    pub keys: usize,
    pub schemes: usize,
    pub secrets: usize,
    /// Secrets that could not be stored, as sentences.
    pub secret_failures: Vec<String>,
}

/// The notice after an import: `Imported 3 profiles (1 replaced), 2 key files, 1 color scheme
/// and 2 saved secrets.`
pub fn imported_text(s: &ImportSummary) -> String {
    let mut first = count(s.added + s.replaced, "profile", "profiles");
    if s.replaced > 0 {
        first.push_str(&format!(" ({} replaced)", s.replaced));
    }
    let mut parts = vec![first];
    if s.keys > 0 {
        parts.push(count(s.keys, "key file", "key files"));
    }
    if s.schemes > 0 {
        parts.push(count(s.schemes, "color scheme", "color schemes"));
    }
    if s.secrets > 0 {
        parts.push(count(s.secrets, "saved secret", "saved secrets"));
    }
    let mut text = format!("Imported {}.", and_list(&parts));
    for f in &s.secret_failures {
        text.push(' ');
        text.push_str(f);
    }
    text
}

/// What an import wrote before a later step failed, for the message saying so.
pub fn written_before(keys: usize, schemes: usize) -> Option<String> {
    let mut parts = Vec::new();
    if keys > 0 {
        parts.push(count(keys, "key file", "key files"));
    }
    if schemes > 0 {
        parts.push(count(schemes, "color scheme", "color schemes"));
    }
    (!parts.is_empty()).then(|| and_list(&parts))
}

/// `~/.ssh` under the home folder.
pub fn default_key_dir() -> Option<PathBuf> {
    directories::BaseDirs::new().map(|d| d.home_dir().join(".ssh"))
}

/// A folder as shown: under the home folder as `~/...`.
pub fn shown_dir(dir: &Path) -> String {
    if let Some(home) = directories::BaseDirs::new().map(|d| d.home_dir().to_path_buf())
        && let Ok(rest) = dir.strip_prefix(&home)
    {
        return if rest.as_os_str().is_empty() {
            "~".into()
        } else {
            format!("~/{}", rest.display())
        };
    }
    dir.display().to_string()
}

/// Make the key folder when it is missing (readable by the user only on Unix); an existing
/// folder is left as it is.
pub(crate) fn ensure_key_dir(dir: &Path) -> std::io::Result<()> {
    if dir.is_dir() {
        return Ok(());
    }
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(dir)
}

/// Create each key file, never replacing anything (a file that appeared since the preview
/// stops the import), readable and writable by the user only (mode `0600` on Unix). On a
/// failure the files this call created are removed again and the reason returned.
pub(crate) fn write_key_files(files: &[NewKeyFile]) -> Result<usize, String> {
    let mut created: Vec<&Path> = Vec::new();
    for f in files {
        if let Err(e) = create_key_file(&f.path, &f.bytes) {
            for p in &created {
                let _ = std::fs::remove_file(p);
            }
            let name = file_name(&f.path);
            return Err(if e.kind() == std::io::ErrorKind::AlreadyExists {
                format!(
                    "A file named {name} appeared in the key folder meanwhile, so nothing was imported. Open the archive again to import."
                )
            } else {
                format!("Could not write the key file {name} ({e}), so nothing was imported.")
            });
        }
        created.push(&f.path);
    }
    Ok(created.len())
}

fn create_key_file(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    let written = file.write_all(bytes).and_then(|()| file.sync_all());
    if written.is_err() {
        // Ours, and incomplete.
        let _ = std::fs::remove_file(path);
    }
    written
}

/// A sample full archive for the `FBTERM_DEMO_ARCHIVE` debug aid, made against `local`: the
/// first local SSH profile again (a clash), two new SSH profiles (one jumping through the
/// other, with tunnels and a key file) and a color scheme. Built in memory only.
pub(crate) fn demo_archive(local: &ProfileStore) -> Archive {
    use fbterm_config::profile::{AuthMethod, SshProfile, StartMode, TunnelDef, TunnelKind};
    let ssh = |name: &str, auth: AuthMethod, jump: Vec<ProfileId>| Profile {
        id: ProfileId::new(),
        name: name.into(),
        group: Some("staging".into()),
        accent: None,
        accent_style: Default::default(),
        color_scheme: None,
        kind: ProfileKind::Ssh(SshProfile {
            host: format!("{name}.example.com"),
            port: 22,
            user: "deploy".into(),
            auth,
            jump,
            tunnels: Vec::new(),
        }),
    };
    let mut out = Archive::new(ArchiveKind::Full, "2026-10-27T10:00:00Z");
    if let Some(p) = local
        .profiles
        .iter()
        .find(|p| matches!(p.kind, ProfileKind::Ssh(_)))
    {
        let mut copy = p.clone();
        copy.id = ProfileId::new();
        out.profiles.push(copy);
    }
    let gateway = ssh("staging-gateway", AuthMethod::Agent, Vec::new());
    let mut web = ssh(
        "staging-web",
        AuthMethod::Key {
            path: Some("/home/deploy/.ssh/id_staging".into()),
            passphrase: false,
        },
        vec![gateway.id],
    );
    let mut scheme = fbterm_config::schemes::builtins().remove(1);
    scheme.id = "user-harbor".into();
    scheme.name = "Harbor".into();
    scheme.builtin = false;
    web.color_scheme = Some(scheme.id.clone());
    if let ProfileKind::Ssh(s) = &mut web.kind {
        let forward = |name: &str, port: u16| TunnelDef {
            name: name.into(),
            kind: TunnelKind::Local {
                bind: "127.0.0.1".into(),
                port,
                host: "localhost".into(),
                host_port: port,
            },
            start: StartMode::OnDemand,
        };
        s.tunnels = vec![forward("app", 8080), forward("metrics", 9090)];
    }
    out.keys.push(archive::ArchivedKey {
        file_name: "id_staging".into(),
        bytes: Zeroizing::new(b"sample key bytes".to_vec()),
        profiles: vec![web.id],
    });
    out.profiles.push(web);
    out.profiles.push(gateway);
    out.schemes.push(scheme);
    out
}

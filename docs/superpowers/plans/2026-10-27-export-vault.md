# Export, Import and Secrets Vault Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Encrypted full and shared profile archives (export with optional key files, import with preview and clash resolution), and an `age`-encrypted secrets vault used automatically where the system keychain falls short or by choice.

**Architecture:** A versioned archive model and its `age` passphrase encryption live in `fbterm-secrets::archive` (new `age` dependency). Pure planning code builds archives from the profile store and plans imports (clash resolution, reference rewiring) so the risky logic is unit-tested without UI. The secrets side gets a `VaultStore` and a `SecretBackend` switcher that replaces today's session-only `FallbackStore`, behind the same `Arc<dyn SecretStore>` the pool and editor already share.

**Tech Stack:** Rust 2024, `age` (passphrase recipient; MIT OR Apache-2.0), `zeroize`, `keyring` 4.2, `rfd`, iced 0.14.

**Spec:** `docs/superpowers/specs/2026-10-27-export-vault-design.md` (approved).

**Prerequisite:** color schemes (`Profile.color_scheme`, user schemes in `<config>/schemes/`), tunnels (`SshProfile.tunnels`) and the file panel are merged into `develop`; rebase `feature/export-vault` before Task 1.

## Global Constraints

- `age` with a passphrase recipient for every encryption (archives and vault); no home-grown crypto; never store or log a plaintext secret; secret strings wrapped and zeroized where the types allow (`zeroize`, existing `Redacted`).
- A shared archive never contains a password, a key passphrase, a private key, a private key path, or a local profile, whatever the input: enforced by construction and by tests on the decrypted bytes.
- Archive file extension `.fbprofiles`; format versioned (`format = 1`); an archive with a newer format is refused with `This archive was made by a newer fbterm.`; a wrong password says `Wrong password, or the file is not an fbterm archive.`
- Full archives: profiles (local and SSH), saved passwords and passphrases, optional key files (`Include private key files`, off by default); missing or unreadable keys are left out with a warning listed in the export dialog; a key used by several profiles is stored once; key files over 1 MB are refused.
- Jump-chain profiles of selected profiles are always included; tunnels and `color_scheme` travel with profiles; user schemes used by exported profiles are included.
- Import: matches by id, then by name; choices `Skip` (default), `Overwrite`, `Keep both`; Overwrite replaces only what the archive carries (a shared import keeps local secrets and key paths); Keep both gives a new id and the name `{name} (imported)`; references inside the archive follow Keep both copies and point at existing profiles on Skip; invalid results (not SSH, jump cycle) show on that profile and block it. Key files go to a chosen folder (default `~/.ssh/`) with mode `0600`; identical existing files are reused, different same-named files get a new name; nothing on disk is replaced silently. Schemes: same id and content reused, else imported with a new id and name `{name} (imported)`.
- Key profiles without a key path connect by asking the user to pick a key file (`Choose the private key for {profile}`), remembered in the profile.
- Secret storage setting `secret_storage = "auto" | "keychain" | "vault"` (default `"auto"`). "keychain" means the system storage: macOS Keychain, Linux Secret Service, and on Windows a DPAPI-encrypted file `<config>/secrets.dpapi` (owner decision 2026-10-27; Credential Manager is not used). Auto uses the system storage where it works and switches to the vault when it is unavailable; an automatic switch writes `"vault"` and says once `The system keychain is not available, so saved secrets now use an encrypted vault.` There is no size-based switch. Switching moves every secret; a failed read of the old storage cancels the switch and removes nothing.
- Vault: `<config>/secrets.fbvault`, `age`-encrypted TOML; unlocked once per app session on first need with `Unlock saved secrets` (main password); setting it asks twice (`Set a main password`); `Reset vault` deletes saved secrets after confirmation (`Delete all saved passwords and passphrases? Profiles are kept.`) and keeps profiles; cancelling unlock makes that connection ask for its secret as if none were saved.
- Copy: palette actions `Export profiles…`, `Import profiles…`; editor footer buttons `Export…`, `Import…`; export kinds `Full (for your own machines)` and `Shared (no secrets, for teammates)`; Settings section `Saved secrets` with choices `Automatic`, `System keychain`, `Encrypted vault` and button `Reset vault`.
- Users who never use these features see no change.
- Cross-platform; no new `unsafe`; no emoji or em-dashes; never name another terminal application; Conventional Commits with no attribution of any kind; `cargo deny` passes with the new dependencies (record them in `LICENSES.md`).
- Gates per task: `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`; `cd website && npm run build` when docs change.

## Review Focus

1. A shared export of a profile whose key path, passphrase and password are all set, plus a local profile with `env = { TOKEN = "…" }`: the decrypted archive bytes contain none of them. Test in Task 3 (`shared_archive_contains_no_secrets_bytes`).
2. Importing a shared archive over the owner's own profiles with Overwrite: their saved passwords and key paths survive. Test in Task 4 (`shared_overwrite_keeps_local_secrets_and_key_path`).
3. Switching to the vault while the keychain errors on read: nothing moved, nothing deleted, a clear notice. Test in Task 7 (`switch_aborts_when_old_store_unreadable`).
4. A truncated or tampered `.fbprofiles` file, and a wrong password: clear message, nothing imported, nothing written. Test in Task 2 (`tampered_archive_is_refused`).
5. Importing key files into a folder that already has `id_ed25519` with different content: the existing file is untouched and the profile points at the new name. Test in Task 4 (`existing_key_file_is_never_replaced`).

---

### Task 1: Optional key path and asking for a key file

**Files:**
- Modify: `crates/fbterm-config/src/profile.rs` (`AuthMethod::Key { path: Option<PathBuf>, passphrase }`, serde omits `path` when None, old files with a path load), every reader (`fbterm-session/src/ssh/auth.rs`, editor `Draft::{from,to}_profile`, `refresh_key_check`, tests and fixtures), `fbterm-session/src/ssh/mod.rs` (`Prompter::ask_key_file(&self, profile_name: &str) -> Option<PathBuf>`; implementors `UiPrompter`, `ClockedPrompter`, test prompters), `crates/fbterm/src/prompts.rs` + `prompt_view.rs` (a dialog with the copy from Global Constraints, `Choose…` opening `rfd` in `~/.ssh`, `Cancel`), app side: remember the chosen path in `profiles.toml` after the key is accepted.
- [ ] **Step 1: Failing tests**: `key_without_path_round_trips`, `old_key_profiles_load`, `connect_without_key_path_asks_and_remembers` (fake prompter), `cancel_key_choice_fails_cleanly`, editor `empty_key_path_saves_as_none`.
- [ ] **Step 2: Run** — Expected: FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** the gates and live SSH tests — Expected: PASS.
- [ ] **Step 5: Commit** `feat(profiles): key profiles may have no key file yet`.

### Task 2: Archive format and encryption

**Files:**
- Create: `crates/fbterm-secrets/src/archive.rs`; Modify: workspace `Cargo.toml` (`age`, `zeroize`), `crates/fbterm-secrets/Cargo.toml`, `LICENSES.md`

**Interfaces:**
- Produces: `pub struct Archive { pub format: u32, pub kind: ArchiveKind, pub created: String, pub profiles: Vec<Profile>, pub secrets: Vec<ArchivedSecret>, pub keys: Vec<ArchivedKey>, pub schemes: Vec<Scheme> }`, `pub enum ArchiveKind { Full, Shared }`, `pub struct ArchivedSecret { pub profile: ProfileId, pub kind: SecretKind, pub value: Zeroizing<String> }`, `pub struct ArchivedKey { pub file_name: String, pub bytes: Zeroizing<Vec<u8>>, pub profiles: Vec<ProfileId> }`; `pub fn seal(archive: &Archive, password: &str) -> Result<Vec<u8>, ArchiveError>` (TOML inside `age` passphrase encryption; key bytes base64); `pub fn open(bytes: &[u8], password: &str) -> Result<Archive, ArchiveError>`; `ArchiveError::{WrongPassword, Newer, Corrupt(String), Io(String)}` with the Global Constraints copy.
- [ ] **Step 1: Failing tests**: `seal_open_round_trip`, `wrong_password_is_refused`, `newer_format_is_refused`, `tampered_archive_is_refused`, `secrets_are_zeroized_types` (compile-level), `cargo deny` passes.
- [ ] **Step 2: Run** — Expected: FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** the gates and `cargo deny --all-features check` — Expected: PASS.
- [ ] **Step 5: Commit** `feat(archive): versioned age-encrypted profile archives`.

### Task 3: Building archives

**Files:**
- Create: `crates/fbterm/src/archive_build.rs` (pure, no UI)

**Interfaces:**
- Consumes: Task 2 types; `ProfileStore`, `SecretStore`, `SchemeSet`.
- Produces: `pub(crate) fn build(store: &ProfileStore, selected: &[ProfileId], kind: ArchiveKind, include_keys: bool, secrets: &dyn SecretStore, schemes: &SchemeSet) -> Result<(Archive, Vec<String> /* warnings */), String>` with jump closure, shared stripping (SSH only; `AuthMethod::Key { path: None, passphrase: false }`; no secrets; no keys), key dedupe and size limit, scheme inclusion.
- [ ] **Step 1: Failing tests**: `jump_chain_profiles_are_included`, `shared_archive_contains_no_secrets_bytes` (seal, open with the password, search the decrypted TOML for every secret value, every key path and the local profile's env value), `shared_leaves_out_local_profiles`, `full_includes_secrets_and_optional_keys`, `missing_key_is_left_out_with_warning`, `shared_key_stored_once`, `oversized_key_is_refused`, `used_user_schemes_are_included`.
- [ ] **Step 2: Run** — Expected: FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** the gates — Expected: PASS.
- [ ] **Step 5: Commit** `feat(archive): build full and shared archives`.

### Task 4: Planning and applying imports

**Files:**
- Create: `crates/fbterm/src/archive_import.rs` (pure planning + an apply step returning file writes and secret ops)

**Interfaces:**
- Produces: `pub(crate) struct ImportPlan { pub entries: Vec<ImportEntry>, pub schemes: Vec<SchemeEntry>, pub has_secrets: bool, pub has_keys: bool }`, `ImportEntry { incoming: Profile, matched: Option<ProfileId>, choice: Choice, error: Option<String> }`, `enum Choice { Skip, Overwrite, KeepBoth }`; `pub(crate) fn plan(local: &ProfileStore, archive: &Archive, schemes: &SchemeSet) -> ImportPlan`; `pub(crate) fn set_choice(plan: &mut ImportPlan, i: usize, c: Choice)` (re-validates references); `pub(crate) fn apply(plan: &ImportPlan, archive: &Archive, local: &ProfileStore, key_dir: &Path) -> Result<Applied, String>` where `Applied { store: ProfileStore, secret_ops: Vec<SecretOp>, key_writes: Vec<(PathBuf, Vec<u8>)>, scheme_writes: Vec<Scheme> }` computed without touching disk; the app writes keys (mode 0600, never replacing), schemes, `profiles.toml` (via the existing save path), then secrets.
- [ ] **Step 1: Failing tests**: `matches_by_id_then_name`, `default_choice_is_skip`, `keep_both_renames_and_rewires`, `skip_points_references_at_local`, `invalid_reference_blocks_entry`, `shared_overwrite_keeps_local_secrets_and_key_path`, `existing_key_file_is_never_replaced`, `identical_key_file_is_reused`, `scheme_clash_imports_copy`.
- [ ] **Step 2: Run** — Expected: FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** the gates — Expected: PASS.
- [ ] **Step 5: Commit** `feat(archive): plan and apply imports`.

### Task 5: Export and import dialogs

**Files:**
- Create: `crates/fbterm/src/archive_window/{mod.rs, view.rs}` (one window with an Export and an Import mode, like Settings)
- Modify: `app.rs`, `window.rs` routing, `editor/view.rs` footer buttons, `palette.rs`, `actions.rs`, `crates/fbterm-config/src/keys.rs` (`Action::ExportProfiles` `export_profiles`, `Action::ImportProfiles` `import_profiles`, no default chords), `debug.rs`/`main.rs`/`CLAUDE.md` (`FBTERM_OPEN_ARCHIVE=export|import`, `FBTERM_SCREENSHOT_WINDOW=archive`)

**Interfaces:**
- Export mode: grouped checklist (all checked), included-by-jump rows marked `Included for jump hosts`, kind choice, `Include private key files` (Full only), warnings list, password and confirmation (must match, non-empty), `Export…` opens the `rfd` save dialog (`fbterm-profiles-{date}.fbprofiles`), sealing runs on the blocking pool.
- Import mode: `Choose file…`, password, preview table (name, host, jump chain, tunnels count, match, choice segmented control, error), key folder choice (default `~/.ssh/`), `Import`; respects read-only profiles; notice with counts when done.
- [ ] **Step 1: Failing tests**: `export_requires_matching_password`, `export_writes_openable_archive`, `import_preview_lists_entries_and_clashes`, `import_applies_choices_and_writes_keys_0600`, `import_refused_when_profiles_read_only`, `actions_open_the_window`.
- [ ] **Step 2: Run** — Expected: FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** the gates; screenshots of both modes with sample data — Expected: PASS.
- [ ] **Step 5: Commit** `feat(archive): export and import dialogs`.

### Task 6: The vault store

**Files:**
- Create: `crates/fbterm-secrets/src/vault.rs`; Modify: `lib.rs` (`SecretStore::entries(&self) -> Result<Vec<(ProfileId, SecretKind, Zeroizing<String>)>, SecretError>`, `SecretError::Locked`), `keychain.rs` (`entries`, serialized-size check), `paths.rs` (`Paths::vault()` = `<config>/secrets.fbvault`)

**Interfaces:**
- Produces: `pub struct VaultStore` with `create(path, password)`, `unlock(path, password) -> Result<(), SecretError>`, `is_unlocked()`, `reset(path)`; get/set/remove return `SecretError::Locked` until unlocked; writes are atomic and re-encrypt the whole file; contents only in memory while unlocked.
- [ ] **Step 1: Failing tests**: `vault_round_trip`, `locked_vault_reports_locked`, `wrong_main_password_is_refused`, `vault_file_never_contains_plaintext`, `reset_removes_file`, `keychain_reports_entries_and_size`.
- [ ] **Step 2: Run** — Expected: FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** the gates — Expected: PASS.
- [ ] **Step 5: Commit** `feat(secrets): encrypted vault store`.

### Task 6b: Windows system storage with DPAPI

**Files:**
- Create: `crates/fbterm-secrets/src/dpapi.rs` (`#[cfg(windows)]`); Modify: `lib.rs`, `keychain.rs` (remove the size check), `paths.rs` (`Paths::dpapi()` = `<config>/secrets.dpapi`), workspace `Cargo.toml` (`windows-sys` with the `Win32_Security_Cryptography` and `Win32_Foundation` features, a version already in the lockfile), `LICENSES.md`

**Interfaces:**
- Produces: `pub struct DpapiStore` implementing `SecretStore` (get/set/remove/remove_kind/entries/set_many) over a TOML map (`format = 1`) protected with `CryptProtectData`/`CryptUnprotectData` (`CRYPTPROTECT_UI_FORBIDDEN`, fixed app entropy), atomic writes (part file, flush, rename), bounded reads, `Zeroizing` buffers, `LocalFree` on every output blob; a pure `Codec` seam so the TOML map, atomic write and error mapping are unit-tested on every platform with a fake codec, and the DPAPI round trip is tested on Windows CI. The system-storage constructor returns `DpapiStore` on Windows and the keyring store elsewhere. `keychain_blob_bytes`/`keychain_fits` are removed.
- [ ] **Step 1: Failing tests**: `dpapi_map_round_trip` (fake codec), `dpapi_write_is_atomic`, `dpapi_corrupt_file_is_refused`, `dpapi_file_never_contains_plaintext` (Windows), `dpapi_real_round_trip` (Windows).
- [ ] **Step 2: Run** — Expected: FAIL.
- [ ] **Step 3: Implement.** Only the DPAPI calls are `unsafe`, each with `// SAFETY:`.
- [ ] **Step 4: Run** the gates and Windows CI — Expected: PASS.
- [ ] **Step 5: Commit** `feat(secrets): DPAPI system storage on Windows`.

### Task 7: Choosing and switching storage

**Files:**
- Modify: `crates/fbterm-secrets/src/lib.rs` (`SecretBackend` switcher implementing `SecretStore`, replacing `FallbackStore`), `crates/fbterm-config/src/{settings.rs, settings_edit.rs}` (`secret_storage`), `crates/fbterm/src/{app.rs, prompts.rs, prompt_view.rs, settings_window/*}`, `fbterm-session/src/ssh/auth.rs` (`SecretError::Locked` → ask the app to unlock via the prompter, else treat as not saved), editor degraded notes

**Interfaces:**
- Produces: `SecretBackend::new(mode, paths)`, `switch_to(mode, main_password: Option<&str>) -> Result<(), SecretError>` (copy every entry, verify, then remove from the old storage; any read failure aborts with nothing removed), automatic switch detection on startup (system storage unavailable); `Prompter::unlock_vault(&self) -> Option<Zeroizing<String>>` with the unlock dialog; Settings `Saved secrets` section with the three choices (switching asks for or sets the main password), `Reset vault` with confirmation; the editor's notes under secret fields say where secrets are kept.
- [ ] **Step 1: Failing tests**: `auto_uses_keychain_when_it_works`, `auto_switches_when_keychain_unavailable`, `switch_to_vault_moves_everything`, `switch_back_to_keychain_moves_everything`, `switch_aborts_when_old_store_unreadable`, `locked_vault_asks_once_per_session`, `cancel_unlock_asks_for_password_instead`, `reset_vault_keeps_profiles`, `settings_section_switches_storage`.
- [ ] **Step 2: Run** — Expected: FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** the gates — Expected: PASS.
- [ ] **Step 5: Commit** `feat(secrets): choose between keychain and vault`.

### Task 8: Docs

**Files:**
- Create: `website/docs/export-import.md`, `website/docs/saved-secrets.md` (sidebar entries)
- Modify: `profiles.mdx` (key profiles without a key file), `files.md` (`secrets.fbvault`, `secret_storage`), `keyboard-shortcuts.md`, main spec 4, 5.4, 5.9 and 6 (milestone 5 done), `CLAUDE.md` (secrets rule now names the vault; Status; gotcha lines on shared-archive stripping and `SecretBackend`)

- [ ] **Step 1: Write** the docs, screenshots with sample data only.
- [ ] **Step 2: Run** `cd website && npm run build` and the gates — Expected: PASS.
- [ ] **Step 3: Commit** `docs: export, import and saved secrets`.

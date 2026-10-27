# File Panel Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A side panel that browses the focused SSH session's server over SFTP and downloads and uploads files safely, with a transfer queue that never blocks the terminal.

**Architecture:** `fbterm-session::ssh::sftp` opens an SFTP channel on the profile's shared connection (`SshPool::acquire`, added by the tunnels work) and exposes listing and safe transfer primitives; `fbterm-session::ssh::transfer` runs transfers as tasks with cancel and coalesced progress. The app keeps one `FilePanels` registry per connection (folder per profile, transfer queue) and docks a panel at the active tab's right edge, outside the tiles, following the focused pane.

**Tech Stack:** Rust 2024, russh 0.64.1 plus `russh-sftp` (new dependency), tokio, iced 0.14, `rfd` (already used), `directories` (already used) for the Downloads folder.

**Spec:** `docs/superpowers/specs/2026-10-27-file-panel-design.md` (approved; refines `2026-10-23-featherbit-terminal-design.md` 5.8).

**Prerequisite:** the tunnels branch is merged into `develop` first (`SshPool::acquire`, public `Connection`, `Connection::closed()`, `NoPrompt`); rebase `feature/file-panel` on `develop` before Task 1.

## Global Constraints

- In-process SSH only; SFTP rides the profile's one shared connection and its jump chain; the connection stays open while the panel or a transfer uses it. No SCP or other fallback when SFTP is unavailable: show the server's error inline.
- A remote program can never trigger a transfer, open the panel or write on this machine; downloads only on the user's click, to Downloads or the folder the user chose.
- Downloads: write `.<name>.fbterm-part` next to the destination, rename on completion; `Download` into Downloads picks a free name `name (1).ext`, `name (2).ext`, …; cancel or failure deletes the part file. Symlink to a file downloads its target; special files and broken links can't be downloaded; a size change during download is not an error.
- Uploads: never replace without the user's choice. Upload to `.<name>.fbterm-part` in the target folder; on Replace copy the existing file's mode (and uid/gid when `setstat` allows) to the part file and `posix-rename@openssh.com` over the target; if the folder isn't writable or the extension is missing, write the existing file in place and mark the row `Replaced in place`; a cancelled or failed in-place replace marks the row `File may be incomplete`. New uploads keep the server's default permissions. Folders are not uploaded (skipped with `Folders are not uploaded: {name}` in the queue).
- Clash dialog copy: `"{name}" already exists in {folder}.` buttons `Replace`, `Rename`, `Cancel`; `Rename` shows a name field prefilled `name (1).ext`; with several files, `Apply to all` (for Replace and Cancel).
- Transfers belong to the connection: hiding the panel or moving focus never cancels them; closing a tab or window, or quitting, while that connection has running transfers asks `{n} transfers are still running.` (`1 transfer is still running.`) with `Cancel transfers and close` / `Keep open`. A dropped connection fails running transfers with the reason and offers `Retry`.
- Panel copy: palette action `File panel`; buttons `Download`, `Download as…`, `Upload…`, `Show in Finder` (`Show in folder` off macOS), `Retry`; placeholders `Not connected`, `Local session`, `SFTP is not available: {reason}`; filter placeholder `Filter`; toggle `Show hidden files` (default on, remembered app-wide).
- Panel follows the focused pane's profile; each profile's last folder is kept for the app session; the panel starts in the server's answer for `.`.
- Listing: folders first then files, by name; click a header to sort by name, size or modified (again reverses); `..` row and Backspace go up; Enter or double-click opens a folder; one selected item at a time; filter is a case-insensitive substring match on the loaded folder; names shown as plain text with control characters replaced by `?`.
- Drag and drop: a drop anywhere in a window with the panel open uploads to the panel's current folder; with the panel closed, drops are ignored.
- Redraw contract: progress coalesced to at most 4 updates per second per transfer; no periodic subscription.
- Cross-platform; no new `unsafe`; no emoji or em-dashes; never name another terminal application; Conventional Commits with no attribution of any kind; `cargo deny` must pass with the new dependency (license recorded in `LICENSES.md`).
- Gates per task: `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`; live tasks also the Docker live tests; `cd website && npm run build` when docs change.

## Review Focus

1. Replacing `/etc/app/app.conf` owned by root with mode 0640 as a user who may write the file but not the folder: falls back to in-place write, mode and owner unchanged, row says `Replaced in place`. Test in Task 2 (`replace_without_folder_write_writes_in_place`).
2. A 2 GB log archive download cancelled halfway: no file left in Downloads, the terminal kept responding. Test in Task 2 (`cancel_download_leaves_nothing`) and Task 6 (`transfers_do_not_block_terminal_input`).
3. A file name with escape sequences or newlines on the server: shown with `?`, downloads to a safe local name. Test in Task 4 (`control_chars_in_names_are_neutralized`) and Task 5 (`download_name_is_sanitized`).
4. Network drop mid-upload: the row fails with the reason; the remote target is untouched (part file removed when possible); Retry works after reconnecting. Test in Task 2 (`connection_drop_fails_upload_and_keeps_target`).
5. Quitting with a transfer running: asks first; `Keep open` keeps everything. Test in Task 6 (`quit_with_running_transfer_asks`).

---

### Task 1: SFTP channel and listing

**Files:**
- Modify: `Cargo.toml` (workspace dep `russh-sftp`), `crates/fbterm-session/Cargo.toml`, `deny.toml` only if needed, `LICENSES.md`
- Create: `crates/fbterm-session/src/ssh/sftp.rs`, `crates/fbterm-session/tests/ssh_sftp.rs`, `.github/workflows/ci.yml` (run the new live file)

**Interfaces:**
- Pick the newest `russh-sftp` release compatible with `russh 0.64.1` (check its `russh` requirement and that it builds); if none is compatible, stop and report BLOCKED with the evidence.
- Produces: `pub struct Sftp` with `pub async fn open(pool: &SshPool, store: &ProfileStore, profile: &ProfileId, prompter: Arc<dyn Prompter>) -> Result<Sftp, SshError>` (holds the `Arc<Connection>`), `home(&self) -> Result<String, SftpError>` (`realpath(".")`), `list(&self, path: &str) -> Result<Vec<Entry>, SftpError>`, `stat`, `lstat`, `supports_posix_rename(&self) -> bool`; `pub struct Entry { pub name: String, pub kind: EntryKind, pub size: u64, pub modified: Option<SystemTime>, pub mode: Option<u32> }`, `pub enum EntryKind { File, Dir, Link { target: String, to: Option<Box<EntryKind>> }, Other }`; `pub enum SftpError { Unavailable(String), Denied(String), NotFound(String), Other(String) }` with plain messages.
- [ ] **Step 1: Failing tests**: live `home_is_reported`, `list_home_through_jump` (two hops), `list_shows_links_and_kinds`, `missing_folder_is_not_found`, `denied_folder_is_denied` (`/root`), unit `entry_sorting_helpers`.
- [ ] **Step 2: Run** — Expected: FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** the gates, `cargo deny --all-features check`, live tests — Expected: PASS.
- [ ] **Step 5: Commit** `feat(sftp): sftp channel and listing`.

### Task 2: Safe transfers

**Files:**
- Create: `crates/fbterm-session/src/ssh/transfer.rs`
- Modify: `crates/fbterm-session/tests/ssh_sftp.rs`

**Interfaces:**
- Consumes: Task 1 `Sftp`, `Entry`.
- Produces:
  - `pub struct TransferId(pub u64)`; `pub enum TransferEvent { Progress { done: u64, total: Option<u64> }, Finished { note: Option<String> /* "Replaced in place" */ , local: Option<PathBuf> }, Failed(String), Cancelled }`; `pub type TransferSink = mpsc::UnboundedSender<(TransferId, TransferEvent)>`.
  - `pub fn download(sftp: Arc<Sftp>, remote: String, dest: PathBuf, events: TransferSink) -> TransferHandle` (part file next to `dest`, rename on completion, removed on cancel/failure).
  - `pub fn upload(sftp: Arc<Sftp>, local: PathBuf, folder: String, name: String, mode: UploadMode, events: TransferSink) -> TransferHandle` with `pub enum UploadMode { New, Replace }` (Replace algorithm exactly as in Global Constraints).
  - `TransferHandle::cancel(&self)`; dropping does not cancel (the app owns handles).
  - `pub fn free_name(dir: &Path, name: &str) -> PathBuf` and `pub fn safe_local_name(remote: &str) -> String` (no path separators, no control characters, not empty).
- [ ] **Step 1: Failing tests**: unit `free_name_counts_up`, `download_name_is_sanitized`; live `download_round_trip`, `cancel_download_leaves_nothing`, `upload_new_file`, `replace_keeps_mode`, `replace_without_folder_write_writes_in_place` (prepare a file in a root-owned folder with `sudo`-free setup in the test container: a folder `chmod 555` owned by the user with a writable file inside), `connection_drop_fails_upload_and_keeps_target`, `progress_is_coalesced`.
- [ ] **Step 2: Run** — Expected: FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** the gates and live tests — Expected: PASS.
- [ ] **Step 5: Commit** `feat(sftp): safe downloads and uploads`.

### Task 3: Panel state, docking and focus

**Files:**
- Create: `crates/fbterm/src/file_panel/{mod.rs, state.rs}`
- Modify: `app.rs` (`App.file_panels`, messages), `window.rs` (dock at the active tab's right edge outside the tiles with a draggable left edge; width remembered in `state.toml`), `crates/fbterm-config/src/{keys.rs, state.rs}` (`Action::FilePanel`, config name `file_panel`, default Cmd+Shift+E / Ctrl+Shift+E, title `File panel`; `SavedState.file_panel_width: Option<f32>`), `palette.rs`, `actions.rs`, `tiles.rs` only if the content area must shrink

**Interfaces:**
- Produces: `FilePanels { open: HashSet<window::Id>, by_profile: HashMap<ProfileId, ProfilePanel { sftp: Option<Arc<Sftp>>, folder: String, listing: Listing, transfers: Vec<TransferRow> }>, width: f32 }`; the panel shows the focused pane's profile; `Not connected` / `Local session` placeholders; opening on a connected SSH pane opens SFTP via `UiPrompter` for that window; the SFTP connection keeps the profile's connection open while the panel or a transfer uses it.
- [ ] **Step 1: Failing tests**: `toggle_opens_and_closes_panel`, `panel_follows_focused_pane_profile`, `local_pane_shows_placeholder`, `folder_is_kept_per_profile`, `width_is_restored`, `single_pane_tab_layout_unchanged_when_closed` (content area identical with the panel closed).
- [ ] **Step 2: Run** — Expected: FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** the gates; screenshot of the docked panel with a sample listing — Expected: PASS.
- [ ] **Step 5: Commit** `feat(file-panel): dock the panel and follow the focused pane`.

### Task 4: Browsing

**Files:**
- Create: `crates/fbterm/src/file_panel/view.rs`
- Modify: `file_panel/{mod.rs, state.rs}`

**Interfaces:**
- Produces: path bar (clickable segments; click the empty end or press a key to type a path; Enter goes there), list with folders first and sortable headers, `..` row, Enter/double-click/Backspace, single selection, filter box, `Show hidden files` toggle (setting kept with the panel width in `state.toml`), inline errors, control characters shown as `?`. Listing loads asynchronously; a slow folder shows `Loading…` and the panel stays responsive.
- [ ] **Step 1: Failing tests**: `folders_first_then_files`, `sort_toggles_direction`, `filter_is_substring_case_insensitive`, `hidden_toggle_hides_dotfiles`, `path_bar_navigates`, `error_shows_inline`, `control_chars_in_names_are_neutralized`.
- [ ] **Step 2: Run** — Expected: FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** the gates; screenshot — Expected: PASS.
- [ ] **Step 5: Commit** `feat(file-panel): browse folders`.

### Task 5: Download

**Files:**
- Modify: `file_panel/*`, `app.rs`

**Interfaces:**
- Consumes: Task 2 `download`, `free_name`, `safe_local_name`; `rfd` save dialog parented to the window (as `export.rs`); Downloads folder from `directories::UserDirs::download_dir()` (fallback: home).
- Produces: `Download` (to Downloads, free name), `Download as…` (save dialog prefilled with the safe name), enabled only for a selected file or link-to-file; finished rows offer `Show in Finder` (`config_files::reveal_file`).
- [ ] **Step 1: Failing tests**: `download_goes_to_downloads_with_free_name`, `download_as_uses_chosen_path`, `download_disabled_for_folders_and_special_files`, `download_name_is_sanitized`.
- [ ] **Step 2: Run** — Expected: FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** the gates — Expected: PASS.
- [ ] **Step 5: Commit** `feat(file-panel): download files`.

### Task 6: Upload, drag and drop, queue, close confirmation

**Files:**
- Modify: `file_panel/*`, `app.rs` (window `FileDropped` events), `windows.rs` (close and quit confirmation), `prompts.rs`/`prompt_view.rs` (clash and close dialogs in the existing dialog layer)

**Interfaces:**
- Consumes: Task 2 `upload`, `UploadMode`.
- Produces: `Upload…` (multi-file pick, folders skipped with the queue note), drops while the panel is open, the clash dialog with `Rename` and `Apply to all`, the queue (name, direction, bytes, bar, cancel, `Retry` on failure, dismiss), the close/quit confirmation with exact copy.
- [ ] **Step 1: Failing tests**: `upload_new_files_into_current_folder`, `clash_asks_replace_rename_cancel`, `apply_to_all_covers_remaining`, `rename_prefills_free_name`, `drop_uploads_when_panel_open_and_is_ignored_when_closed`, `folders_are_skipped_with_note`, `quit_with_running_transfer_asks`, `close_tab_with_running_transfer_asks`, `retry_after_connection_drop`, `transfers_do_not_block_terminal_input` (input to a shell pane is delivered while a transfer runs).
- [ ] **Step 2: Run** — Expected: FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** the gates and live tests — Expected: PASS.
- [ ] **Step 5: Commit** `feat(file-panel): upload files and manage transfers`.

### Task 7: Docs

**Files:**
- Create: `website/docs/file-panel.md` (sidebar after `tunnels`), screenshot with sample data only
- Modify: `keyboard-shortcuts.md` (`file_panel`), `files.md` (`state.toml` panel keys), main spec 5.8 and 6 (milestone 4 done), `CLAUDE.md` (Status; a gotcha line on transfer ownership and the part-file rules), `LICENSES.md` if not done in Task 1

- [ ] **Step 1: Write** the docs.
- [ ] **Step 2: Run** `cd website && npm run build` and the gates — Expected: PASS.
- [ ] **Step 3: Commit** `docs(file-panel): file panel guide`.

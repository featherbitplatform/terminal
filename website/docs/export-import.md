---
id: export-import
title: Export and import profiles
sidebar_label: Export and import
---

An archive is one encrypted file, with the extension `.fbprofiles`, that holds profiles. Use it to move your setup to another machine or to hand a team the same set of hosts. Archives are encrypted with a password you choose, using the `age` format with a passphrase; nobody can read one without that password.

![The Export profiles window: the profile list, the two kinds and a warning about a key file that could not be read](/img/export-profiles.png)

## Exporting

Open **Export profiles…** from the palette (`Cmd+Shift+P` or `Ctrl+Shift+P`), or press **Export…** in the footer of the profile editor. The `export_profiles` action has no shortcut by default; give it one in **Keyboard** in Settings (see [Keyboard shortcuts](keyboard-shortcuts#the-keys-table)).

The window lists every profile, grouped, all checked. Uncheck what you do not want to send, choose a kind, type the archive password twice and press **Export…** to pick where the file goes.

### Full and Shared

| | **Full (for your own machines)** | **Shared (no secrets, for teammates)** |
| --- | --- | --- |
| Profiles | Local and SSH | SSH only |
| Saved passwords and key passphrases | Yes, always (a key profile's saved passphrase travels even when the profile does not say its key has one) | Never |
| Private key files | Only if you check **Include private key files** | Never |
| Private key paths | Kept | Removed; each person points the profile at their own key |
| Tunnels, color scheme choice and the color schemes of yours that profiles use | Yes | Yes |

A shared archive cannot carry a password, a passphrase, a private key or a key path, whatever your profiles hold: those fields are left out when the archive is built, not hidden afterwards. A password profile or a key profile still arrives as that kind; the recipient types the password or picks a key the first time they connect. A key profile that arrives without a key file asks for one (see [Profiles](profiles#authentication)).

Local profiles (a program on your machine) make no sense on someone else's computer, so a shared archive leaves them out and the window says so: `Local profiles are not part of a shared archive, so {name} was left out.` If nothing but local profiles is checked, there is nothing to share and the export stops with an error.

### Jump hosts

A profile that jumps through other profiles needs them to work, so those are included automatically even when unchecked. Their rows show `Included for jump hosts`. This follows chains of any depth.

### Key files

**Include private key files** is off by default. When on, the key file of each profile that uses one is stored inside the archive; a key used by several profiles is stored once. Keys that cannot go in are left out and listed as warnings in the window, and the export still goes ahead:

- A key file that cannot be read: `The key file ~/.ssh/id_dev of dev-box could not be read, so it was left out.`
- A key file larger than 1 MB: it is not a key, so it is left out with a warning.

Keep an archive with key files as carefully as the keys themselves.

A full archive carries saved secrets, so with the encrypted vault in use and locked, exporting asks for the main password first.

## Importing

Open **Import profiles…** from the palette, or **Import…** in the profile editor's footer (the `import_profiles` action, also without a default shortcut). Choose the file, type its password and press **Open**. Nothing is changed yet: the window shows a preview.

A wrong password says `Wrong password, or the file is not an fbterm archive.` An archive made by a newer version of the app says `This archive was made by a newer fbterm.`; update the app to open it. A damaged file says so too. An archive with an unreasonable amount in it (more than 1000 profiles, 200 color schemes or 200 key files, a profile with more than 16 jumps or the same jump twice, or more than 2000 saved secrets) is refused as a whole with `This archive is too large or malformed to import.` A file over 64 MB says `The file is too large to be an fbterm archive.`

### The preview

The top line says what the archive is: `Full archive. Includes saved secrets and private key files.` or `Shared archive: no saved secrets or key files.` Under it, one row per profile, with what it connects to, its jump hosts and tunnels, and a tag:

- **New**: no profile here matches. Choices are **Import** (the default) and **Skip**.
- **Matches** followed by the local profile's name: a local profile has the same id, or else the same name. Choices are **Skip** (the default), **Overwrite** and **Keep both**.

| Choice | What happens |
| --- | --- |
| **Skip** | The profile is not imported. Profiles of the archive that jump through it use the one you already have. |
| **Overwrite** | The local profile is replaced by the archive's. |
| **Keep both** | The archive's profile is added as a new profile named `{name} (imported)` (`{name} (imported 2)` and so on if that name is taken). Profiles in the archive that jump through it follow the copy. |

Overwrite replaces only what the archive carries:

- A shared archive carries no secrets and no key paths, so overwriting keeps your saved password, your saved passphrase and your key path.
- A full archive without key files carries key paths that mean nothing on this machine, so overwriting keeps your local key path.
- A key path from an archive is used only when it is an absolute path on this computer or starts with `~/`. Network shares (`\\server\share\...`, `//server/share/...`), devices (`/dev/...`, `\\.\...`), paths with `..` and relative paths are dropped: that profile asks you to choose its key file the first time you connect. A preview row shows `Key file` and the path when the profile keeps one from the archive.
- A full archive with the key file replaces the path with the file the import writes.

A row whose result would be invalid is marked in red with the reason and cannot be imported until you change its choice: it is not an SSH profile, its jump chain loops back to itself or goes through more than 32 jump hosts, it jumps through a profile that is neither in the archive nor here, a profile with that name already exists, and so on. Color schemes in the archive are listed too: a scheme you already have with the same id and content is reused, and one that clashes is added with a new id and the name `{name} (imported)`.

### Key files

When the archive carries key files for profiles you import, a **Key files** section names the folder they are written to, `~/.ssh/` by default; **Choose folder…** changes it. Each file is written readable by you only (mode `0600` on macOS and Linux). A file that is already there with the same contents is reused. A file with the same name but different contents is never replaced: the new one gets another name, such as `id_ed25519-2`. If a file appears in the folder while the import runs, the import stops and writes nothing more.

### Saved secrets

The saved passwords and passphrases of a full archive are stored where you keep [saved secrets](saved-secrets). With the encrypted vault in use, the import asks for the main password first when the vault is locked, or asks you to set a main password when there is no vault yet. With **Automatic** storage not checked yet in this session, the import checks it first, so a machine with no system storage moves to the vault and asks for a main password before anything is written. A key profile that receives a passphrase is marked as having one.

### When import is not available

- While the profile editor has unsaved changes the import is refused (`The profile editor has unsaved changes. Save or discard them before importing.`); save or discard them first.
- While the profile editor is still saving the import is refused too (`The profile editor is still saving. Wait for it to finish before importing.`).
- While a connection's main password dialog is open, an import or full export that needs the vault says `Finish unlocking saved secrets first.` (or `Finish setting the main password first.`) and waits for you to answer that dialog.
- While profiles are read-only because `profiles.toml` could not be read, nothing can be imported until the file is fixed.

The import writes key files, then color schemes, then `profiles.toml`, then secrets. If a step fails, the window says which, and later steps are not run.

## Files and size

An archive is a normal file; copy it anywhere. Opening and sealing take about a second on purpose, because the password is stretched against guessing. A file over 64 MB is refused as too large; real archives are far smaller (each key file is at most 1 MB).

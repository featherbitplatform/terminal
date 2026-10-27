# Profile export/import and the secrets vault: design

Status: approved by the owner, 2026-10-27. Milestone 5 of
`2026-10-23-featherbit-terminal-design.md` (sections 4 "Saved passwords", 5.4 and 5.9), which this
document refines. Where they differ, this document wins.

## 1. Intent

Two real moments drive this. Setting up the owner's **second machine** should take one archive:
every profile, jump chain, tunnel and saved password, optionally the SSH keys too. Onboarding a
**new teammate** should take one archive with the team's hosts and jump chains and none of the
owner's secrets, so the teammate plugs in their own credentials.

Separately, saved secrets must stay safe on every machine. The system's secure storage is the
default wherever it works well; where it doesn't, secrets go into an encrypted vault file
unlocked with a main password, and the user can also choose the vault on purpose.

Success: the owner exports a full archive with key files, imports it on a fresh machine, and
connects to the bastion-and-database setup without retyping anything; a teammate imports a
shared archive, is asked for their own password on first connect, and has the same tunnels.

## 2. Goals

1. **Export** selected profiles (all or some) as an archive of one of two kinds:
   - **Full:** profiles plus saved passwords and key passphrases; an opt-in **Include private key
     files** embeds the key files the profiles reference.
   - **Shared:** profiles with passwords, key passphrases and private key paths removed; the
     authentication method stays as a hint (e.g. "key").
2. Profiles reached through a selected profile's jump chain are always included. Tunnels and a
   profile's color scheme setting travel with the profile, and a user-made color scheme a
   profile uses travels as well *(my guess, approved)*.
3. Every archive is encrypted with a password typed at export, using `age` with a passphrase
   recipient (no home-grown crypto), saved as a `.fbprofiles` file.
4. **Import:** pick the file, type its password, see a preview of what is inside, resolve name
   clashes with **Skip / Overwrite / Keep both**, choose where key files go (default `~/.ssh/`,
   written readable only by the user), and the imported profiles point to them.
5. **Secret storage:**
   - The system's secure storage is the default: macOS Keychain, Linux Secret Service, and on
     Windows a file in the config directory encrypted with DPAPI for the user's Windows account
     (`secrets.dpapi`). *(Owner decision, 2026-10-27: Credential Manager caps a stored credential
     at about 2.5 KB and fbterm keeps all secrets in one entry, so Windows uses DPAPI instead; it
     has no size limit and needs no main password.)*
   - fbterm switches to the **vault** on its own where the system storage doesn't work (for
     example no secret service on Linux).
   - In Settings the user can **choose the vault** on any machine. Switching either way moves
     the saved secrets across.

## 3. Non-goals and anti-goals

- No cloud sync, no sharing service, no automatic export; archives are files the user moves.
- No partial-secret archives (e.g. passwords but not passphrases) beyond the two kinds.
- No recovery of a forgotten main password or archive password.
- Anti-goals, a failure even if the features work:
  - A shared archive must never contain a password, a passphrase, a private key or a private key
    path, whatever the profile looked like.
  - Secrets never appear in plain text on disk, in logs, in the preview, or in config files
    (`CLAUDE.md`: secrets only in secure storage, now also the vault).
  - An import never silently replaces a profile, a secret or a file on disk.
  - Switching storage never loses secrets: a failed move leaves the old storage intact.
  - Users who never use these features see no change (the keychain keeps working as today).

## 4. Behaviour

**Export** (palette `Export profiles…`, and a button in the profile editor). A dialog lists
profiles with checkboxes (all checked by default, grouped like the editor), shows the profiles
added through jump chains as included, offers the kind (Full / Shared), the **Include private key
files** checkbox (Full only), and an archive password typed twice. Saving uses the system save
dialog with the `.fbprofiles` extension.

**Import** (palette `Import profiles…`, and the profile editor). Pick a file, type its password.
The preview lists the profiles (name, host, jump chain, number of tunnels), whether secrets and
key files are included, and each clash by name with a Skip / Overwrite / Keep both choice
(default Skip). Key files go to a chosen folder (default `~/.ssh/`, mode `0600`); an existing key
file with the same name is never replaced (a new name is used). Jump-chain references and color
scheme ids are rewired to the imported or kept profiles and schemes.

**Vault.**
- An `age`-encrypted file in the config directory, unlocked with the main password.
- *(my guess, approved)* fbterm asks for the main password the first time a secret is needed in
  a session (connecting with a saved password, exporting a full archive) and keeps it unlocked
  until the app quits.
- Choosing the vault in Settings asks for a new main password (twice) and moves the secrets
  from the keychain into the vault, then removes them from the keychain. Choosing the keychain
  again moves them back after unlocking the vault.
- When fbterm switches to the vault on its own, it says why once (`The system keychain is not
  available`) and asks the user to set a main password the next time a secret is saved.
- *(my guess, approved)* A forgotten main password can't be recovered: `Reset vault` deletes the
  saved secrets (after a confirmation) and keeps the profiles.
- *(my guess, approved)* The main password and archive passwords are separate.

**Settled after the builder check** *(my calls, 2026-10-27; the owner can change any of them)*:

- **Key profiles in shared archives:** a key profile's key path becomes optional in the profile
  model. A shared archive carries "key" with no path and no passphrase flag. Connecting with a
  key profile that has no key asks the user to pick a key file (and remembers it in the
  profile), the same way a missing password is asked for.
- **Shared archives carry SSH profiles only.** Local profiles hold machine-specific programs and
  possibly tokens in their environment or arguments, so they are left out of shared archives;
  full archives carry them as they are.
- **Matching on import:** a profile matches an existing one with the same id (the same profile
  coming back, e.g. on re-import) or, failing that, the same name. Overwrite replaces only what
  the archive carries: importing a shared archive over an existing profile keeps that profile's
  saved password, passphrase and key path. Keep both gives the copy a new id and the name
  `{name} (imported)`, and references inside the archive follow the copy. Skip makes references
  inside the archive point at the existing local profile; if that would be invalid (not SSH, or
  a jump cycle), the preview shows the error on that profile and it can't be imported until
  resolved.
- **Key files:** a key file that is missing or unreadable at export is listed with a warning and
  left out (the export still succeeds); a key shared by several profiles is stored once; key
  files over 1 MB are refused as not keys. On import, an existing file with identical content is
  reused; a different file with the same name gets a new name.
- **Color schemes:** a user scheme travels with the profiles that use it. On import, a scheme
  with the same id and identical content is reused; a different one is imported with a new id
  and the name `{name} (imported)`, shown in the preview.
- **Storage choice:** a `secret_storage` setting with `"auto"` (default: keychain where it
  works, vault otherwise), `"keychain"` and `"vault"`. An automatic switch writes `"vault"` and
  says why once. Switching moves every secret; if reading the old storage fails, the switch is
  cancelled and nothing is removed from it. The vault replaces today's session-only fallback;
  cancelling the main-password prompt makes that connection ask for its password as if none were
  saved.

## 5. Constraints

- Everything in the main design and `CLAUDE.md` holds: `age` for every encryption, secrets only
  in secure storage or the vault, cross-platform, Graphite look, no naming of other terminal
  applications, Conventional Commits with no attribution.
- The archive format is versioned so future fbterm versions can read old archives; a newer
  archive than the app understands is refused with a clear message.
- Secret values in memory are wiped when no longer needed where the libraries allow; the vault's
  unlocked contents live only in memory.
- Tests cover: a shared archive never contains secrets or key paths (checked on the decrypted
  bytes), round trips of both kinds, wrong passwords, corrupt files, clashes, key file
  permissions, and storage switching both ways including failures.

## 6. Left to the builder

- Archive internal layout (e.g. a TOML manifest plus files in a tar inside `age`), versioning
  scheme, file name defaults.
- How the vault's main password prompt reuses the existing prompt dialogs.
- Dialog layouts for export, import preview and the Settings storage section.

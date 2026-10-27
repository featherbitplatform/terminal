---
id: saved-secrets
title: Saved secrets
sidebar_label: Saved secrets
---

Passwords and key passphrases you choose to remember are never written to `profiles.toml` or any other plain file. Where they go is the **Saved secrets** section of Settings (`Cmd+,` on macOS, `Ctrl+,` elsewhere), the `secret_storage` setting.

| Choice | Where secrets are kept |
| --- | --- |
| **Automatic** (default) | The system storage; see below. |
| **System keychain** | On macOS the Keychain, on Linux the Secret Service. On Windows the choice is called **Windows account**: a file, `secrets.dpapi`, in the [configuration directory](files), encrypted for your Windows account with DPAPI. |
| **Encrypted vault** | A file, `secrets.fbvault`, in the configuration directory, encrypted with a main password you choose. |

The system storage holds every secret as one item (service `fbterm`, account `secrets`), so macOS asks about it once, not once per profile. Unlike the system storage, the vault does not depend on the operating system: pick it on a machine where you do not trust the keychain, or want the same arrangement everywhere.

## Automatic

Automatic uses the system storage. The one exception is a machine that has no system storage at all, for example a Linux desktop without a Secret Service. There the app switches to the vault, once: it writes `secret_storage = "vault"`, says `The system keychain is not available, so saved secrets now use an encrypted vault.` and asks you to set a main password when you next save a secret.

The app only looks at the system storage when a secret is first needed, so launching it never prompts. A storage that exists but refuses (access denied, a locked keychain, a cancelled prompt) never makes Automatic switch, because that would hide the secrets already in it; the connection asks for its password as if none were saved.

## The encrypted vault

The vault opens with a main password. The first time a connection needs a saved secret in a session, **Unlock saved secrets** asks for it, and the vault stays open until you quit. Cancelling means that connection asks for its password or passphrase as if none were saved.

The first time you save a secret with the vault in use and no vault yet, **Set a main password** asks for the password twice. Choose it carefully: **a forgotten main password cannot be recovered**. The vault is encrypted with it, and there is no other way in.

### Reset vault

With a vault in place, Settings shows **Reset vault**. It asks `Delete all saved passwords and passphrases? Profiles are kept.` and, once confirmed, deletes the vault file. Profiles are not touched; connections ask for their passwords again and the next save asks for a new main password. This is the way out of a forgotten main password.

## Switching

Choosing another storage in Settings moves every saved secret. The secrets are written to the new storage, read back and compared, and only then removed from the old one. If the old storage cannot be read (a locked vault you cannot open, a refused keychain), nothing is moved and nothing is removed; Settings says `Nothing was moved.` and the old storage stays in use. Moving into the vault asks you to set or enter the main password; moving out of a locked vault asks for it to read the secrets.

Switching is refused while `profiles.toml` cannot be read (`Saved secrets cannot move while profiles.toml cannot be read. Fix it first.`), because the profiles are how the app finds the secrets to move.

Notes under the password fields and the **Remember** checkbox in prompts name the storage in use, for example `Remember in the encrypted vault`.

## Editing the setting by hand

`secret_storage` is `"auto"`, `"keychain"` or `"vault"` in [`settings.toml`](files#settingstoml). Prefer the Settings window: it moves the secrets. A hand edit made while the app runs takes effect at the next launch and moves nothing, so secrets saved in the old place are not found until you switch back.

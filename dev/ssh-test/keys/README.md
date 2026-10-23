# Test-only SSH keys

These keys exist only to log in to the throwaway containers in `dev/ssh-test/compose.yaml`
during the live SSH tests. They protect nothing, are public on purpose and must never be
authorized anywhere else.

| File | Passphrase |
|---|---|
| `fb_test_ed25519` | none |
| `fb_test_ed25519_pass` | `fb-test-pass` |

Public halves live in `pub/`; every container authorizes both. Regenerate with
`ssh-keygen -t ed25519 -C fbterm-test -f fb_test_ed25519` (add `-N fb-test-pass` for the
protected one) and move the `.pub` files into `pub/`.

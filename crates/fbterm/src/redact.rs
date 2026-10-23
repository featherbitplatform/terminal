//! Secret text carried through messages: its `Debug` never shows the value.

/// A typed password or passphrase. `Debug` prints `<redacted>`.
#[derive(Clone, PartialEq, Eq, Default)]
pub struct Redacted(pub String);

impl std::fmt::Debug for Redacted {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("<redacted>")
    }
}

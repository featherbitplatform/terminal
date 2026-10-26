//! The font family list: installed monospace families, from the font database the renderer
//! already uses.

use std::collections::BTreeSet;
use std::fmt;

/// The bundled terminal face, listed first.
pub const BUNDLED: &str = "Geist Mono";

/// One entry of the font family list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FontChoice {
    pub family: String,
    /// False for a configured family that is not installed.
    pub installed: bool,
}

impl fmt::Display for FontChoice {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.installed {
            f.write_str(&self.family)
        } else {
            write!(f, "{} (not found)", self.family)
        }
    }
}

/// The families of every monospace face the renderer knows (system fonts and the bundled
/// ones).
pub fn installed_monospace() -> Vec<String> {
    let mut system = iced::advanced::graphics::text::font_system()
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let families: BTreeSet<String> = system
        .raw()
        .db()
        .faces()
        .filter(|face| face.monospaced)
        .filter_map(|face| face.families.first().map(|(name, _)| name.clone()))
        .collect();
    families.into_iter().collect()
}

/// The list to offer: the bundled face first, then the installed families by name, and the
/// configured family (marked not found) when it is not among them.
pub fn choices(installed: Vec<String>, current: &str) -> Vec<FontChoice> {
    let mut names: Vec<String> = installed
        .into_iter()
        .filter(|n| !n.trim().is_empty() && n != BUNDLED)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    names.sort_by_key(|n| n.to_lowercase());
    let mut out = vec![FontChoice {
        family: BUNDLED.into(),
        installed: true,
    }];
    if current != BUNDLED && !names.iter().any(|n| n == current) {
        out.push(FontChoice {
            family: current.into(),
            installed: false,
        });
    }
    out.extend(names.into_iter().map(|family| FontChoice {
        family,
        installed: true,
    }));
    out
}

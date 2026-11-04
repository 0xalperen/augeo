//! The first-run disclaimer. The engine refuses to apply anything until this exact text is accepted.

use sha2::{Digest, Sha256};

use crate::Disclaimer;

/// The text shown on first run and repeated word for word in the README.
pub const TEXT: &str = "augeo is provided \"as is\", without warranty of any kind, express or implied. \
It changes Windows and NVIDIA driver settings. The author and repository owner, and contributors, \
are not liable for any harm, damage, data loss, instability or other consequences from using it, \
including effects on games, anti-cheat systems or game accounts. You are responsible for your own \
system. Full terms are in the GPL-3.0 license, sections 15 and 16.";

/// Bump this whenever the text changes, so everyone sees the new text and accepts it again.
pub const VERSION: u32 = 1;

/// Identifies the current text, as stored in the journal when it is accepted.
pub fn current() -> Disclaimer {
    let digest = Sha256::digest(TEXT.as_bytes());
    Disclaimer {
        version: VERSION,
        text_sha256: digest.iter().map(|b| format!("{b:02x}")).collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Failing here means the text changed. Bump [`VERSION`], update this hash, and update the README.
    #[test]
    fn changing_the_text_forces_a_version_bump() {
        assert_eq!(VERSION, 1, "update the hash below when bumping the version");
        assert_eq!(
            current().text_sha256,
            "7933895d9a2ef80214b25c4b4cabc6918eba0aa882838df0a6b3ed8057bdab90",
            "the disclaimer text changed: bump VERSION and update this hash"
        );
    }

    #[test]
    fn the_readme_repeats_the_text_exactly() {
        let readme = include_str!("../../../README.md");
        let flattened = readme.split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(
            flattened.contains(TEXT),
            "README.md must contain the disclaimer word for word"
        );
    }

    #[test]
    fn the_text_says_what_it_must() {
        for phrase in [
            "\"as is\"",
            "without warranty",
            "not liable",
            "data loss",
            "GPL-3.0",
        ] {
            assert!(TEXT.contains(phrase), "{phrase}");
        }
    }

    #[test]
    fn the_license_is_the_unmodified_gpl3() {
        let license = include_str!("../../../LICENSE");
        assert!(license.contains("GNU GENERAL PUBLIC LICENSE"));
        assert!(license.contains("Version 3, 29 June 2007"));
        assert!(license.contains("15. Disclaimer of Warranty."));
        assert!(license.contains("16. Limitation of Liability."));
    }
}

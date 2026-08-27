//! Deterministic dictionary normalization and membership interfaces.

use std::{collections::BTreeSet, sync::LazyLock};

use crate::{DictionaryRef, ProfileReferenceError};

const BUNDLED_WORDS: &str = include_str!("../data/enable1.txt");

/// Stable bundled dictionary identifier.
pub const BUNDLED_DICTIONARY_ID: &str = "enable1-en";
/// Immutable bundled dictionary version.
pub const BUNDLED_DICTIONARY_VERSION: u32 = 1;
/// SHA-256 of the exact bundled UTF-8 word-list bytes.
pub const BUNDLED_DICTIONARY_SHA256: &str =
    "3f16130220645692ed49c7134e24a18504c2ca55b3c012f7290e3e77c63b1a89";

static BUNDLED_DICTIONARY: LazyLock<WordSetDictionary> =
    LazyLock::new(|| WordSetDictionary::new(BUNDLED_WORDS.lines()));

/// Read-only dictionary used by move validation.
pub trait Dictionary {
    /// Returns whether a normalized uppercase word is accepted.
    fn contains(&self, normalized_word: &str) -> bool;
}

/// Deterministic in-memory dictionary suitable for bundled word data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WordSetDictionary {
    words: BTreeSet<String>,
}

impl WordSetDictionary {
    /// Builds a dictionary, discarding blank and non-ASCII entries and normalizing accepted words.
    #[must_use]
    pub fn new(words: impl IntoIterator<Item = impl AsRef<str>>) -> Self {
        Self {
            words: words
                .into_iter()
                .filter_map(|word| normalize_word(word.as_ref()))
                .collect(),
        }
    }

    /// Returns the number of unique normalized words.
    #[must_use]
    pub fn len(&self) -> usize {
        self.words.len()
    }

    /// Returns whether the dictionary has no words.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.words.is_empty()
    }
}

impl Dictionary for WordSetDictionary {
    fn contains(&self, normalized_word: &str) -> bool {
        self.words.contains(normalized_word)
    }
}

/// Immutable dictionary bytes for an exact compatibility reference.
///
/// Unknown references fail closed so an application upgrade cannot silently replay a persisted
/// game against different word data.
#[must_use]
pub fn dictionary(reference: &DictionaryRef) -> Option<&'static WordSetDictionary> {
    (reference.id() == BUNDLED_DICTIONARY_ID
        && reference.version() == BUNDLED_DICTIONARY_VERSION
        && reference.checksum() == format!("sha256:{BUNDLED_DICTIONARY_SHA256}"))
    .then(bundled_dictionary)
}

/// Loads the immutable dictionary bundled with this crate.
#[must_use]
pub fn bundled_dictionary() -> &'static WordSetDictionary {
    &BUNDLED_DICTIONARY
}

/// Returns the compatibility reference persisted with games using the bundled dictionary.
///
/// # Errors
///
/// * Returns [`ProfileReferenceError`] only if the compile-time identifier, version, or checksum
///   constants become invalid.
pub fn bundled_dictionary_ref() -> Result<DictionaryRef, ProfileReferenceError> {
    DictionaryRef::new(
        BUNDLED_DICTIONARY_ID,
        BUNDLED_DICTIONARY_VERSION,
        format!("sha256:{BUNDLED_DICTIONARY_SHA256}"),
    )
}

/// Normalizes dictionary and board words to uppercase ASCII.
#[must_use]
pub fn normalize_word(word: &str) -> Option<String> {
    let word = word.trim();
    if word.is_empty() || !word.chars().all(|letter| letter.is_ascii_alphabetic()) {
        return None;
    }
    Some(word.to_ascii_uppercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dictionary_normalization_is_ascii_and_case_insensitive() {
        let dictionary = WordSetDictionary::new([" cat ", "DOG", "dog", "can't", ""]);

        assert_eq!(dictionary.len(), 2);
        assert!(dictionary.contains("CAT"));
        assert!(dictionary.contains("DOG"));
        assert_eq!(normalize_word("Cat"), Some("CAT".to_string()));
        assert_eq!(normalize_word("café"), None);
    }

    #[test]
    fn bundled_dictionary_has_stable_identity_and_content() {
        let reference = bundled_dictionary_ref().expect("bundled reference is valid");
        let bundled = bundled_dictionary();

        assert!(std::ptr::eq(bundled, bundled_dictionary(),));

        assert!(dictionary(&reference).is_some());
        let unsupported = DictionaryRef::new(
            BUNDLED_DICTIONARY_ID,
            BUNDLED_DICTIONARY_VERSION + 1,
            format!("sha256:{BUNDLED_DICTIONARY_SHA256}"),
        )
        .expect("future reference is structurally valid");
        assert!(dictionary(&unsupported).is_none());
        assert_eq!(reference.id(), BUNDLED_DICTIONARY_ID);
        assert_eq!(reference.version(), BUNDLED_DICTIONARY_VERSION);
        assert_eq!(
            reference.checksum(),
            format!("sha256:{BUNDLED_DICTIONARY_SHA256}")
        );
        assert_eq!(bundled.len(), 172_823);
        assert!(bundled.contains("WORD"));
        assert!(bundled.contains("ZYZZYVAS"));
    }
}

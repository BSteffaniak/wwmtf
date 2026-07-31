#![cfg_attr(feature = "fail-on-warnings", deny(warnings))]
#![warn(clippy::all, clippy::pedantic, clippy::nursery, clippy::cargo)]
#![allow(clippy::multiple_crate_versions)]

//! Deterministic, server-authoritative game-domain primitives.

use serde::{Deserialize, Serialize};
use std::fmt;
use thiserror::Error;
use time::OffsetDateTime;
use uuid::Uuid;

/// Stable identity of one game aggregate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct GameId(Uuid);

impl GameId {
    /// Creates a new random game identity.
    #[must_use]
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }

    /// Returns the underlying UUID.
    #[must_use]
    pub const fn as_uuid(self) -> Uuid {
        self.0
    }
}

impl Default for GameId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for GameId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

/// Immutable identity and version of data-driven game rules.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuleProfileRef {
    id: String,
    version: u32,
}

impl RuleProfileRef {
    /// Creates a pinned rules-profile reference.
    ///
    /// # Errors
    ///
    /// Returns [`ProfileReferenceError::EmptyId`] when `id` is empty after trimming,
    /// or [`ProfileReferenceError::ZeroVersion`] when `version` is zero.
    pub fn new(id: impl Into<String>, version: u32) -> Result<Self, ProfileReferenceError> {
        let id = id.into();
        if id.trim().is_empty() {
            return Err(ProfileReferenceError::EmptyId);
        }
        if version == 0 {
            return Err(ProfileReferenceError::ZeroVersion);
        }
        Ok(Self { id, version })
    }

    /// Returns the stable profile identifier.
    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Returns the immutable profile version.
    #[must_use]
    pub const fn version(&self) -> u32 {
        self.version
    }
}

/// Immutable identity, version, and content checksum of a dictionary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DictionaryRef {
    id: String,
    version: u32,
    checksum: String,
}

impl DictionaryRef {
    /// Creates a pinned dictionary reference.
    ///
    /// # Errors
    ///
    /// Returns [`ProfileReferenceError::EmptyId`] when `id` is empty,
    /// [`ProfileReferenceError::ZeroVersion`] when `version` is zero, or
    /// [`ProfileReferenceError::EmptyChecksum`] when `checksum` is empty.
    pub fn new(
        id: impl Into<String>,
        version: u32,
        checksum: impl Into<String>,
    ) -> Result<Self, ProfileReferenceError> {
        let id = id.into();
        let checksum = checksum.into();
        if id.trim().is_empty() {
            return Err(ProfileReferenceError::EmptyId);
        }
        if version == 0 {
            return Err(ProfileReferenceError::ZeroVersion);
        }
        if checksum.trim().is_empty() {
            return Err(ProfileReferenceError::EmptyChecksum);
        }
        Ok(Self {
            id,
            version,
            checksum,
        })
    }

    /// Returns the stable dictionary identifier.
    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Returns the immutable dictionary version.
    #[must_use]
    pub const fn version(&self) -> u32 {
        self.version
    }

    /// Returns the content checksum.
    #[must_use]
    pub fn checksum(&self) -> &str {
        &self.checksum
    }
}

/// Error constructing a compatibility-critical data reference.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum ProfileReferenceError {
    /// The stable identifier was empty.
    #[error("profile identifier cannot be empty")]
    EmptyId,
    /// Persisted versions start at one.
    #[error("profile version must be greater than zero")]
    ZeroVersion,
    /// The dictionary content checksum was empty.
    #[error("dictionary checksum cannot be empty")]
    EmptyChecksum,
}

/// Immutable metadata established when a game is created.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GameMetadata {
    id: GameId,
    rules: RuleProfileRef,
    dictionary: DictionaryRef,
    created_at: OffsetDateTime,
}

impl GameMetadata {
    /// Creates game metadata pinned to exact rules and dictionary data.
    #[must_use]
    pub const fn new(
        id: GameId,
        rules: RuleProfileRef,
        dictionary: DictionaryRef,
        created_at: OffsetDateTime,
    ) -> Self {
        Self {
            id,
            rules,
            dictionary,
            created_at,
        }
    }

    /// Returns the game identity.
    #[must_use]
    pub const fn id(&self) -> GameId {
        self.id
    }

    /// Returns the pinned rules profile.
    #[must_use]
    pub const fn rules(&self) -> &RuleProfileRef {
        &self.rules
    }

    /// Returns the pinned dictionary.
    #[must_use]
    pub const fn dictionary(&self) -> &DictionaryRef {
        &self.dictionary
    }

    /// Returns the creation timestamp.
    #[must_use]
    pub const fn created_at(&self) -> OffsetDateTime {
        self.created_at
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compatibility_references_require_versions_and_checksums() {
        assert_eq!(
            RuleProfileRef::new("", 1),
            Err(ProfileReferenceError::EmptyId)
        );
        assert_eq!(
            RuleProfileRef::new("classic-en", 0),
            Err(ProfileReferenceError::ZeroVersion)
        );
        assert_eq!(
            DictionaryRef::new("curated-en", 1, ""),
            Err(ProfileReferenceError::EmptyChecksum)
        );
    }

    #[test]
    fn game_metadata_round_trips_without_losing_pins() {
        let metadata = GameMetadata::new(
            GameId::new(),
            RuleProfileRef::new("classic-en", 1).expect("valid rules reference"),
            DictionaryRef::new("curated-en", 1, "sha256:example")
                .expect("valid dictionary reference"),
            OffsetDateTime::UNIX_EPOCH,
        );

        let encoded = serde_json::to_string(&metadata).expect("metadata serializes");
        let decoded: GameMetadata = serde_json::from_str(&encoded).expect("metadata deserializes");

        assert_eq!(decoded, metadata);
    }
}

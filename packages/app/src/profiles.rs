//! Renderer-neutral public account profiles and stable generated challenge handles.

use rand_core::{OsRng, RngCore as _};
use switchy_database::{Database, query::FilterableQuery as _};
use thiserror::Error;
use time::OffsetDateTime;

const MAX_DISPLAY_NAME_CHARS: usize = 80;
const HANDLE_SUFFIX_BYTES: usize = 4;
const HANDLE_SUFFIX_CHARS: usize = HANDLE_SUFFIX_BYTES * 2;
const MAX_HANDLE_CHARS: usize = 32;
const MAX_HANDLE_ATTEMPTS: usize = 8;

/// Ownership of one synchronized profile field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProfileFieldSource {
    Google,
    Custom,
}

impl ProfileFieldSource {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Google => "GOOGLE",
            Self::Custom => "CUSTOM",
        }
    }
}

/// Ownership of the account avatar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AvatarSource {
    Google,
    Custom,
    CustomNone,
}

impl AvatarSource {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Google => "GOOGLE",
            Self::Custom => "CUSTOM",
            Self::CustomNone => "CUSTOM_NONE",
        }
    }
}

/// Public WWMTF profile state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserProfile {
    pub user_id: String,
    pub display_name: String,
    pub display_name_source: ProfileFieldSource,
    pub avatar_source: AvatarSource,
    pub provider_picture_url: Option<String>,
}

/// Normalizes a provider or custom display name.
///
/// # Errors
///
/// * Returns [`ProfileError::InvalidDisplayName`] for control characters or a name longer than 80
///   Unicode scalar values.
pub fn normalize_display_name(value: &str) -> Result<String, ProfileError> {
    if value.chars().any(char::is_control) {
        return Err(ProfileError::InvalidDisplayName);
    }
    let normalized = value.split_whitespace().collect::<Vec<_>>().join(" ");
    let normalized = if normalized.is_empty() {
        "Player".to_string()
    } else {
        normalized
    };
    if normalized.chars().count() > MAX_DISPLAY_NAME_CHARS {
        return Err(ProfileError::InvalidDisplayName);
    }
    Ok(normalized)
}

/// Creates a unique stable challenge handle derived from a display name.
///
/// # Errors
///
/// * Returns [`ProfileError::HandleCollision`] if collision-safe generation is exhausted.
/// * Returns [`ProfileError::Database`] when persistence lookup fails.
pub async fn generate_unique_handle(
    db: &dyn Database,
    display_name: &str,
) -> Result<String, ProfileError> {
    let max_slug_chars = MAX_HANDLE_CHARS - HANDLE_SUFFIX_CHARS - 1;
    let slug = display_name
        .chars()
        .filter_map(|character| {
            character
                .is_ascii_alphanumeric()
                .then_some(character.to_ascii_lowercase())
                .or_else(|| matches!(character, ' ' | '_' | '-').then_some('-'))
        })
        .fold(String::new(), |mut slug, character| {
            if character != '-' || !slug.ends_with('-') {
                slug.push(character);
            }
            slug
        });
    let slug = slug.trim_matches('-');
    let slug = if slug.len() < 3 { "player" } else { slug };
    let slug = &slug[..slug.len().min(max_slug_chars)];
    for _ in 0..MAX_HANDLE_ATTEMPTS {
        let mut suffix = [0_u8; HANDLE_SUFFIX_BYTES];
        OsRng.fill_bytes(&mut suffix);
        let suffix = suffix.iter().fold(String::new(), |mut output, byte| {
            use std::fmt::Write as _;
            write!(output, "{byte:02x}").expect("writing to String is infallible");
            output
        });
        let handle = format!("{slug}-{suffix}");
        if db
            .select("users")
            .where_eq("username_normalized", handle.clone())
            .execute(db)
            .await?
            .is_empty()
        {
            return Ok(handle);
        }
    }
    Err(ProfileError::HandleCollision)
}

/// Creates the initial Google-owned profile for a user.
///
/// # Errors
///
/// * Returns display-name validation or database failures.
pub async fn create_google_profile(
    db: &dyn Database,
    user_id: &str,
    display_name: &str,
    provider_picture_url: Option<&str>,
    now: OffsetDateTime,
) -> Result<(), ProfileError> {
    let display_name = normalize_display_name(display_name)?;
    db.insert("user_profiles")
        .value("user_id", user_id)
        .value("display_name", display_name)
        .value("display_name_source", ProfileFieldSource::Google.as_str())
        .value("avatar_source", AvatarSource::Google.as_str())
        .value("provider_picture_url", provider_picture_url)
        .value("provider_picture_checked_at_ms", Option::<i64>::None)
        .value("updated_at_ms", timestamp_ms(now)?)
        .execute(db)
        .await?;
    Ok(())
}

/// Synchronizes provider-owned fields without overwriting customization.
///
/// # Errors
///
/// * Returns display-name validation, malformed persisted profile, or database failures.
pub async fn synchronize_google_profile(
    db: &dyn Database,
    user_id: &str,
    display_name: &str,
    provider_picture_url: Option<&str>,
    now: OffsetDateTime,
) -> Result<(), ProfileError> {
    let profile = load_profile(db, user_id)
        .await?
        .ok_or(ProfileError::Malformed)?;
    let mut query = db
        .update("user_profiles")
        .value("updated_at_ms", timestamp_ms(now)?);
    if profile.display_name_source == ProfileFieldSource::Google {
        query = query.value("display_name", normalize_display_name(display_name)?);
    }
    if profile.avatar_source == AvatarSource::Google {
        query = query.value("provider_picture_url", provider_picture_url);
    }
    query.where_eq("user_id", user_id).execute(db).await?;
    Ok(())
}

/// Loads one public profile.
///
/// # Errors
///
/// * Returns [`ProfileError::Malformed`] for invalid persisted source values.
/// * Returns [`ProfileError::Database`] when persistence fails.
pub async fn load_profile(
    db: &dyn Database,
    user_id: &str,
) -> Result<Option<UserProfile>, ProfileError> {
    let rows = db
        .select("user_profiles")
        .where_eq("user_id", user_id)
        .execute(db)
        .await?;
    rows.first()
        .map(|row| {
            Ok(UserProfile {
                user_id: string_column(row, "user_id")?,
                display_name: string_column(row, "display_name")?,
                display_name_source: match string_column(row, "display_name_source")?.as_str() {
                    "GOOGLE" => ProfileFieldSource::Google,
                    "CUSTOM" => ProfileFieldSource::Custom,
                    _ => return Err(ProfileError::Malformed),
                },
                avatar_source: match string_column(row, "avatar_source")?.as_str() {
                    "GOOGLE" => AvatarSource::Google,
                    "CUSTOM" => AvatarSource::Custom,
                    "CUSTOM_NONE" => AvatarSource::CustomNone,
                    _ => return Err(ProfileError::Malformed),
                },
                provider_picture_url: optional_string(row, "provider_picture_url"),
            })
        })
        .transpose()
}

/// Replaces the display name and opts out of provider synchronization.
///
/// # Errors
///
/// * Returns validation or database failures.
pub async fn set_custom_display_name(
    db: &dyn Database,
    user_id: &str,
    display_name: &str,
    now: OffsetDateTime,
) -> Result<(), ProfileError> {
    db.update("user_profiles")
        .value("display_name", normalize_display_name(display_name)?)
        .value("display_name_source", ProfileFieldSource::Custom.as_str())
        .value("updated_at_ms", timestamp_ms(now)?)
        .where_eq("user_id", user_id)
        .execute(db)
        .await?;
    Ok(())
}

/// Marks the avatar intentionally absent so provider login cannot restore it.
///
/// # Errors
///
/// * Returns database or timestamp failures.
pub async fn remove_custom_avatar(
    db: &dyn Database,
    user_id: &str,
    now: OffsetDateTime,
) -> Result<(), ProfileError> {
    let tx = db.begin_transaction().await?;
    tx.delete("user_profile_images")
        .where_eq("user_id", user_id)
        .execute(&*tx)
        .await?;
    tx.update("user_profiles")
        .value("avatar_source", AvatarSource::CustomNone.as_str())
        .value("updated_at_ms", timestamp_ms(now)?)
        .where_eq("user_id", user_id)
        .execute(&*tx)
        .await?;
    tx.commit().await?;
    Ok(())
}

fn string_column(row: &switchy_database::Row, name: &str) -> Result<String, ProfileError> {
    row.get(name)
        .and_then(|value| value.as_str().map(ToOwned::to_owned))
        .ok_or(ProfileError::Malformed)
}

fn optional_string(row: &switchy_database::Row, name: &str) -> Option<String> {
    row.get(name)
        .and_then(|value| value.as_str().map(ToOwned::to_owned))
}

fn timestamp_ms(timestamp: OffsetDateTime) -> Result<i64, ProfileError> {
    i64::try_from(timestamp.unix_timestamp_nanos() / 1_000_000).map_err(|_| ProfileError::Timestamp)
}

/// Profile validation or persistence failure.
#[derive(Debug, Error)]
pub enum ProfileError {
    #[error("display name is invalid")]
    InvalidDisplayName,
    #[error("could not generate a collision-free challenge handle")]
    HandleCollision,
    #[error("profile row is malformed")]
    Malformed,
    #[error("profile timestamp is outside the supported range")]
    Timestamp,
    #[error(transparent)]
    Database(#[from] switchy_database::DatabaseError),
}

#[cfg(test)]
mod tests {
    use futures_lite::future::block_on;

    use super::*;
    use crate::{migrate_app, register};

    #[test]
    fn display_names_are_unicode_normalized_and_safe() {
        assert_eq!(
            normalize_display_name("  Ada   Lovelace ").unwrap(),
            "Ada Lovelace"
        );
        assert_eq!(normalize_display_name(" 李 雷 ").unwrap(), "李 雷");
        assert_eq!(normalize_display_name("  ").unwrap(), "Player");
        assert!(normalize_display_name("bad\nname").is_err());
        assert!(normalize_display_name(&"x".repeat(81)).is_err());
    }

    #[test]
    fn generated_handles_are_stable_format_and_unique() {
        block_on(async {
            let db = switchy_database_connection::builder()
                .turso()
                .with_in_memory()
                .build()
                .await
                .unwrap();
            migrate_app(&*db).await.unwrap();
            let first = generate_unique_handle(&*db, "Ada Lovelace").await.unwrap();
            assert!(first.starts_with("ada-lovelace-"));
            assert!(first.len() <= MAX_HANDLE_CHARS);
            let unicode = generate_unique_handle(&*db, "李雷").await.unwrap();
            assert!(unicode.starts_with("player-"));
        });
    }

    #[test]
    fn google_sync_stops_after_customization() {
        block_on(async {
            let db = switchy_database_connection::builder()
                .turso()
                .with_in_memory()
                .build()
                .await
                .unwrap();
            migrate_app(&*db).await.unwrap();
            let user = register(
                &*db,
                "ada",
                "correct horse battery staple",
                OffsetDateTime::UNIX_EPOCH,
            )
            .await
            .unwrap();
            create_google_profile(
                &*db,
                &user,
                "Ada Google",
                Some("https://lh3.googleusercontent.com/photo"),
                OffsetDateTime::UNIX_EPOCH,
            )
            .await
            .unwrap();
            synchronize_google_profile(
                &*db,
                &user,
                "Ada Changed",
                Some("https://lh3.googleusercontent.com/new"),
                OffsetDateTime::UNIX_EPOCH,
            )
            .await
            .unwrap();
            assert_eq!(
                load_profile(&*db, &user)
                    .await
                    .unwrap()
                    .unwrap()
                    .display_name,
                "Ada Changed"
            );
            set_custom_display_name(&*db, &user, "Countess Ada", OffsetDateTime::UNIX_EPOCH)
                .await
                .unwrap();
            synchronize_google_profile(
                &*db,
                &user,
                "Overwritten",
                None,
                OffsetDateTime::UNIX_EPOCH,
            )
            .await
            .unwrap();
            assert_eq!(
                load_profile(&*db, &user)
                    .await
                    .unwrap()
                    .unwrap()
                    .display_name,
                "Countess Ada"
            );
            remove_custom_avatar(&*db, &user, OffsetDateTime::UNIX_EPOCH)
                .await
                .unwrap();
            assert_eq!(
                load_profile(&*db, &user)
                    .await
                    .unwrap()
                    .unwrap()
                    .avatar_source,
                AvatarSource::CustomNone
            );
        });
    }
}

//! Renderer-neutral public account profiles and stable generated challenge handles.

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use image::{ImageFormat, ImageReader};
use rand_core::{OsRng, RngCore as _};
use sha2::{Digest as _, Sha256};
use std::{collections::BTreeSet, io::Cursor};
use switchy_database::{Database, query::FilterableQuery as _};
use thiserror::Error;
use time::OffsetDateTime;

const MAX_DISPLAY_NAME_CHARS: usize = 80;
const HANDLE_SUFFIX_BYTES: usize = 4;
const HANDLE_SUFFIX_CHARS: usize = HANDLE_SUFFIX_BYTES * 2;
const MAX_HANDLE_CHARS: usize = 32;
const MAX_HANDLE_ATTEMPTS: usize = 8;
const AVATAR_SIZE: u32 = 128;
const MAX_AVATAR_INPUT_BYTES: usize = 2 * 1024 * 1024;
const MAX_AVATAR_PIXELS: u64 = 16_000_000;
const GOOGLE_AVATAR_HOSTS: &[&str] = &["lh3.googleusercontent.com"];

/// Downloads a Google profile picture with strict host, redirect, timeout, and byte bounds.
///
/// # Errors
///
/// * Returns [`ProfileError::InvalidImageUrl`] unless the URL is HTTPS on the explicit Google
///   avatar host allowlist.
/// * Returns [`ProfileError::InvalidImage`] for unsuccessful, oversized, or malformed responses.
/// * Returns [`ProfileError::Http`] for transport failures.
pub async fn download_google_avatar(
    picture_url: &str,
    timeout: std::time::Duration,
) -> Result<Vec<u8>, ProfileError> {
    let url = reqwest::Url::parse(picture_url).map_err(|_| ProfileError::InvalidImageUrl)?;
    if url.scheme() != "https"
        || !GOOGLE_AVATAR_HOSTS.contains(&url.host_str().unwrap_or_default())
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err(ProfileError::InvalidImageUrl);
    }
    let client = reqwest::Client::builder()
        .connect_timeout(timeout)
        .timeout(timeout)
        .redirect(reqwest::redirect::Policy::none())
        .build()?;
    let mut response = client.get(url).send().await?;
    if !response.status().is_success() {
        return Err(ProfileError::InvalidImage);
    }
    if let Some(length) = response.content_length()
        && length > u64::try_from(MAX_AVATAR_INPUT_BYTES).unwrap_or(u64::MAX)
    {
        return Err(ProfileError::InvalidImage);
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        if bytes.len().saturating_add(chunk.len()) > MAX_AVATAR_INPUT_BYTES {
            return Err(ProfileError::InvalidImage);
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

/// Normalized profile image stored without source metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileImage {
    pub content_type: String,
    pub bytes: Vec<u8>,
    pub content_sha256: String,
    pub width: u32,
    pub height: u32,
}

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

/// Restores provider synchronization for the display name.
///
/// The currently displayed value is retained until the next verified Google login refreshes it.
///
/// # Errors
///
/// * Returns database or timestamp failures.
pub async fn use_google_display_name(
    db: &dyn Database,
    user_id: &str,
    now: OffsetDateTime,
) -> Result<(), ProfileError> {
    db.update("user_profiles")
        .value("display_name_source", ProfileFieldSource::Google.as_str())
        .value("updated_at_ms", timestamp_ms(now)?)
        .where_eq("user_id", user_id)
        .execute(db)
        .await?;
    Ok(())
}

/// Restores provider synchronization for the avatar.
///
/// A prior normalized mirror is retained; a later verified Google login refreshes it.
///
/// # Errors
///
/// * Returns database or timestamp failures.
pub async fn use_google_avatar(
    db: &dyn Database,
    user_id: &str,
    now: OffsetDateTime,
) -> Result<(), ProfileError> {
    db.update("user_profiles")
        .value("avatar_source", AvatarSource::Google.as_str())
        .value("provider_picture_checked_at_ms", Option::<i64>::None)
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

/// Validates, bounds, resizes, and metadata-strips an avatar into a square PNG.
///
/// # Errors
///
/// * Returns [`ProfileError::InvalidImage`] for unsupported, oversized, malformed, or
///   allocation-risking input.
pub fn normalize_avatar(bytes: &[u8]) -> Result<ProfileImage, ProfileError> {
    if bytes.is_empty() || bytes.len() > MAX_AVATAR_INPUT_BYTES {
        return Err(ProfileError::InvalidImage);
    }
    let reader = ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|_| ProfileError::InvalidImage)?;
    if !matches!(
        reader.format(),
        Some(ImageFormat::Jpeg | ImageFormat::Png | ImageFormat::WebP)
    ) {
        return Err(ProfileError::InvalidImage);
    }
    let image = reader.decode().map_err(|_| ProfileError::InvalidImage)?;
    if u64::from(image.width()) * u64::from(image.height()) > MAX_AVATAR_PIXELS {
        return Err(ProfileError::InvalidImage);
    }
    let image = image.resize_to_fill(
        AVATAR_SIZE,
        AVATAR_SIZE,
        image::imageops::FilterType::Lanczos3,
    );
    let mut output = Vec::new();
    image
        .write_to(&mut Cursor::new(&mut output), ImageFormat::Png)
        .map_err(|_| ProfileError::InvalidImage)?;
    let content_sha256 =
        Sha256::digest(&output)
            .iter()
            .fold(String::with_capacity(64), |mut encoded, byte| {
                use std::fmt::Write as _;
                write!(encoded, "{byte:02x}").expect("writing to String is infallible");
                encoded
            });
    Ok(ProfileImage {
        content_type: "image/png".to_string(),
        bytes: output,
        content_sha256,
        width: AVATAR_SIZE,
        height: AVATAR_SIZE,
    })
}

/// Stores a normalized custom avatar and opts out of provider synchronization.
///
/// # Errors
///
/// * Returns image validation, timestamp, or database failures.
pub async fn set_custom_avatar(
    db: &dyn Database,
    user_id: &str,
    bytes: &[u8],
    now: OffsetDateTime,
) -> Result<ProfileImage, ProfileError> {
    let image = normalize_avatar(bytes)?;
    store_profile_image(db, user_id, &image, AvatarSource::Custom, now).await?;
    Ok(image)
}

/// Stores a normalized provider avatar only while the avatar remains Google-owned.
///
/// # Errors
///
/// * Returns image validation, malformed profile, timestamp, or database failures.
pub async fn set_google_avatar(
    db: &dyn Database,
    user_id: &str,
    bytes: &[u8],
    now: OffsetDateTime,
) -> Result<Option<ProfileImage>, ProfileError> {
    let profile = load_profile(db, user_id)
        .await?
        .ok_or(ProfileError::Malformed)?;
    if profile.avatar_source != AvatarSource::Google {
        return Ok(None);
    }
    let image = normalize_avatar(bytes)?;
    store_profile_image(db, user_id, &image, AvatarSource::Google, now).await?;
    Ok(Some(image))
}

/// Loads the current normalized avatar content hash.
///
/// # Errors
///
/// * Returns malformed persisted image or database failures.
pub async fn profile_image_hash(
    db: &dyn Database,
    user_id: &str,
) -> Result<Option<String>, ProfileError> {
    let rows = db
        .select("user_profile_images")
        .where_eq("user_id", user_id)
        .execute(db)
        .await?;
    rows.first()
        .map(|row| string_column(row, "content_sha256"))
        .transpose()
}

/// Determines whether a signed-in viewer may load another user's avatar.
///
/// Avatars are private profile data. Access is limited to the owner, a current game opponent, or
/// either side of a pending challenge. The function deliberately returns only a boolean so callers
/// can use the same not-found response for missing and unauthorized images.
///
/// # Errors
///
/// * Returns database failures while resolving product relationships.
pub async fn can_view_profile_avatar(
    db: &dyn Database,
    viewer_user_id: &str,
    profile_user_id: &str,
) -> Result<bool, ProfileError> {
    if viewer_user_id == profile_user_id {
        return Ok(true);
    }

    let viewer_games = db
        .select("game_players")
        .where_eq("user_id", viewer_user_id)
        .execute(db)
        .await?
        .iter()
        .filter_map(|row| optional_string(row, "game_id"))
        .collect::<BTreeSet<_>>();
    if !viewer_games.is_empty()
        && db
            .select("game_players")
            .where_eq("user_id", profile_user_id)
            .execute(db)
            .await?
            .iter()
            .filter_map(|row| optional_string(row, "game_id"))
            .any(|game_id| viewer_games.contains(&game_id))
    {
        return Ok(true);
    }

    let outgoing = db
        .select("challenges")
        .where_eq("challenger_user_id", viewer_user_id)
        .where_eq("challenged_user_id", profile_user_id)
        .where_eq("status", "PENDING")
        .execute(db)
        .await?;
    if !outgoing.is_empty() {
        return Ok(true);
    }
    Ok(!db
        .select("challenges")
        .where_eq("challenger_user_id", profile_user_id)
        .where_eq("challenged_user_id", viewer_user_id)
        .where_eq("status", "PENDING")
        .execute(db)
        .await?
        .is_empty())
}

/// Loads the normalized avatar when its requested content hash is current.
///
/// # Errors
///
/// * Returns malformed persisted image or database failures.
pub async fn load_profile_image(
    db: &dyn Database,
    user_id: &str,
    expected_hash: &str,
) -> Result<Option<ProfileImage>, ProfileError> {
    let rows = db
        .select("user_profile_images")
        .where_eq("user_id", user_id)
        .where_eq("content_sha256", expected_hash)
        .execute(db)
        .await?;
    rows.first()
        .map(|row| {
            let content_type = string_column(row, "content_type")?;
            if content_type != "image/png" {
                return Err(ProfileError::Malformed);
            }
            let bytes = BASE64
                .decode(string_column(row, "content_base64")?)
                .map_err(|_| ProfileError::Malformed)?;
            let width = u32::try_from(integer_column(row, "width")?)
                .map_err(|_| ProfileError::Malformed)?;
            let height = u32::try_from(integer_column(row, "height")?)
                .map_err(|_| ProfileError::Malformed)?;
            Ok(ProfileImage {
                content_type,
                bytes,
                content_sha256: string_column(row, "content_sha256")?,
                width,
                height,
            })
        })
        .transpose()
}

async fn store_profile_image(
    db: &dyn Database,
    user_id: &str,
    image: &ProfileImage,
    source: AvatarSource,
    now: OffsetDateTime,
) -> Result<(), ProfileError> {
    let now_ms = timestamp_ms(now)?;
    let tx = db.begin_transaction().await?;
    tx.upsert("user_profile_images")
        .where_eq("user_id", user_id)
        .value("user_id", user_id)
        .value("content_type", image.content_type.clone())
        .value("content_base64", BASE64.encode(&image.bytes))
        .value("content_sha256", image.content_sha256.clone())
        .value("width", i64::from(image.width))
        .value("height", i64::from(image.height))
        .value("updated_at_ms", now_ms)
        .execute(&*tx)
        .await?;
    tx.update("user_profiles")
        .value("avatar_source", source.as_str())
        .value("provider_picture_checked_at_ms", now_ms)
        .value("updated_at_ms", now_ms)
        .where_eq("user_id", user_id)
        .execute(&*tx)
        .await?;
    tx.commit().await?;
    Ok(())
}

fn integer_column(row: &switchy_database::Row, name: &str) -> Result<i64, ProfileError> {
    row.get(name)
        .and_then(|value| value.as_i64())
        .ok_or(ProfileError::Malformed)
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
    #[error("profile image URL is not an allowed HTTPS Google avatar URL")]
    InvalidImageUrl,
    #[error("profile image is invalid or exceeds safety limits")]
    InvalidImage,
    #[error(transparent)]
    Http(#[from] reqwest::Error),
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
    use crate::{create_challenge, migrate_app, register};

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
    fn avatar_visibility_is_limited_to_product_relationships() {
        block_on(async {
            let db = switchy_database_connection::builder()
                .turso()
                .with_in_memory()
                .build()
                .await
                .unwrap();
            migrate_app(&*db).await.unwrap();
            let now = OffsetDateTime::UNIX_EPOCH;
            let alice = register(&*db, "alice-profile", "correct horse battery staple", now)
                .await
                .unwrap();
            let bob = register(&*db, "bob-profile", "correct horse battery staple", now)
                .await
                .unwrap();
            let stranger = register(
                &*db,
                "stranger-profile",
                "correct horse battery staple",
                now,
            )
            .await
            .unwrap();

            assert!(can_view_profile_avatar(&*db, &alice, &alice).await.unwrap());
            assert!(!can_view_profile_avatar(&*db, &alice, &bob).await.unwrap());
            create_challenge(&*db, &alice, &bob, now).await.unwrap();
            assert!(can_view_profile_avatar(&*db, &alice, &bob).await.unwrap());
            assert!(can_view_profile_avatar(&*db, &bob, &alice).await.unwrap());
            assert!(
                !can_view_profile_avatar(&*db, &stranger, &alice)
                    .await
                    .unwrap()
            );
        });
    }

    #[test]
    fn avatars_are_bounded_normalized_and_round_trip_from_storage() {
        block_on(async {
            let mut source = image::RgbaImage::new(4, 2);
            for pixel in source.pixels_mut() {
                *pixel = image::Rgba([20, 40, 60, 255]);
            }
            let mut input = Vec::new();
            image::DynamicImage::ImageRgba8(source)
                .write_to(&mut Cursor::new(&mut input), ImageFormat::Png)
                .unwrap();
            let normalized = normalize_avatar(&input).unwrap();
            assert_eq!((normalized.width, normalized.height), (128, 128));
            assert_eq!(normalized.content_type, "image/png");
            assert!(normalize_avatar(&[]).is_err());
            assert!(normalize_avatar(&vec![0; MAX_AVATAR_INPUT_BYTES + 1]).is_err());

            let db = switchy_database_connection::builder()
                .turso()
                .with_in_memory()
                .build()
                .await
                .unwrap();
            migrate_app(&*db).await.unwrap();
            let user = register(
                &*db,
                "avatar-user",
                "correct horse battery staple",
                OffsetDateTime::UNIX_EPOCH,
            )
            .await
            .unwrap();
            create_google_profile(&*db, &user, "Avatar User", None, OffsetDateTime::UNIX_EPOCH)
                .await
                .unwrap();
            let stored = set_custom_avatar(&*db, &user, &input, OffsetDateTime::UNIX_EPOCH)
                .await
                .unwrap();
            let loaded = load_profile_image(&*db, &user, &stored.content_sha256)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(loaded, stored);
            assert_eq!(
                load_profile(&*db, &user)
                    .await
                    .unwrap()
                    .unwrap()
                    .avatar_source,
                AvatarSource::Custom
            );
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

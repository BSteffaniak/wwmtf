//! Durable private multiplayer lobby lifecycle and creation policy.

use std::fmt::Write as _;

use sha2::{Digest as _, Sha256};
use switchy_database::{
    Database,
    query::{FilterableQuery as _, SortDirection},
};
use thiserror::Error;
use time::{Duration, OffsetDateTime};
use uuid::Uuid;
use wwmtf_game_domain::{GameId, generated_rule_profile};

/// Deployment-owned limits for accepting new game configurations.
///
/// These limits never participate in replay. A started game carries all canonical state needed to
/// recover even when a later deployment changes this policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GameCreationPolicy {
    pub max_players: usize,
    pub max_board_size: u8,
    pub max_tile_sets: u8,
}

impl GameCreationPolicy {
    /// Creates validated resource policy.
    ///
    /// # Errors
    ///
    /// Returns [`LobbyError::InvalidPolicy`] when any limit cannot admit a valid game.
    pub const fn new(
        max_players: usize,
        max_board_size: u8,
        max_tile_sets: u8,
    ) -> Result<Self, LobbyError> {
        if max_players < 2 || max_board_size == 0 || max_tile_sets == 0 {
            return Err(LobbyError::InvalidPolicy);
        }
        Ok(Self {
            max_players,
            max_board_size,
            max_tile_sets,
        })
    }

    #[allow(clippy::suspicious_operation_groupings)]
    const fn validate(self, settings: &LobbySettings) -> Result<(), LobbyError> {
        if settings.max_players < 2
            || settings.max_players > self.max_players
            || settings.board_size == 0
            || settings.board_size > self.max_board_size
            || settings.tile_set_count == 0
            || settings.tile_set_count > self.max_tile_sets
        {
            return Err(LobbyError::InvalidSettings);
        }
        Ok(())
    }
}

/// Creator-selected first-turn behavior.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FirstPlayerPolicy {
    Random,
    Creator,
    Chosen(String),
}

impl FirstPlayerPolicy {
    const fn as_str(&self) -> &'static str {
        match self {
            Self::Random => "RANDOM",
            Self::Creator => "CREATOR",
            Self::Chosen(_) => "CHOSEN",
        }
    }
}

/// Settings visible to every lobby member before start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LobbySettings {
    pub max_players: usize,
    pub board_size: u8,
    pub tile_set_count: u8,
    pub first_player: FirstPlayerPolicy,
}

/// Raw secret returned once to the creator for link transport.
#[derive(Clone, PartialEq, Eq)]
pub struct LobbyInvitationToken(String);

impl std::fmt::Debug for LobbyInvitationToken {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("LobbyInvitationToken([REDACTED])")
    }
}

impl LobbyInvitationToken {
    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }
}

/// Public lobby member in durable seat order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LobbyMember {
    pub user_id: String,
    pub seat: usize,
}

/// Authorized durable lobby view without its invitation secret.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GameLobby {
    pub lobby_id: String,
    pub creator_user_id: String,
    pub status: String,
    pub revision: u64,
    pub settings: LobbySettings,
    pub members: Vec<LobbyMember>,
    pub started_game_id: Option<GameId>,
}

/// Creates an open private lobby and creator membership atomically.
///
/// # Errors
///
/// Returns validation, timestamp, serialization, collision, or database failures.
pub async fn create_lobby(
    db: &dyn Database,
    creator_user_id: &str,
    settings: LobbySettings,
    policy: GameCreationPolicy,
    now: OffsetDateTime,
    invitation_lifetime: Duration,
) -> Result<(String, LobbyInvitationToken), LobbyError> {
    policy.validate(&settings)?;
    if let FirstPlayerPolicy::Chosen(user_id) = &settings.first_player
        && user_id != creator_user_id
    {
        return Err(LobbyError::InvalidSettings);
    }
    let expires = now
        .checked_add(invitation_lifetime)
        .ok_or(LobbyError::Timestamp)?;
    let now_ms = timestamp_ms(now)?;
    let expires_ms = timestamp_ms(expires)?;
    for _ in 0..4 {
        let lobby_id = Uuid::new_v4().to_string();
        let token = LobbyInvitationToken(format!("{}{}", Uuid::new_v4(), Uuid::new_v4()));
        let token_hash = token_hash(token.expose());
        if !db
            .select("game_lobbies")
            .where_eq("invitation_token_hash", token_hash.clone())
            .execute(db)
            .await?
            .is_empty()
        {
            continue;
        }
        let tx = db.begin_transaction().await?;
        tx.insert("game_lobbies")
            .value("lobby_id", lobby_id.clone())
            .value("creator_user_id", creator_user_id)
            .value("status", "OPEN")
            .value("revision", 1_i64)
            .value("max_players", usize_i64(settings.max_players)?)
            .value("board_size", i64::from(settings.board_size))
            .value("tile_set_count", i64::from(settings.tile_set_count))
            .value("first_player_policy", settings.first_player.as_str())
            .value(
                "chosen_first_user_id",
                match &settings.first_player {
                    FirstPlayerPolicy::Chosen(user) => Some(user.clone()),
                    _ => None,
                },
            )
            .value("invitation_token_hash", token_hash)
            .value("invitation_expires_at_ms", expires_ms)
            .value("started_game_id", Option::<String>::None)
            .value("created_at_ms", now_ms)
            .value("updated_at_ms", now_ms)
            .execute(&*tx)
            .await?;
        insert_member(&*tx, &lobby_id, creator_user_id, 0, now_ms).await?;
        tx.commit().await?;
        return Ok((lobby_id, token));
    }
    Err(LobbyError::Collision)
}

/// Joins an open lobby through its secret token.
///
/// # Errors
///
/// Returns [`LobbyError::Unavailable`] for invalid, expired, duplicate, closed, or full lobbies.
pub async fn join_lobby(
    db: &dyn Database,
    token: &str,
    user_id: &str,
    policy: GameCreationPolicy,
    now: OffsetDateTime,
) -> Result<String, LobbyError> {
    let tx = db.begin_transaction().await?;
    let rows = tx
        .select("game_lobbies")
        .where_eq("invitation_token_hash", token_hash(token))
        .where_eq("status", "OPEN")
        .execute(&*tx)
        .await?;
    let row = rows.first().ok_or(LobbyError::Unavailable)?;
    let lobby_id = string(row, "lobby_id")?;
    let settings = settings(row)?;
    policy.validate(&settings)?;
    if signed(row, "invitation_expires_at_ms")? <= timestamp_ms(now)? {
        return Err(LobbyError::Unavailable);
    }
    let members = member_rows(&*tx, &lobby_id).await?;
    if members.len() >= settings.max_players
        || members.iter().any(|member| member.user_id == user_id)
    {
        return Err(LobbyError::Unavailable);
    }
    insert_member(&*tx, &lobby_id, user_id, members.len(), timestamp_ms(now)?).await?;
    let updated = tx
        .update("game_lobbies")
        .value(
            "revision",
            signed(row, "revision")?
                .checked_add(1)
                .ok_or(LobbyError::Invalid)?,
        )
        .value("updated_at_ms", timestamp_ms(now)?)
        .where_eq("lobby_id", lobby_id.clone())
        .where_eq("status", "OPEN")
        .where_eq("revision", signed(row, "revision")?)
        .execute(&*tx)
        .await?;
    if updated.len() != 1 {
        return Err(LobbyError::Conflict);
    }
    tx.commit().await?;
    Ok(lobby_id)
}

/// Joins an open lobby through its unguessable durable lobby identity.
///
/// # Errors
///
/// Returns [`LobbyError::Unavailable`] for unknown, expired, duplicate, closed, or full lobbies.
pub async fn join_lobby_by_id(
    db: &dyn Database,
    lobby_id: &str,
    user_id: &str,
    policy: GameCreationPolicy,
    now: OffsetDateTime,
) -> Result<String, LobbyError> {
    let tx = db.begin_transaction().await?;
    let rows = tx
        .select("game_lobbies")
        .where_eq("lobby_id", lobby_id)
        .where_eq("status", "OPEN")
        .execute(&*tx)
        .await?;
    let row = rows.first().ok_or(LobbyError::Unavailable)?;
    let settings = settings(row)?;
    policy.validate(&settings)?;
    if signed(row, "invitation_expires_at_ms")? <= timestamp_ms(now)? {
        return Err(LobbyError::Unavailable);
    }
    let members = member_rows(&*tx, lobby_id).await?;
    if members.len() >= settings.max_players
        || members.iter().any(|member| member.user_id == user_id)
    {
        return Err(LobbyError::Unavailable);
    }
    insert_member(&*tx, lobby_id, user_id, members.len(), timestamp_ms(now)?).await?;
    let revision = signed(row, "revision")?;
    let updated = tx
        .update("game_lobbies")
        .value(
            "revision",
            revision.checked_add(1).ok_or(LobbyError::Invalid)?,
        )
        .value("updated_at_ms", timestamp_ms(now)?)
        .where_eq("lobby_id", lobby_id)
        .where_eq("status", "OPEN")
        .where_eq("revision", revision)
        .execute(&*tx)
        .await?;
    if updated.len() != 1 {
        return Err(LobbyError::Conflict);
    }
    tx.commit().await?;
    Ok(lobby_id.to_string())
}

/// Loads a lobby for one current member.
///
/// # Errors
///
/// Returns unavailable or malformed persistence errors.
pub async fn load_lobby(
    db: &dyn Database,
    lobby_id: &str,
    user_id: &str,
) -> Result<GameLobby, LobbyError> {
    let members = member_rows(db, lobby_id).await?;
    if !members.iter().any(|member| member.user_id == user_id) {
        return Err(LobbyError::Unavailable);
    }
    let rows = db
        .select("game_lobbies")
        .where_eq("lobby_id", lobby_id)
        .execute(db)
        .await?;
    let row = rows.first().ok_or(LobbyError::Unavailable)?;
    Ok(GameLobby {
        lobby_id: lobby_id.to_string(),
        creator_user_id: string(row, "creator_user_id")?,
        status: string(row, "status")?,
        revision: unsigned(row, "revision")?,
        settings: settings(row)?,
        members,
        started_game_id: optional_string(row, "started_game_id")?
            .map(|id| id.parse())
            .transpose()
            .map_err(|_| LobbyError::Invalid)?,
    })
}

/// Updates settings for an open lobby as its creator.
///
/// # Errors
///
/// Returns authorization, policy, membership, conflict, or persistence failures.
pub async fn update_lobby_settings(
    db: &dyn Database,
    lobby_id: &str,
    creator_user_id: &str,
    settings: LobbySettings,
    policy: GameCreationPolicy,
    now: OffsetDateTime,
) -> Result<(), LobbyError> {
    policy.validate(&settings)?;
    let tx = db.begin_transaction().await?;
    let rows = tx
        .select("game_lobbies")
        .where_eq("lobby_id", lobby_id)
        .where_eq("status", "OPEN")
        .execute(&*tx)
        .await?;
    let row = rows.first().ok_or(LobbyError::Unavailable)?;
    if string(row, "creator_user_id")? != creator_user_id {
        return Err(LobbyError::Unauthorized);
    }
    if let FirstPlayerPolicy::Chosen(user_id) = &settings.first_player
        && !member_rows(&*tx, lobby_id)
            .await?
            .iter()
            .any(|member| &member.user_id == user_id)
    {
        return Err(LobbyError::NotReady);
    }
    let revision = signed(row, "revision")?;
    let updated = tx
        .update("game_lobbies")
        .value("max_players", usize_i64(settings.max_players)?)
        .value("board_size", i64::from(settings.board_size))
        .value("tile_set_count", i64::from(settings.tile_set_count))
        .value("first_player_policy", settings.first_player.as_str())
        .value(
            "chosen_first_user_id",
            match settings.first_player {
                FirstPlayerPolicy::Chosen(user_id) => Some(user_id),
                FirstPlayerPolicy::Random | FirstPlayerPolicy::Creator => None,
            },
        )
        .value(
            "revision",
            revision.checked_add(1).ok_or(LobbyError::Invalid)?,
        )
        .value("updated_at_ms", timestamp_ms(now)?)
        .where_eq("lobby_id", lobby_id)
        .where_eq("status", "OPEN")
        .where_eq("revision", revision)
        .execute(&*tx)
        .await?;
    if updated.len() != 1 {
        return Err(LobbyError::Conflict);
    }
    tx.commit().await?;
    Ok(())
}

/// Cancels an open lobby as its creator, revoking its join link.
///
/// # Errors
///
/// Returns authorization, conflict, or persistence failures.
pub async fn cancel_lobby(
    db: &dyn Database,
    lobby_id: &str,
    creator_user_id: &str,
    now: OffsetDateTime,
) -> Result<(), LobbyError> {
    let tx = db.begin_transaction().await?;
    let rows = tx
        .select("game_lobbies")
        .where_eq("lobby_id", lobby_id)
        .where_eq("status", "OPEN")
        .execute(&*tx)
        .await?;
    let row = rows.first().ok_or(LobbyError::Unavailable)?;
    if string(row, "creator_user_id")? != creator_user_id {
        return Err(LobbyError::Unauthorized);
    }
    let revision = signed(row, "revision")?;
    let updated = tx
        .update("game_lobbies")
        .value("status", "CANCELLED")
        .value(
            "revision",
            revision.checked_add(1).ok_or(LobbyError::Invalid)?,
        )
        .value("updated_at_ms", timestamp_ms(now)?)
        .where_eq("lobby_id", lobby_id)
        .where_eq("status", "OPEN")
        .where_eq("revision", revision)
        .execute(&*tx)
        .await?;
    if updated.len() != 1 {
        return Err(LobbyError::Conflict);
    }
    tx.commit().await?;
    Ok(())
}

/// Removes a non-creator from an open lobby and compacts durable seat order.
///
/// # Errors
///
/// Returns authorization, conflict, or persistence failures.
pub async fn leave_lobby(
    db: &dyn Database,
    lobby_id: &str,
    user_id: &str,
    now: OffsetDateTime,
) -> Result<(), LobbyError> {
    let tx = db.begin_transaction().await?;
    let rows = tx
        .select("game_lobbies")
        .where_eq("lobby_id", lobby_id)
        .where_eq("status", "OPEN")
        .execute(&*tx)
        .await?;
    let row = rows.first().ok_or(LobbyError::Unavailable)?;
    if string(row, "creator_user_id")? == user_id {
        return Err(LobbyError::Unauthorized);
    }
    let removed = tx
        .delete("game_lobby_members")
        .where_eq("lobby_id", lobby_id)
        .where_eq("user_id", user_id)
        .execute(&*tx)
        .await?;
    if removed.len() != 1 {
        return Err(LobbyError::Unavailable);
    }
    let members = member_rows(&*tx, lobby_id).await?;
    for (seat, member) in members.iter().enumerate() {
        tx.update("game_lobby_members")
            .value(
                "seat",
                -i64::try_from(seat).map_err(|_| LobbyError::Invalid)? - 1,
            )
            .where_eq("lobby_id", lobby_id)
            .where_eq("user_id", member.user_id.clone())
            .execute(&*tx)
            .await?;
    }
    for (seat, member) in members.iter().enumerate() {
        tx.update("game_lobby_members")
            .value("seat", usize_i64(seat)?)
            .where_eq("lobby_id", lobby_id)
            .where_eq("user_id", member.user_id.clone())
            .execute(&*tx)
            .await?;
    }
    let revision = signed(row, "revision")?;
    let updated = tx
        .update("game_lobbies")
        .value(
            "revision",
            revision.checked_add(1).ok_or(LobbyError::Invalid)?,
        )
        .value("updated_at_ms", timestamp_ms(now)?)
        .where_eq("lobby_id", lobby_id)
        .where_eq("status", "OPEN")
        .where_eq("revision", revision)
        .execute(&*tx)
        .await?;
    if updated.len() != 1 {
        return Err(LobbyError::Conflict);
    }
    tx.commit().await?;
    Ok(())
}

/// Starts an open lobby exactly once and returns the canonical game.
///
/// # Errors
///
/// Returns authorization, readiness, conflict, game-creation, or persistence failures.
pub async fn start_lobby(
    db: &dyn Database,
    lobby_id: &str,
    creator_user_id: &str,
    policy: GameCreationPolicy,
    now: OffsetDateTime,
    shuffle_seed: u64,
) -> Result<GameId, LobbyError> {
    let tx = db.begin_transaction().await?;
    let rows = tx
        .select("game_lobbies")
        .where_eq("lobby_id", lobby_id)
        .execute(&*tx)
        .await?;
    let row = rows.first().ok_or(LobbyError::Unavailable)?;
    if string(row, "creator_user_id")? != creator_user_id {
        return Err(LobbyError::Unauthorized);
    }
    if string(row, "status")? == "STARTED" {
        return optional_string(row, "started_game_id")?
            .ok_or(LobbyError::Invalid)?
            .parse()
            .map_err(|_| LobbyError::Invalid);
    }
    if string(row, "status")? != "OPEN" {
        return Err(LobbyError::Unavailable);
    }
    let settings = settings(row)?;
    policy.validate(&settings)?;
    let members = member_rows(&*tx, lobby_id).await?;
    if members.len() < 2 {
        return Err(LobbyError::NotReady);
    }
    let user_ids = members
        .iter()
        .map(|member| member.user_id.clone())
        .collect::<Vec<_>>();
    let first_index = match settings.first_player {
        FirstPlayerPolicy::Creator => members
            .iter()
            .position(|member| member.user_id == creator_user_id),
        FirstPlayerPolicy::Chosen(user_id) => {
            members.iter().position(|member| member.user_id == user_id)
        }
        FirstPlayerPolicy::Random => Some(scale_seed(shuffle_seed, members.len())),
    }
    .ok_or(LobbyError::NotReady)?;
    let rules = generated_rule_profile(settings.board_size, settings.tile_set_count)?;
    let game_id = crate::create_game_for_users_with_rules_in_transaction(
        &*tx,
        &user_ids,
        first_index,
        &rules,
        now,
        shuffle_seed,
        &format!("start-lobby:{lobby_id}"),
    )
    .await?;
    let revision = signed(row, "revision")?;
    let updated = tx
        .update("game_lobbies")
        .value("status", "STARTED")
        .value(
            "revision",
            revision.checked_add(1).ok_or(LobbyError::Invalid)?,
        )
        .value("started_game_id", game_id.to_string())
        .value("updated_at_ms", timestamp_ms(now)?)
        .where_eq("lobby_id", lobby_id)
        .where_eq("status", "OPEN")
        .where_eq("revision", revision)
        .execute(&*tx)
        .await?;
    if updated.len() != 1 {
        return Err(LobbyError::Conflict);
    }
    tx.commit().await?;
    Ok(game_id)
}

async fn insert_member(
    db: &dyn Database,
    lobby_id: &str,
    user_id: &str,
    seat: usize,
    joined_at_ms: i64,
) -> Result<(), LobbyError> {
    db.insert("game_lobby_members")
        .value("lobby_member_id", format!("{lobby_id}:{user_id}"))
        .value("lobby_id", lobby_id)
        .value("user_id", user_id)
        .value("seat", usize_i64(seat)?)
        .value("joined_at_ms", joined_at_ms)
        .execute(db)
        .await?;
    Ok(())
}

async fn member_rows(db: &dyn Database, lobby_id: &str) -> Result<Vec<LobbyMember>, LobbyError> {
    db.select("game_lobby_members")
        .where_eq("lobby_id", lobby_id)
        .sort("seat", SortDirection::Asc)
        .execute(db)
        .await?
        .iter()
        .map(|row| {
            Ok(LobbyMember {
                user_id: string(row, "user_id")?,
                seat: usize::try_from(unsigned(row, "seat")?).map_err(|_| LobbyError::Invalid)?,
            })
        })
        .collect()
}

fn settings(row: &switchy_database::Row) -> Result<LobbySettings, LobbyError> {
    let chosen = optional_string(row, "chosen_first_user_id")?;
    let first_player = match string(row, "first_player_policy")?.as_str() {
        "RANDOM" => FirstPlayerPolicy::Random,
        "CREATOR" => FirstPlayerPolicy::Creator,
        "CHOSEN" => FirstPlayerPolicy::Chosen(chosen.ok_or(LobbyError::Invalid)?),
        _ => return Err(LobbyError::Invalid),
    };
    Ok(LobbySettings {
        max_players: usize::try_from(unsigned(row, "max_players")?)
            .map_err(|_| LobbyError::Invalid)?,
        board_size: u8::try_from(unsigned(row, "board_size")?).map_err(|_| LobbyError::Invalid)?,
        tile_set_count: u8::try_from(unsigned(row, "tile_set_count")?)
            .map_err(|_| LobbyError::Invalid)?,
        first_player,
    })
}

fn scale_seed(seed: u64, upper: usize) -> usize {
    usize::try_from((u128::from(seed) * upper as u128) >> 64).expect("scaled seed is below upper")
}

fn string(row: &switchy_database::Row, name: &str) -> Result<String, LobbyError> {
    row.get(name)
        .and_then(|value| value.as_str().map(ToOwned::to_owned))
        .ok_or(LobbyError::Invalid)
}
fn optional_string(row: &switchy_database::Row, name: &str) -> Result<Option<String>, LobbyError> {
    row.get(name)
        .map(|value| value.as_str().map(ToOwned::to_owned))
        .ok_or(LobbyError::Invalid)
}
fn signed(row: &switchy_database::Row, name: &str) -> Result<i64, LobbyError> {
    row.get(name)
        .and_then(|value| value.as_i64())
        .ok_or(LobbyError::Invalid)
}
fn unsigned(row: &switchy_database::Row, name: &str) -> Result<u64, LobbyError> {
    u64::try_from(signed(row, name)?).map_err(|_| LobbyError::Invalid)
}
fn usize_i64(value: usize) -> Result<i64, LobbyError> {
    i64::try_from(value).map_err(|_| LobbyError::Invalid)
}
fn timestamp_ms(timestamp: OffsetDateTime) -> Result<i64, LobbyError> {
    i64::try_from(timestamp.unix_timestamp_nanos() / 1_000_000).map_err(|_| LobbyError::Timestamp)
}
fn token_hash(token: &str) -> String {
    Sha256::digest(token.as_bytes())
        .iter()
        .fold(String::with_capacity(64), |mut output, byte| {
            write!(output, "{byte:02x}").expect("writing to String is infallible");
            output
        })
}

#[derive(Debug, Error)]
pub enum LobbyError {
    #[error("game creation policy cannot admit a valid game")]
    InvalidPolicy,
    #[error("lobby settings violate game creation policy")]
    InvalidSettings,
    #[error("lobby is unavailable, expired, closed, full, or already joined")]
    Unavailable,
    #[error("only the lobby creator may perform this action")]
    Unauthorized,
    #[error("at least two valid members are required to start")]
    NotReady,
    #[error("lobby changed concurrently")]
    Conflict,
    #[error("lobby data is malformed")]
    Invalid,
    #[error("could not generate a collision-free lobby token")]
    Collision,
    #[error("timestamp is outside the supported range")]
    Timestamp,
    #[error(transparent)]
    Rules(#[from] wwmtf_game_domain::RuleProfileError),
    #[error(transparent)]
    GameCreation(#[from] crate::ChallengeError),
    #[error(transparent)]
    Database(#[from] switchy_database::DatabaseError),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{migrate_app, recover_game, register};
    use futures_lite::future::block_on;

    #[test]
    fn durable_lobby_identity_joins_only_while_open() {
        block_on(async {
            let db = switchy_database_connection::builder()
                .turso()
                .with_in_memory()
                .build()
                .await
                .expect("db");
            migrate_app(&*db).await.expect("migrate");
            let now = OffsetDateTime::UNIX_EPOCH;
            let creator = register(&*db, "durable-creator", "correct horse battery staple", now)
                .await
                .expect("creator");
            let joiner = register(&*db, "durable-joiner", "another correct horse battery", now)
                .await
                .expect("joiner");
            let late = register(&*db, "durable-late", "third correct horse battery", now)
                .await
                .expect("late");
            let policy = GameCreationPolicy::new(8, 32, 4).expect("policy");
            let (lobby_id, _) = create_lobby(
                &*db,
                &creator,
                LobbySettings {
                    max_players: 4,
                    board_size: 15,
                    tile_set_count: 1,
                    first_player: FirstPlayerPolicy::Creator,
                },
                policy,
                now,
                Duration::days(1),
            )
            .await
            .expect("lobby creates");
            assert_eq!(
                join_lobby_by_id(&*db, &lobby_id, &joiner, policy, now)
                    .await
                    .expect("durable identity joins"),
                lobby_id
            );
            start_lobby(&*db, &lobby_id, &creator, policy, now, 17)
                .await
                .expect("starts");
            assert!(matches!(
                join_lobby_by_id(&*db, &lobby_id, &late, policy, now).await,
                Err(LobbyError::Unavailable)
            ));
        });
    }

    #[test]
    fn lobby_secrets_are_redacted_and_never_persisted_or_returned_in_authorized_views() {
        block_on(async {
            let db = switchy_database_connection::builder()
                .turso()
                .with_in_memory()
                .build()
                .await
                .expect("db");
            migrate_app(&*db).await.expect("migrate");
            let now = OffsetDateTime::UNIX_EPOCH;
            let alice = register(&*db, "alice", "correct horse battery staple", now)
                .await
                .expect("alice");
            let policy = GameCreationPolicy::new(8, 32, 4).expect("policy");
            let (lobby_id, token) = create_lobby(
                &*db,
                &alice,
                LobbySettings {
                    max_players: 4,
                    board_size: 15,
                    tile_set_count: 1,
                    first_player: FirstPlayerPolicy::Creator,
                },
                policy,
                now,
                Duration::days(1),
            )
            .await
            .expect("lobby creates");
            let token_debug = format!("{token:?}");
            assert!(!token_debug.contains(token.expose()));
            let row = db
                .select("game_lobbies")
                .where_eq("lobby_id", lobby_id.clone())
                .execute(&*db)
                .await
                .expect("lobby row loads")
                .remove(0);
            let stored = row
                .get("invitation_token_hash")
                .and_then(|value| value.as_str().map(ToOwned::to_owned))
                .expect("hash stored");
            assert_ne!(stored, token.expose());
            let authorized = load_lobby(&*db, &lobby_id, &alice)
                .await
                .expect("authorized view loads");
            assert!(!format!("{authorized:?}").contains(token.expose()));
            assert!(!LobbyError::Unavailable.to_string().contains(token.expose()));
        });
    }

    #[test]
    fn three_members_join_and_creator_starts_exactly_once() {
        block_on(async {
            let db = switchy_database_connection::builder()
                .turso()
                .with_in_memory()
                .build()
                .await
                .expect("db");
            migrate_app(&*db).await.expect("migrate");
            let now = OffsetDateTime::UNIX_EPOCH;
            let alice = register(&*db, "alice", "correct horse battery staple", now)
                .await
                .expect("alice");
            let bob = register(&*db, "bob", "another correct horse battery", now)
                .await
                .expect("bob");
            let carol = register(&*db, "carol", "third correct horse battery", now)
                .await
                .expect("carol");
            let policy = GameCreationPolicy::new(16, 64, 16).expect("policy");
            let settings = LobbySettings {
                max_players: 8,
                board_size: 21,
                tile_set_count: 2,
                first_player: FirstPlayerPolicy::Creator,
            };
            let (lobby_id, token) =
                create_lobby(&*db, &alice, settings, policy, now, Duration::days(1))
                    .await
                    .expect("create");
            assert_eq!(
                join_lobby(&*db, token.expose(), &bob, policy, now)
                    .await
                    .expect("bob joins"),
                lobby_id
            );
            join_lobby(&*db, token.expose(), &carol, policy, now)
                .await
                .expect("carol joins");
            let game_id = start_lobby(&*db, &lobby_id, &alice, policy, now, 7)
                .await
                .expect("starts");
            let state = recover_game(&*db, game_id).await.expect("recovers");
            assert_eq!(state.players.len(), 3);
            assert_eq!(state.active_player, state.players[0]);
            assert_eq!(state.rules.board_size, 21);
            assert_eq!(state.rules.tile_count(), 200);
            assert_eq!(
                start_lobby(&*db, &lobby_id, &alice, policy, now, 9)
                    .await
                    .expect("idempotent"),
                game_id
            );
        });
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn multiplayer_lobby_product_matrix_covers_policies_capacity_leave_cancel_and_large_membership()
    {
        block_on(async {
            let db = switchy_database_connection::builder()
                .turso()
                .with_in_memory()
                .build()
                .await
                .expect("db");
            migrate_app(&*db).await.expect("migrate");
            let now = OffsetDateTime::UNIX_EPOCH;
            let mut users = Vec::new();
            for index in 0..8 {
                users.push(
                    register(
                        &*db,
                        &format!("player-{index}"),
                        &format!("correct horse battery password {index}"),
                        now,
                    )
                    .await
                    .expect("member registers"),
                );
            }
            let policy = GameCreationPolicy::new(8, 32, 4).expect("policy");

            for (policy_index, first_player) in [
                FirstPlayerPolicy::Creator,
                FirstPlayerPolicy::Random,
                FirstPlayerPolicy::Chosen(users[2].clone()),
            ]
            .into_iter()
            .enumerate()
            {
                let chosen_at_creation = matches!(first_player, FirstPlayerPolicy::Chosen(_));
                let initial_policy = if chosen_at_creation {
                    FirstPlayerPolicy::Creator
                } else {
                    first_player.clone()
                };
                let (lobby_id, token) = create_lobby(
                    &*db,
                    &users[0],
                    LobbySettings {
                        max_players: 8,
                        board_size: 21,
                        tile_set_count: 2,
                        first_player: initial_policy,
                    },
                    policy,
                    now,
                    Duration::days(1),
                )
                .await
                .expect("lobby creates");
                for user in &users[1..] {
                    join_lobby(&*db, token.expose(), user, policy, now)
                        .await
                        .expect("large membership joins");
                }
                if chosen_at_creation {
                    update_lobby_settings(
                        &*db,
                        &lobby_id,
                        &users[0],
                        LobbySettings {
                            max_players: 8,
                            board_size: 21,
                            tile_set_count: 2,
                            first_player: first_player.clone(),
                        },
                        policy,
                        now,
                    )
                    .await
                    .expect("chosen member configured");
                }
                let game_id = start_lobby(
                    &*db,
                    &lobby_id,
                    &users[0],
                    policy,
                    now,
                    u64::try_from(policy_index).expect("index fits") + 17,
                )
                .await
                .expect("manual start succeeds");
                let state = recover_game(&*db, game_id).await.expect("game recovers");
                assert_eq!(state.players.len(), 8);
                match first_player {
                    FirstPlayerPolicy::Creator => assert_eq!(state.active_player, state.players[0]),
                    FirstPlayerPolicy::Chosen(_) => {
                        assert_eq!(state.active_player, state.players[2]);
                    }
                    FirstPlayerPolicy::Random => {
                        assert!(state.players.contains(&state.active_player));
                    }
                }
            }

            let (leave_id, leave_token) = create_lobby(
                &*db,
                &users[0],
                LobbySettings {
                    max_players: 3,
                    board_size: 15,
                    tile_set_count: 1,
                    first_player: FirstPlayerPolicy::Creator,
                },
                policy,
                now,
                Duration::days(1),
            )
            .await
            .expect("leave lobby creates");
            join_lobby(&*db, leave_token.expose(), &users[1], policy, now)
                .await
                .expect("member joins");
            join_lobby(&*db, leave_token.expose(), &users[2], policy, now)
                .await
                .expect("capacity fills");
            assert!(matches!(
                join_lobby(&*db, leave_token.expose(), &users[3], policy, now).await,
                Err(LobbyError::Unavailable)
            ));
            leave_lobby(&*db, &leave_id, &users[2], now)
                .await
                .expect("member leaves");
            join_lobby(&*db, leave_token.expose(), &users[3], policy, now)
                .await
                .expect("released seat can be joined");

            let (cancel_id, cancel_token) = create_lobby(
                &*db,
                &users[0],
                LobbySettings {
                    max_players: 4,
                    board_size: 15,
                    tile_set_count: 1,
                    first_player: FirstPlayerPolicy::Creator,
                },
                policy,
                now,
                Duration::days(1),
            )
            .await
            .expect("cancel lobby creates");
            cancel_lobby(&*db, &cancel_id, &users[0], now)
                .await
                .expect("creator cancels");
            assert!(matches!(
                join_lobby(&*db, cancel_token.expose(), &users[1], policy, now).await,
                Err(LobbyError::Unavailable)
            ));
        });
    }

    #[test]
    fn policy_rejects_excessive_settings_and_lobby_capacity() {
        block_on(async {
            let db = switchy_database_connection::builder()
                .turso()
                .with_in_memory()
                .build()
                .await
                .expect("db");
            migrate_app(&*db).await.expect("migrate");
            let now = OffsetDateTime::UNIX_EPOCH;
            let alice = register(&*db, "alice", "correct horse battery staple", now)
                .await
                .expect("alice");
            let policy = GameCreationPolicy::new(4, 20, 2).expect("policy");
            let settings = LobbySettings {
                max_players: 5,
                board_size: 15,
                tile_set_count: 1,
                first_player: FirstPlayerPolicy::Random,
            };
            assert!(matches!(
                create_lobby(&*db, &alice, settings, policy, now, Duration::days(1)).await,
                Err(LobbyError::InvalidSettings)
            ));
        });
    }
}

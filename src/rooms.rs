//! Room finite-state-machine, matchmaking, and the turn-based battle engine.
//!
//! Built incrementally per DESIGN-MULTIPLAYER.md. Step 1 only needs `current_room_for` (F5
//! resume / first-paint routing); the matchmaker + RoomEngine land in later steps.

use crate::auth::MeRoom;
use crate::signaling::AppState;

/// Where is this user right now? `None` => Lobby. Populated once the room layer exists.
pub async fn current_room_for(_st: &AppState, _user_id: i32) -> Option<MeRoom> {
    None
}

/// Restart cleanup: mark live rooms abandoned so users resume to Lobby. No-op until rooms exist.
pub async fn recover_abandoned(_db: &sea_orm::DatabaseConnection) -> Result<(), sea_orm::DbErr> {
    Ok(())
}

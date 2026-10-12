pub mod connection;
#[cfg(test)]
pub mod test_helpers;

use rusqlite::Connection;
use std::sync::Mutex;

pub struct DbState(pub Mutex<Connection>);

/// Where the database file lives, for commands that need their own connection
/// (long-running work must not hold `DbState`'s lock and freeze every other command).
pub struct DbPath(pub std::path::PathBuf);

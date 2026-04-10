pub mod connection;
#[cfg(test)]
pub mod test_helpers;

use rusqlite::Connection;
use std::sync::Mutex;

pub struct DbState(pub Mutex<Connection>);

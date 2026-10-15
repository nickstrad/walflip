//! walflip: capture SQLite WAL commits and rebuild a database as of any commit.
//!
//! Layering:
//! - `wal`, `schema`, `replay` are pure: bytes/strings in, values out, no I/O.
//! - `store` persists captured commits in the metadata database.
//! - `db` composes the above into the operations the CLI exposes and returns plain data;
//!   it never prints.

pub mod db;
pub mod replay;
pub mod store;
pub mod wal;

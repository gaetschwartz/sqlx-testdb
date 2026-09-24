//! Throwaway per-test databases cloned from a shared template, for sqlx.

mod args;
mod backend;
mod config;
mod error;
mod harness;
mod name;
#[cfg(feature = "postgres")]
mod postgres;
mod schema;
mod sweep;
mod template;

pub use args::{TestArgs, TestOutcome};
pub use backend::{Backend, ConnectOptionsOf, DropMode, LockKey, LockPurpose};
pub use config::{Config, Fixture, PoolSettings, Schema, SqlSource, SweepSettings};
pub use error::Error;
pub use harness::{TestDb, run, url_of};
pub use sqlx_testdb_macros::test;

#[doc(hidden)]
pub mod __private {
    pub use sqlx::migrate::Migrator;
}

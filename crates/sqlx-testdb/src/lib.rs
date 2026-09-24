//! Throwaway per-test databases cloned from a shared template, for sqlx.

mod args;
mod backend;
mod config;
mod error;
mod harness;
mod name;
#[cfg(feature = "postgres")]
mod postgres;
mod probe;
mod schema;
mod sweep;
mod template;

pub use args::{TestArgs, TestOutcome};
pub use backend::{Backend, ConnectFailure, ConnectOptionsOf, DropMode, LockKey, LockPurpose};
pub use config::{CONFIG_FILE, CONFIG_PATH_VAR, Config, PoolSettings, Schema, SweepSettings};
pub use error::Error;
pub use harness::{SchemaOverride, TestDb, TestSpec, run, run_with, url_of};
pub use sqlx_testdb_macros::test;

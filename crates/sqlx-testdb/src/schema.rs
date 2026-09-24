use sha2::{Digest, Sha256};
use snafu::ResultExt;
use sqlx::migrate::{MigrationType, Migrator};

use crate::backend::Backend;
use crate::config::Schema;
use crate::error::{ApplySchemaSnafu, Error, LoadMigrationsSnafu, MigrateSnafu, ReadSchemaSnafu};

#[derive(Debug)]
pub enum LoadedSchema {
    None,
    Sql(Vec<String>),
    Migrator(&'static Migrator),
    Loaded(Migrator),
}

#[derive(Debug, Clone, Copy)]
enum SourceKind {
    None,
    Sql,
    Migrations,
}

impl SourceKind {
    const fn tag(self) -> &'static [u8] {
        match self {
            Self::None => b"none",
            Self::Sql => b"sql",
            Self::Migrations => b"migrations",
        }
    }
}

impl LoadedSchema {
    pub async fn load(schema: &Schema) -> Result<Self, Error> {
        Ok(match schema {
            Schema::None => Self::None,
            Schema::Sql(files) => {
                let mut texts = Vec::with_capacity(files.len());
                for path in files {
                    texts.push(std::fs::read_to_string(path).context(ReadSchemaSnafu { path })?);
                }
                Self::Sql(texts)
            }
            Schema::Migrator(migrator) => Self::Migrator(migrator),
            Schema::Migrations(path) => Self::Loaded(
                Migrator::new(path.as_std_path()).await.context(LoadMigrationsSnafu { path })?,
            ),
        })
    }

    const fn migrator(&self) -> Option<&Migrator> {
        match self {
            Self::None | Self::Sql(_) => None,
            Self::Migrator(migrator) => Some(migrator),
            Self::Loaded(migrator) => Some(migrator),
        }
    }

    pub fn digest(&self) -> [u8; 32] {
        let mut hasher = Sha256::new();
        let kind = match self {
            Self::None => SourceKind::None,
            Self::Sql(_) => SourceKind::Sql,
            Self::Migrator(_) | Self::Loaded(_) => SourceKind::Migrations,
        };
        hasher.update(kind.tag());
        if let Self::Sql(texts) = self {
            for sql in texts {
                hasher.update(sql.len().to_be_bytes());
                hasher.update(sql.as_bytes());
            }
        }
        if let Some(migrator) = self.migrator() {
            for migration in migrator.iter() {
                hasher.update(migration.version.to_be_bytes());
                hasher.update([u8::from(migration_direction_is_down(migration.migration_type))]);
                hasher.update(migration.checksum.len().to_be_bytes());
                hasher.update(&migration.checksum);
            }
        }
        hasher.finalize().into()
    }

    pub async fn apply<DB: Backend>(
        &self,
        conn: &mut DB::Connection,
        name: &str,
    ) -> Result<(), Error> {
        match self {
            Self::None => Ok(()),
            Self::Sql(texts) => {
                for sql in texts {
                    DB::apply_sql(conn, sql).await.context(ApplySchemaSnafu { name })?;
                }
                Ok(())
            }
            Self::Migrator(migrator) => {
                DB::apply_migrator(conn, migrator).await.context(MigrateSnafu { name })
            }
            Self::Loaded(migrator) => {
                DB::apply_migrator(conn, migrator).await.context(MigrateSnafu { name })
            }
        }
    }
}

const fn migration_direction_is_down(kind: MigrationType) -> bool {
    matches!(kind, MigrationType::ReversibleDown)
}

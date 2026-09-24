use snafu::ResultExt;
use sqlx::Connection;

use crate::backend::{Backend, ConnectOptionsOf, DropMode};
use crate::error::{
    AlterTemplateSnafu, BookkeepingSnafu, ConnectSnafu, CreateDatabaseSnafu, DropDatabaseSnafu,
    Error, LockSnafu,
};
use crate::harness::Ctx;
use crate::schema::LoadedSchema;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dropped {
    Yes,
    InUse,
}

pub async fn drop<DB: Backend>(
    conn: &mut DB::Connection,
    name: &str,
    mode: DropMode,
) -> Result<Dropped, Error> {
    match DB::drop_database(conn, name, mode).await {
        Ok(()) => Ok(Dropped::Yes),
        Err(e) if mode == DropMode::IfIdle && DB::is_in_use(&e) => Ok(Dropped::InUse),
        Err(source) => Err(source).context(DropDatabaseSnafu { name }),
    }
}

pub async fn ensure<DB: Backend>(
    conn: &mut DB::Connection,
    ctx: &Ctx<'_>,
    schema: &LoadedSchema,
    options: &ConnectOptionsOf<DB>,
) -> Result<(), Error> {
    if DB::database_exists(conn, &ctx.template).await.context(BookkeepingSnafu)? {
        return touch::<DB>(conn, ctx).await;
    }
    build::<DB>(conn, ctx, schema, options).await
}

pub async fn clone_into<DB: Backend>(
    conn: &mut DB::Connection,
    ctx: &Ctx<'_>,
    schema: &LoadedSchema,
    options: &ConnectOptionsOf<DB>,
    name: &str,
) -> Result<(), Error> {
    match DB::clone_database(conn, name, &ctx.template).await {
        Err(e) if DB::is_missing(&e) => {
            build::<DB>(conn, ctx, schema, options).await?;
            DB::clone_database(conn, name, &ctx.template)
                .await
                .context(CreateDatabaseSnafu { name })
        }
        created => created.context(CreateDatabaseSnafu { name }),
    }
}

pub async fn drop_orphaned_builds<DB: Backend>(
    conn: &mut DB::Connection,
    ctx: &Ctx<'_>,
) -> Result<(), Error> {
    let orphans = DB::databases_with_prefix(conn, &ctx.names.building_prefix())
        .await
        .context(BookkeepingSnafu)?;
    for orphan in orphans {
        drop::<DB>(conn, &orphan, DropMode::Force).await?;
    }
    Ok(())
}

async fn touch<DB: Backend>(conn: &mut DB::Connection, ctx: &Ctx<'_>) -> Result<(), Error> {
    DB::touch_template(conn, ctx.names.bookkeeping_schema, &ctx.template)
        .await
        .context(BookkeepingSnafu)
}

async fn build<DB: Backend>(
    conn: &mut DB::Connection,
    ctx: &Ctx<'_>,
    schema: &LoadedSchema,
    options: &ConnectOptionsOf<DB>,
) -> Result<(), Error> {
    let key = ctx.template_key;
    DB::lock(conn, key).await.context(LockSnafu { key })?;
    let built = build_locked::<DB>(conn, ctx, schema, options).await;
    DB::unlock(conn, key).await.context(LockSnafu { key })?;
    built
}

async fn build_locked<DB: Backend>(
    conn: &mut DB::Connection,
    ctx: &Ctx<'_>,
    schema: &LoadedSchema,
    options: &ConnectOptionsOf<DB>,
) -> Result<(), Error> {
    if DB::database_exists(conn, &ctx.template).await.context(BookkeepingSnafu)? {
        return touch::<DB>(conn, ctx).await;
    }
    drop_orphaned_builds::<DB>(conn, ctx).await?;
    let building = ctx.names.building();
    DB::create_database(conn, &building).await.context(CreateDatabaseSnafu { name: &building })?;
    if let Err(e) = apply_schema::<DB>(schema, options, &building).await {
        drop::<DB>(conn, &building, DropMode::Force).await?;
        return Err(e);
    }
    DB::publish_template(conn, &building, &ctx.template)
        .await
        .context(AlterTemplateSnafu { name: &ctx.template })?;
    touch::<DB>(conn, ctx).await
}

async fn apply_schema<DB: Backend>(
    schema: &LoadedSchema,
    options: &ConnectOptionsOf<DB>,
    building: &str,
) -> Result<(), Error> {
    let mut target = DB::Connection::connect_with(&DB::options_for_database(options, building))
        .await
        .context(ConnectSnafu)?;
    let applied = schema.apply::<DB>(&mut target, building).await;
    let closed = target.close().await;
    applied?;
    closed.context(ConnectSnafu)
}

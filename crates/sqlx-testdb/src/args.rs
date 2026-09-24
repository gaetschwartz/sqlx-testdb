use std::fmt::Debug;

use snafu::ResultExt;
use sqlx::Pool;
use sqlx::pool::{PoolConnection, PoolOptions};

use crate::backend::{Backend, ConnectOptionsOf};
use crate::error::{ConnectSnafu, Error};
use crate::harness::TestDb;

pub trait TestArgs<DB: Backend>: Sized {
    fn make(db: &TestDb<DB>) -> impl Future<Output = Result<Self, Error>>;
}

impl<DB: Backend> TestArgs<DB> for Pool<DB> {
    fn make(db: &TestDb<DB>) -> impl Future<Output = Result<Self, Error>> {
        std::future::ready(Ok(db.pool().clone()))
    }
}

impl<DB: Backend> TestArgs<DB> for PoolConnection<DB> {
    async fn make(db: &TestDb<DB>) -> Result<Self, Error> {
        db.pool().acquire().await.context(ConnectSnafu)
    }
}

impl<DB: Backend> TestArgs<DB> for (PoolOptions<DB>, ConnectOptionsOf<DB>) {
    fn make(db: &TestDb<DB>) -> impl Future<Output = Result<Self, Error>> {
        std::future::ready(Ok((db.pool_options(), db.connect_options().clone())))
    }
}

pub trait TestOutcome {
    fn into_result(self) -> Result<(), Box<dyn Debug>>;
}

impl TestOutcome for () {
    fn into_result(self) -> Result<(), Box<dyn Debug>> {
        Ok(())
    }
}

impl<T, E: Debug + 'static> TestOutcome for Result<T, E> {
    fn into_result(self) -> Result<(), Box<dyn Debug>> {
        self.map(drop).map_err(|e| Box::new(e) as Box<dyn Debug>)
    }
}

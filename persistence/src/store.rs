use std::sync::Arc;

use chrono::{DateTime, Duration, Utc};
use rocksdb::{ColumnFamilyDescriptor, DB as RocksDb, Error as RocksDbError, Options};
use serde::de::DeserializeOwned;
use serde::ser::Serialize;
use serde_cbor;

use semaphore_config::Config;

/// Represents an error from the store.
#[derive(Debug, Fail)]
pub enum StoreError {
    /// Indicates that the store could not be opened.
    #[fail(display = "cannot open store")]
    CannotOpen(#[cause] RocksDbError),
    /// Indicates that writing to the db failed.
    #[fail(display = "cannot write to database")]
    WriteError(#[cause] RocksDbError),
    /// Raised if repairs failed
    #[fail(display = "could not repair storage")]
    RepairFailed(#[cause] RocksDbError),
    /// Raised on deserialization errors.
    #[fail(display = "cannot deseralize value from database")]
    DeserializeError(#[cause] serde_cbor::error::Error),
}

/// Represents the store for the persistence layer.
pub struct Store {
    db: RocksDb,
    config: Arc<Config>,
}

impl Store {
    /// Opens a store for the given config.
    pub fn open(config: Arc<Config>) -> Result<Store, StoreError> {
        let path = config.path().join("storage");
        let opts = get_database_options();
        let cfs = vec![
            ColumnFamilyDescriptor::new("cache", get_column_family_options()),
            ColumnFamilyDescriptor::new("projects", get_column_family_options()),
        ];
        let db = RocksDb::open_cf_descriptors(&opts, &path, cfs).map_err(StoreError::CannotOpen)?;
        Ok(Store { db: db, config })
    }

    /// Attempts to repair the store.
    pub fn repair(config: Arc<Config>) -> Result<(), StoreError> {
        let path = config.path().join("storage");
        RocksDb::repair(get_database_options(), &path).map_err(StoreError::RepairFailed)
    }

    /// Caches a certain value.
    pub fn cache_set<S: Serialize>(
        &self,
        key: &str,
        value: &S,
        ttl: Option<Duration>,
    ) -> Result<(), StoreError> {
        #[derive(Serialize)]
        pub struct CacheItem<'a, T: Serialize + 'a>(&'a T, Option<DateTime<Utc>>);
        let main_cf = self.db.cf_handle("main").unwrap();
        self.db
            .put_cf(
                main_cf,
                key.as_bytes(),
                &serde_cbor::to_vec(&CacheItem(value, ttl.map(|x| Utc::now() + x))).unwrap(),
            )
            .map_err(StoreError::WriteError)
    }

    /// Looks up a value in the cache.
    pub fn cache_get<D: DeserializeOwned>(&self, key: &str) -> Result<Option<D>, StoreError> {
        #[derive(Deserialize)]
        pub struct CacheItem<T>(T, Option<DateTime<Utc>>);
        let main_cf = self.db.cf_handle("main").unwrap();
        match self.db.get_cf(main_cf, key.as_bytes()) {
            Ok(Some(value)) => {
                let item: CacheItem<D> =
                    serde_cbor::from_slice(&value).map_err(StoreError::DeserializeError)?;
                match item.1 {
                    None => Ok(Some(item.0)),
                    Some(ts) if ts > Utc::now() => Ok(Some(item.0)),
                    _ => Ok(None),
                }
            }
            Ok(None) => Ok(None),
            Err(err) => Err(StoreError::WriteError(err)),
        }
    }
}

fn get_column_family_options() -> Options {
    let mut cf_opts = Options::default();
    cf_opts.set_max_write_buffer_number(4);
    cf_opts
}

fn get_database_options() -> Options {
    let mut db_opts = Options::default();
    db_opts.create_missing_column_families(true);
    db_opts.create_if_missing(true);
    db_opts
}

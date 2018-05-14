use std::sync::Arc;

use chrono::{DateTime, Duration, Utc};
use rocksdb::{ColumnFamilyDescriptor, DB as RocksDb, Error as RocksDbError, Options,
              compaction_filter::Decision};
use serde::de::{DeserializeOwned, IgnoredAny};
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

#[derive(Debug, PartialEq)]
enum FamilyType {
    Persistent,
    Cache,
}

impl Store {
    /// Opens a store for the given config.
    pub fn open(config: Arc<Config>) -> Result<Store, StoreError> {
        let path = config.path().join("storage");
        let opts = get_database_options();
        let cfs = vec![
            ColumnFamilyDescriptor::new("cache", get_column_family_options(FamilyType::Cache)),
            ColumnFamilyDescriptor::new(
                "projects",
                get_column_family_options(FamilyType::Persistent),
            ),
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
        pub struct CacheItem<'a, T: Serialize + 'a>(Option<DateTime<Utc>>, &'a T);
        let main_cf = self.db.cf_handle("cache").unwrap();
        self.db
            .put_cf(
                main_cf,
                key.as_bytes(),
                &serde_cbor::to_vec(&CacheItem(ttl.map(|x| Utc::now() + x), value)).unwrap(),
            )
            .map_err(StoreError::WriteError)
    }

    /// Looks up a value in the cache.
    pub fn cache_get<D: DeserializeOwned>(&self, key: &str) -> Result<Option<D>, StoreError> {
        #[derive(Deserialize)]
        pub struct CacheItem<T>(Option<DateTime<Utc>>, T);
        let main_cf = self.db.cf_handle("cache").unwrap();
        match self.db.get_cf(main_cf, key.as_bytes()) {
            Ok(Some(value)) => {
                let item: CacheItem<D> =
                    serde_cbor::from_slice(&value).map_err(StoreError::DeserializeError)?;
                match item.0 {
                    None => Ok(Some(item.1)),
                    Some(ts) if ts > Utc::now() => Ok(Some(item.1)),
                    _ => Ok(None),
                }
            }
            Ok(None) => Ok(None),
            Err(err) => Err(StoreError::WriteError(err)),
        }
    }
}

fn ttl_compaction_filter(_level: u32, _key: &[u8], value: &[u8]) -> Decision {
    #[derive(Deserialize)]
    pub struct TtlInfo(Option<DateTime<Utc>>, IgnoredAny);

    serde_cbor::from_slice::<TtlInfo>(value)
        .ok()
        .and_then(|x| x.0)
        .map_or(Decision::Keep, |value| {
            if value < Utc::now() {
                Decision::Remove
            } else {
                Decision::Keep
            }
        })
}

fn get_column_family_options(family: FamilyType) -> Options {
    let mut cf_opts = Options::default();
    cf_opts.set_max_write_buffer_number(4);
    if family == FamilyType::Cache {
        cf_opts.set_compaction_filter("ttl", ttl_compaction_filter);
    }
    cf_opts
}

fn get_database_options() -> Options {
    let mut db_opts = Options::default();
    db_opts.create_missing_column_families(true);
    db_opts.create_if_missing(true);
    db_opts
}

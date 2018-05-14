use std::sync::Arc;

use rocksdb::{ColumnFamily, ColumnFamilyDescriptor, DB as RocksDb, Error as RocksDbError, Options};
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
    /// Raised on deserialization errors.
    #[fail(display = "cannot deseralize value from database")]
    DeserializeError(#[cause] serde_cbor::error::Error),
}

/// Represents the store for the persistence layer.
pub struct Store {
    db: RocksDb,
    config: Arc<Config>,
    main_cf: ColumnFamily,
}

impl Store {
    /// Opens a store for the given config.
    pub fn open(config: Arc<Config>) -> Result<Store, StoreError> {
        let path = config.path().join("storage");
        let opts = get_database_options();
        let cfs = vec![
            ColumnFamilyDescriptor::new("main", get_column_family_options()),
        ];
        let db = RocksDb::open_cf_descriptors(&opts, &path, cfs).map_err(StoreError::CannotOpen)?;
        let main_cf = db.cf_handle("main")
            .expect("could not get main column family");
        Ok(Store {
            db: db,
            config,
            main_cf,
        })
    }

    /// Stores a key in the main k/v part of the storage.
    pub fn set<S: Serialize>(&self, key: &str, value: &S) -> Result<(), StoreError> {
        self.db
            .put_cf(
                self.main_cf,
                key.as_bytes(),
                &serde_cbor::to_vec(value).unwrap(),
            )
            .map_err(StoreError::WriteError)
    }

    /// Looks up a key expecting a certain type.
    pub fn get<D: DeserializeOwned>(&self, key: &str) -> Result<Option<D>, StoreError> {
        match self.db.get_cf(self.main_cf, key.as_bytes()) {
            Ok(Some(value)) => {
                Ok(Some(serde_cbor::from_slice(&value).map_err(StoreError::DeserializeError)?))
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

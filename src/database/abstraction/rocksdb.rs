use std::{
    collections::HashSet,
    future::Future,
    pin::Pin,
    sync::{Arc, Mutex, RwLock},
};

use rocksdb::{
    perf::get_memory_usage_stats, BlockBasedOptions, BoundColumnFamily, Cache,
    ColumnFamilyDescriptor, DBCompactionStyle, DBCompressionType,
    DBRecoveryMode, DBWithThreadMode, Direction, IteratorMode, MultiThreaded,
    Options, ReadOptions, WriteOptions,
};
use tracing::Level;

use super::{
    super::Config, watchers::Watchers, KeyValueDatabaseEngine, KvTree,
};
use crate::{utils, Result};

pub(crate) struct Engine {
    rocks: DBWithThreadMode<MultiThreaded>,
    max_open_files: i32,
    cache: Cache,
    old_cfs: HashSet<String>,
    new_cfs: Mutex<HashSet<&'static str>>,
}

pub(crate) struct RocksDbEngineTree<'a> {
    db: Arc<Engine>,
    name: &'a str,
    watchers: Watchers,
    write_lock: RwLock<()>,
}

fn db_options(max_open_files: i32, rocksdb_cache: &Cache) -> Options {
    let mut block_based_options = BlockBasedOptions::default();
    block_based_options.set_block_cache(rocksdb_cache);
    block_based_options.set_bloom_filter(10.0, false);
    block_based_options.set_block_size(4 * 1024);
    block_based_options.set_cache_index_and_filter_blocks(true);
    block_based_options.set_pin_l0_filter_and_index_blocks_in_cache(true);
    block_based_options.set_optimize_filters_for_memory(true);

    let mut db_opts = Options::default();
    db_opts.set_block_based_table_factory(&block_based_options);
    db_opts.create_if_missing(true);
    db_opts
        .increase_parallelism(num_cpus::get().try_into().unwrap_or(i32::MAX));
    db_opts.set_max_open_files(max_open_files);
    db_opts.set_compression_type(DBCompressionType::Lz4);
    db_opts.set_bottommost_compression_type(DBCompressionType::Zstd);
    db_opts.set_compaction_style(DBCompactionStyle::Level);

    // https://github.com/facebook/rocksdb/wiki/Setup-Options-and-Basic-Tuning
    db_opts.set_level_compaction_dynamic_level_bytes(true);
    db_opts.set_max_background_jobs(6);
    db_opts.set_bytes_per_sync(1_048_576);

    // https://github.com/facebook/rocksdb/issues/849
    db_opts.set_keep_log_file_num(100);

    // https://github.com/facebook/rocksdb/wiki/WAL-Recovery-Modes#ktoleratecorruptedtailrecords
    //
    // Unclean shutdowns of a Matrix homeserver are likely to be fine when
    // recovered in this manner as it's likely any lost information will be
    // restored via federation.
    db_opts.set_wal_recovery_mode(DBRecoveryMode::TolerateCorruptedTailRecords);

    db_opts
}

impl KeyValueDatabaseEngine for Arc<Engine> {
    fn open(config: &Config) -> Result<Self> {
        #[allow(
            clippy::as_conversions,
            clippy::cast_sign_loss,
            clippy::cast_possible_truncation
        )]
        let cache_capacity_bytes =
            (config.database.cache_capacity_mb * 1024.0 * 1024.0) as usize;
        let rocksdb_cache = Cache::new_lru_cache(cache_capacity_bytes);

        let db_opts =
            db_options(config.database.rocksdb_max_open_files, &rocksdb_cache);

        let cfs = DBWithThreadMode::<MultiThreaded>::list_cf(
            &db_opts,
            &config.database.path,
        )
        .map(|x| x.into_iter().collect::<HashSet<_>>())
        .unwrap_or_default();

        let db = DBWithThreadMode::<MultiThreaded>::open_cf_descriptors(
            &db_opts,
            &config.database.path,
            cfs.iter().map(|name| {
                ColumnFamilyDescriptor::new(
                    name,
                    db_options(
                        config.database.rocksdb_max_open_files,
                        &rocksdb_cache,
                    ),
                )
            }),
        )?;

        Ok(Arc::new(Engine {
            rocks: db,
            max_open_files: config.database.rocksdb_max_open_files,
            cache: rocksdb_cache,
            old_cfs: cfs,
            new_cfs: Mutex::default(),
        }))
    }

    fn open_tree(&self, name: &'static str) -> Result<Arc<dyn KvTree>> {
        let mut new_cfs =
            self.new_cfs.lock().expect("lock should not be poisoned");

        let created_already = !new_cfs.insert(name);

        assert!(
            // userroomid_highlightcount is special-cased because it is an
            // existing violation of this check that happens to work anyway. We
            // should write a database migration to obviate the need for this.
            !(created_already && name != "userroomid_highlightcount"),
            "detected attempt to alias column family: {name}",
        );

        // Remove `&& !created_already` when the above is addressed
        if !self.old_cfs.contains(name) && !created_already {
            // Create if it didn't exist
            self.rocks
                .create_cf(name, &db_options(self.max_open_files, &self.cache))
                .expect("should be able to create column family");
        }

        Ok(Arc::new(RocksDbEngineTree {
            name,
            db: Arc::clone(self),
            watchers: Watchers::default(),
            write_lock: RwLock::new(()),
        }))
    }

    #[allow(clippy::as_conversions, clippy::cast_precision_loss)]
    fn memory_usage(&self) -> Result<String> {
        let stats =
            get_memory_usage_stats(Some(&[&self.rocks]), Some(&[&self.cache]))?;
        Ok(format!(
            "Approximate memory usage of all the mem-tables: {:.3} \
             MB\nApproximate memory usage of un-flushed mem-tables: {:.3} \
             MB\nApproximate memory usage of all the table readers: {:.3} \
             MB\nApproximate memory usage by cache: {:.3} MB\nApproximate \
             memory usage by cache pinned: {:.3} MB\n",
            stats.mem_table_total as f64 / 1024.0 / 1024.0,
            stats.mem_table_unflushed as f64 / 1024.0 / 1024.0,
            stats.mem_table_readers_total as f64 / 1024.0 / 1024.0,
            stats.cache_total as f64 / 1024.0 / 1024.0,
            self.cache.get_pinned_usage() as f64 / 1024.0 / 1024.0,
        ))
    }
}

impl RocksDbEngineTree<'_> {
    fn cf(&self) -> Arc<BoundColumnFamily<'_>> {
        self.db.rocks.cf_handle(self.name).unwrap()
    }
}

impl KvTree for RocksDbEngineTree<'_> {
    #[tracing::instrument(level = Level::TRACE, skip_all)]
    fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>> {
        let readoptions = ReadOptions::default();

        Ok(self.db.rocks.get_cf_opt(&self.cf(), key, &readoptions)?)
    }

    #[tracing::instrument(level = Level::TRACE, skip_all)]
    fn insert(&self, key: &[u8], value: &[u8]) -> Result<()> {
        let writeoptions = WriteOptions::default();
        let lock = self.write_lock.read().unwrap();
        self.db.rocks.put_cf_opt(&self.cf(), key, value, &writeoptions)?;
        drop(lock);

        self.watchers.wake(key);

        Ok(())
    }

    #[tracing::instrument(level = Level::TRACE, skip_all)]
    fn insert_batch(
        &self,
        iter: &mut dyn Iterator<Item = (Vec<u8>, Vec<u8>)>,
    ) -> Result<()> {
        let writeoptions = WriteOptions::default();
        for (key, value) in iter {
            self.db.rocks.put_cf_opt(&self.cf(), key, value, &writeoptions)?;
        }

        Ok(())
    }

    #[tracing::instrument(level = Level::TRACE, skip_all)]
    fn remove(&self, key: &[u8]) -> Result<()> {
        let writeoptions = WriteOptions::default();
        Ok(self.db.rocks.delete_cf_opt(&self.cf(), key, &writeoptions)?)
    }

    #[tracing::instrument(level = Level::TRACE, skip_all)]
    fn iter<'a>(&'a self) -> Box<dyn Iterator<Item = (Vec<u8>, Vec<u8>)> + 'a> {
        let readoptions = ReadOptions::default();

        Box::new(
            self.db
                .rocks
                .iterator_cf_opt(&self.cf(), readoptions, IteratorMode::Start)
                .map(Result::unwrap)
                .map(|(k, v)| (Vec::from(k), Vec::from(v))),
        )
    }

    #[tracing::instrument(level = Level::TRACE, skip_all)]
    fn iter_from<'a>(
        &'a self,
        from: &[u8],
        backwards: bool,
    ) -> Box<dyn Iterator<Item = (Vec<u8>, Vec<u8>)> + 'a> {
        let readoptions = ReadOptions::default();

        Box::new(
            self.db
                .rocks
                .iterator_cf_opt(
                    &self.cf(),
                    readoptions,
                    IteratorMode::From(
                        from,
                        if backwards {
                            Direction::Reverse
                        } else {
                            Direction::Forward
                        },
                    ),
                )
                .map(Result::unwrap)
                .map(|(k, v)| (Vec::from(k), Vec::from(v))),
        )
    }

    #[tracing::instrument(level = Level::TRACE, skip_all)]
    fn increment(&self, key: &[u8]) -> Result<Vec<u8>> {
        let readoptions = ReadOptions::default();
        let writeoptions = WriteOptions::default();

        let lock = self.write_lock.write().unwrap();

        let old = self.db.rocks.get_cf_opt(&self.cf(), key, &readoptions)?;
        let new = utils::increment(old.as_deref());
        self.db.rocks.put_cf_opt(&self.cf(), key, &new, &writeoptions)?;

        drop(lock);
        Ok(new)
    }

    #[tracing::instrument(level = Level::TRACE, skip_all)]
    fn increment_batch(
        &self,
        iter: &mut dyn Iterator<Item = Vec<u8>>,
    ) -> Result<()> {
        let readoptions = ReadOptions::default();
        let writeoptions = WriteOptions::default();

        let lock = self.write_lock.write().unwrap();

        for key in iter {
            let old =
                self.db.rocks.get_cf_opt(&self.cf(), &key, &readoptions)?;
            let new = utils::increment(old.as_deref());
            self.db.rocks.put_cf_opt(&self.cf(), key, new, &writeoptions)?;
        }

        drop(lock);

        Ok(())
    }

    #[tracing::instrument(level = Level::TRACE, skip_all)]
    fn scan_prefix<'a>(
        &'a self,
        prefix: Vec<u8>,
    ) -> Box<dyn Iterator<Item = (Vec<u8>, Vec<u8>)> + 'a> {
        let readoptions = ReadOptions::default();

        Box::new(
            self.db
                .rocks
                .iterator_cf_opt(
                    &self.cf(),
                    readoptions,
                    IteratorMode::From(&prefix, Direction::Forward),
                )
                .map(Result::unwrap)
                .map(|(k, v)| (Vec::from(k), Vec::from(v)))
                .take_while(move |(k, _)| k.starts_with(&prefix)),
        )
    }

    #[tracing::instrument(level = Level::TRACE, skip_all)]
    fn watch_prefix<'a>(
        &'a self,
        prefix: &[u8],
    ) -> Pin<Box<dyn Future<Output = ()> + Send + 'a>> {
        self.watchers.watch(prefix)
    }
}

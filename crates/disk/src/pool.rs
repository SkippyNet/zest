//! The worker pool shared by all scanners in this crate, plus small helpers
//! for per-thread accumulation.

use std::hash::{BuildHasherDefault, Hasher};
use std::sync::{Mutex, MutexGuard, OnceLock};

/// Recursive directory scans nest one stack frame per folder level and may
/// pick up further work while waiting, so workers get a generous stack.
const WORKER_STACK_BYTES: usize = 16 * 1024 * 1024;

/// Upper bound on worker threads; directory enumeration stops scaling long
/// before this on real disks.
const MAX_WORKERS: usize = 32;

/// The dedicated thread pool for disk scans.
pub(crate) fn pool() -> &'static rayon::ThreadPool {
    static POOL: OnceLock<rayon::ThreadPool> = OnceLock::new();
    POOL.get_or_init(|| {
        let threads = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4)
            .clamp(2, MAX_WORKERS);
        rayon::ThreadPoolBuilder::new()
            .num_threads(threads)
            .stack_size(WORKER_STACK_BYTES)
            .thread_name(|i| format!("zest-disk-{i}"))
            .build()
            .unwrap_or_else(|_| {
                // Falling back to a minimal pool keeps scans working even if
                // large stacks cannot be reserved.
                rayon::ThreadPoolBuilder::new()
                    .num_threads(2)
                    .build()
                    .expect("a two-thread pool can always be created")
            })
    })
}

/// One accumulator per worker thread. Each worker only ever locks its own
/// slot, so the mutexes are uncontended; they exist to keep the code safe.
pub(crate) struct Shards<T> {
    slots: Vec<Mutex<T>>,
}

impl<T: Default> Shards<T> {
    /// Creates one slot per pool worker plus one for outside callers.
    pub fn new() -> Self {
        let count = pool().current_num_threads() + 1;
        Shards {
            slots: (0..count).map(|_| Mutex::new(T::default())).collect(),
        }
    }

    /// Locks the slot belonging to the current worker thread.
    ///
    /// Callers must not run rayon operations while holding the guard: a
    /// worker that goes looking for other work could re-enter this slot.
    pub fn local(&self) -> MutexGuard<'_, T> {
        let index = rayon::current_thread_index().map_or(self.slots.len() - 1, |i| i);
        let slot = &self.slots[index % self.slots.len()];
        slot.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Consumes the shards, yielding every slot's accumulator.
    pub fn into_inner(self) -> Vec<T> {
        self.slots
            .into_iter()
            .map(|slot| slot.into_inner().unwrap_or_else(|p| p.into_inner()))
            .collect()
    }
}

/// A small, fast, non-cryptographic hasher (FxHash) for the hot hash maps.
#[derive(Clone, Copy, Default)]
pub(crate) struct FxHasher(u64);

const FX_SEED: u64 = 0x51_7c_c1_b7_27_22_0a_95;

impl FxHasher {
    #[inline]
    fn add(&mut self, word: u64) {
        self.0 = (self.0.rotate_left(5) ^ word).wrapping_mul(FX_SEED);
    }
}

impl Hasher for FxHasher {
    #[inline]
    fn write(&mut self, bytes: &[u8]) {
        let mut chunks = bytes.chunks_exact(8);
        for chunk in &mut chunks {
            self.add(u64::from_le_bytes([
                chunk[0], chunk[1], chunk[2], chunk[3], chunk[4], chunk[5], chunk[6], chunk[7],
            ]));
        }
        let rest = chunks.remainder();
        if !rest.is_empty() {
            let mut last = [0u8; 8];
            last[..rest.len()].copy_from_slice(rest);
            self.add(u64::from_le_bytes(last));
        }
    }

    #[inline]
    fn write_u8(&mut self, i: u8) {
        self.add(u64::from(i));
    }

    #[inline]
    fn write_u32(&mut self, i: u32) {
        self.add(u64::from(i));
    }

    #[inline]
    fn write_u64(&mut self, i: u64) {
        self.add(i);
    }

    #[inline]
    fn write_usize(&mut self, i: usize) {
        self.add(i as u64);
    }

    #[inline]
    fn finish(&self) -> u64 {
        self.0
    }
}

/// `HashMap` using [`FxHasher`].
pub(crate) type FxHashMap<K, V> = std::collections::HashMap<K, V, BuildHasherDefault<FxHasher>>;

/// `HashSet` using [`FxHasher`].
pub(crate) type FxHashSet<K> = std::collections::HashSet<K, BuildHasherDefault<FxHasher>>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shards_collect_from_all_workers() {
        use rayon::prelude::*;
        let shards: Shards<u64> = Shards::new();
        pool().install(|| {
            (0..10_000u64).into_par_iter().for_each(|i| {
                *shards.local() += i;
            });
        });
        // Also usable from a thread outside the pool.
        *shards.local() += 1;
        let total: u64 = shards.into_inner().into_iter().sum();
        assert_eq!(total, (0..10_000u64).sum::<u64>() + 1);
    }

    #[test]
    fn fx_hash_distinguishes_keys() {
        let mut map: FxHashMap<String, u32> = FxHashMap::default();
        for i in 0..1000 {
            map.insert(format!("ext{i}"), i);
        }
        assert_eq!(map.len(), 1000);
        assert_eq!(map.get("ext512"), Some(&512));
    }
}

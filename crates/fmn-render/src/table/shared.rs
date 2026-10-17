//! Copy-on-write storage for append-only retained IR tables.
//!
//! Frame generations share their rows and lookup indices until an actual
//! insertion. In particular, a zero-row reservation must not detach storage:
//! the retained compiler performs it on every sync, including pure placement
//! changes. All fallible outer allocations happen at the existing reservation
//! boundary, before the caller publishes semantic rows.

use std::collections::HashMap;
use std::hash::Hash;
use std::ops::Deref;
use std::sync::Arc;

/// The table owner attaches its resource name and requested count to refusals.
#[derive(Debug)]
pub(crate) struct ReserveError;

#[derive(Debug)]
pub(crate) struct SharedRows<T>(Arc<Vec<T>>);

impl<T> Default for SharedRows<T> {
    fn default() -> Self {
        Self(Arc::new(Vec::new()))
    }
}

impl<T> Clone for SharedRows<T> {
    fn clone(&self) -> Self {
        Self(Arc::clone(&self.0))
    }
}

impl<T> Deref for SharedRows<T> {
    type Target = [T];

    fn deref(&self) -> &[T] {
        self.0.as_slice()
    }
}

impl<T> SharedRows<T> {
    pub(crate) fn clear(&mut self) {
        match Arc::get_mut(&mut self.0) {
            Some(rows) => rows.clear(),
            None => *self = Self::default(),
        }
    }
}

impl<T: Clone> SharedRows<T> {
    pub(crate) fn try_reserve(&mut self, additional: usize) -> Result<(), ReserveError> {
        self.reserve(additional, false)
    }

    pub(crate) fn try_reserve_exact(&mut self, additional: usize) -> Result<(), ReserveError> {
        self.reserve(additional, true)
    }

    fn reserve(&mut self, additional: usize, exact: bool) -> Result<(), ReserveError> {
        if additional == 0 {
            return Ok(());
        }
        let total = self.len().checked_add(additional).ok_or(ReserveError)?;
        if let Some(rows) = Arc::get_mut(&mut self.0) {
            return if exact {
                rows.try_reserve_exact(additional)
            } else {
                rows.try_reserve(additional)
            }
            .map_err(|_| ReserveError);
        }
        let mut rows = Vec::new();
        if exact {
            rows.try_reserve_exact(total)
        } else {
            rows.try_reserve(total)
        }
        .map_err(|_| ReserveError)?;
        rows.extend_from_slice(&self.0);
        self.0 = Arc::new(rows);
        Ok(())
    }

    pub(crate) fn push(&mut self, row: T) {
        Arc::make_mut(&mut self.0).push(row);
    }

    pub(crate) fn append(&mut self, rows: &mut Vec<T>) {
        // Empty compiled shapes carry no segments and must not detach an
        // otherwise shared table. Nonempty appends follow try_reserve_exact.
        if !rows.is_empty() {
            Arc::make_mut(&mut self.0).append(rows);
        }
    }
}

#[derive(Debug)]
pub(crate) struct SharedIndex<K, V>(Arc<HashMap<K, V>>);

impl<K, V> Default for SharedIndex<K, V> {
    fn default() -> Self {
        Self(Arc::new(HashMap::new()))
    }
}

impl<K, V> Clone for SharedIndex<K, V> {
    fn clone(&self) -> Self {
        Self(Arc::clone(&self.0))
    }
}

impl<K, V> Deref for SharedIndex<K, V> {
    type Target = HashMap<K, V>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<K, V> SharedIndex<K, V> {
    pub(crate) fn clear(&mut self) {
        match Arc::get_mut(&mut self.0) {
            Some(index) => index.clear(),
            None => *self = Self::default(),
        }
    }
}

impl<K: Eq + Hash + Clone, V: Clone> SharedIndex<K, V> {
    pub(crate) fn try_reserve(&mut self, additional: usize) -> Result<(), ReserveError> {
        if additional == 0 {
            return Ok(());
        }
        let total = self.len().checked_add(additional).ok_or(ReserveError)?;
        if let Some(index) = Arc::get_mut(&mut self.0) {
            return index.try_reserve(additional).map_err(|_| ReserveError);
        }
        let mut index = HashMap::new();
        index.try_reserve(total).map_err(|_| ReserveError)?;
        index.extend(self.0.iter().map(|(key, value)| (key.clone(), value.clone())));
        self.0 = Arc::new(index);
        Ok(())
    }

    pub(crate) fn insert(&mut self, key: K, value: V) -> Option<V> {
        Arc::make_mut(&mut self.0).insert(key, value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows_share_reads_and_zero_reservations_but_detach_before_append() {
        let mut rows = SharedRows::default();
        rows.try_reserve_exact(3).unwrap();
        for value in [10, 20, 30] {
            rows.push(value);
        }
        let frozen = rows.clone();
        rows.try_reserve(0).unwrap();
        rows.try_reserve_exact(0).unwrap();
        rows.append(&mut Vec::new());
        assert!(Arc::ptr_eq(&rows.0, &frozen.0));
        rows.try_reserve_exact(1).unwrap();
        assert!(!Arc::ptr_eq(&rows.0, &frozen.0));
        let mut pending = vec![40];
        rows.append(&mut pending);
        assert!(pending.is_empty());
        assert_eq!(&*rows, &[10, 20, 30, 40]);
        assert_eq!(&*frozen, &[10, 20, 30]);
        rows.clear();
        assert!(rows.is_empty());
        assert_eq!(&*frozen, &[10, 20, 30]);
    }

    #[test]
    fn index_forks_preserve_old_lookups_and_independent_clear() {
        let mut index = SharedIndex::default();
        index.try_reserve(1).unwrap();
        index.insert(vec![1, 2], 7);
        let frozen = index.clone();
        index.try_reserve(0).unwrap();
        assert!(Arc::ptr_eq(&index.0, &frozen.0));
        index.try_reserve(1).unwrap();
        index.insert(vec![3, 4], 8);
        assert_eq!(index.get(&vec![1, 2]), Some(&7));
        assert_eq!(frozen.get(&vec![3, 4]), None);
        let mut cleared = frozen.clone();
        cleared.clear();
        assert!(cleared.is_empty());
        assert_eq!(frozen.get(&vec![1, 2]), Some(&7));
    }

    #[test]
    fn impossible_reservations_leave_shared_generations_unchanged() {
        let mut rows = SharedRows::default();
        rows.push(1_u64);
        let frozen = rows.clone();
        assert!(rows.try_reserve_exact(usize::MAX).is_err());
        assert!(Arc::ptr_eq(&rows.0, &frozen.0));
        assert!(rows.try_reserve_exact(usize::MAX / 8).is_err());
        assert!(Arc::ptr_eq(&rows.0, &frozen.0));
        assert_eq!(&*rows, &[1]);
        let mut index = SharedIndex::default();
        index.insert(1_u64, 2_u64);
        let frozen = index.clone();
        assert!(index.try_reserve(usize::MAX).is_err());
        assert!(Arc::ptr_eq(&index.0, &frozen.0));
        assert_eq!(index.get(&1), Some(&2));
    }
}

//! A copy-on-write vector whose clones share one allocation.
//!
//! [`AssistantMessage::content`](crate::types::AssistantMessage::content) uses it so that a streamed
//! partial message can be handed to every event, listener and state slot without copying its
//! text: TypeScript shares one mutable `partial` object, and a [`SharedVec`] clone is the
//! immutable equivalent. Mutation goes through [`Arc::make_mut`], so it copies the elements only
//! while another clone is alive.

use std::fmt;
use std::ops::{Deref, DerefMut};
use std::sync::Arc;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// A `Vec<T>` behind an [`Arc`]: cloning is O(1), the first mutation of a shared value copies it.
///
/// Reads go through `Deref<Target = Vec<T>>`; writes through `DerefMut`, which detaches the value
/// from its other clones first. Serializes exactly like the `Vec<T>` it holds.
pub struct SharedVec<T>(Arc<Vec<T>>);

impl<T> SharedVec<T> {
    /// An empty vector.
    pub fn new() -> Self {
        Self(Arc::new(Vec::new()))
    }

    /// Whether `self` and `other` share one allocation (so they are equal without comparing).
    pub fn ptr_eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl<T: Clone> SharedVec<T> {
    /// The elements, copied only when another clone shares them.
    pub fn into_vec(self) -> Vec<T> {
        Arc::unwrap_or_clone(self.0)
    }
}

impl<T> Default for SharedVec<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T> Clone for SharedVec<T> {
    fn clone(&self) -> Self {
        Self(Arc::clone(&self.0))
    }
}

impl<T> Deref for SharedVec<T> {
    type Target = Vec<T>;
    fn deref(&self) -> &Vec<T> {
        &self.0
    }
}

impl<T: Clone> DerefMut for SharedVec<T> {
    fn deref_mut(&mut self) -> &mut Vec<T> {
        Arc::make_mut(&mut self.0)
    }
}

impl<T: fmt::Debug> fmt::Debug for SharedVec<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.as_slice().fmt(f)
    }
}

impl<T: PartialEq> PartialEq for SharedVec<T> {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0) || self.0 == other.0
    }
}

impl<T: Eq> Eq for SharedVec<T> {}

impl<T: PartialEq> PartialEq<Vec<T>> for SharedVec<T> {
    fn eq(&self, other: &Vec<T>) -> bool {
        *self.0 == *other
    }
}

impl<T: PartialEq> PartialEq<SharedVec<T>> for Vec<T> {
    fn eq(&self, other: &SharedVec<T>) -> bool {
        *self == *other.0
    }
}

impl<T> From<Vec<T>> for SharedVec<T> {
    fn from(vec: Vec<T>) -> Self {
        Self(Arc::new(vec))
    }
}

impl<T: Clone> From<SharedVec<T>> for Vec<T> {
    fn from(shared: SharedVec<T>) -> Self {
        shared.into_vec()
    }
}

impl<T> FromIterator<T> for SharedVec<T> {
    fn from_iter<I: IntoIterator<Item = T>>(iter: I) -> Self {
        Self(Arc::new(iter.into_iter().collect()))
    }
}

impl<T: Clone> Extend<T> for SharedVec<T> {
    fn extend<I: IntoIterator<Item = T>>(&mut self, iter: I) {
        Arc::make_mut(&mut self.0).extend(iter);
    }
}

impl<T: Clone> IntoIterator for SharedVec<T> {
    type Item = T;
    type IntoIter = std::vec::IntoIter<T>;
    fn into_iter(self) -> Self::IntoIter {
        self.into_vec().into_iter()
    }
}

impl<'a, T> IntoIterator for &'a SharedVec<T> {
    type Item = &'a T;
    type IntoIter = std::slice::Iter<'a, T>;
    fn into_iter(self) -> Self::IntoIter {
        self.0.iter()
    }
}

impl<'a, T: Clone> IntoIterator for &'a mut SharedVec<T> {
    type Item = &'a mut T;
    type IntoIter = std::slice::IterMut<'a, T>;
    fn into_iter(self) -> Self::IntoIter {
        Arc::make_mut(&mut self.0).iter_mut()
    }
}

impl<T: Serialize> Serialize for SharedVec<T> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.0.as_slice().serialize(serializer)
    }
}

impl<'de, T: Deserialize<'de>> Deserialize<'de> for SharedVec<T> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Vec::<T>::deserialize(deserializer).map(Self::from)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clones_share_until_one_is_mutated() {
        let mut original: SharedVec<String> = vec!["a".to_string()].into();
        let snapshot = original.clone();
        assert!(original.ptr_eq(&snapshot));
        original.push("b".into());
        assert!(!original.ptr_eq(&snapshot));
        assert_eq!(snapshot, vec!["a".to_string()]);
        assert_eq!(original, vec!["a".to_string(), "b".to_string()]);
        // A value nobody else shares is mutated in place.
        let before = original.as_ptr();
        original[0].push('!');
        assert_eq!(original.as_ptr(), before);
    }

    #[test]
    fn serializes_like_the_vec() {
        let shared: SharedVec<u32> = vec![1, 2, 3].into();
        assert_eq!(serde_json::to_string(&shared).unwrap(), "[1,2,3]");
        let back: SharedVec<u32> = serde_json::from_str("[1,2,3]").unwrap();
        assert_eq!(back, shared);
    }
}

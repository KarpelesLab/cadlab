//! Stable internal identifiers.
//!
//! Every model object gets an [`ObjectId`] that never changes, even on rename. Humans and agents
//! use names instead (see [`crate::refs`]); IDs stay internal and appear in files and undo state.

use std::fmt;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Opaque, project-unique object identifier. Never reused within a project.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(transparent)]
pub struct ObjectId(pub u64);

impl fmt::Display for ObjectId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "#{}", self.0)
    }
}

/// Allocates [`ObjectId`]s. Persisted with the project so IDs are never reused, even after the
/// object holding the highest ID is deleted.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(transparent)]
pub struct IdAllocator {
    next: u64,
}

impl Default for IdAllocator {
    fn default() -> Self {
        IdAllocator { next: 1 }
    }
}

impl IdAllocator {
    /// Returns a fresh ID.
    pub fn alloc(&mut self) -> ObjectId {
        let id = ObjectId(self.next);
        self.next += 1;
        id
    }

    /// The next ID that will be returned.
    pub fn peek(&self) -> ObjectId {
        ObjectId(self.next)
    }

    /// Ensures future IDs are greater than `id` (used when loading or importing data).
    pub fn reserve_through(&mut self, id: ObjectId) {
        self.next = self.next.max(id.0 + 1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allocates_monotonic() {
        let mut a = IdAllocator::default();
        assert_eq!(a.alloc(), ObjectId(1));
        assert_eq!(a.alloc(), ObjectId(2));
        a.reserve_through(ObjectId(10));
        assert_eq!(a.alloc(), ObjectId(11));
        a.reserve_through(ObjectId(3));
        assert_eq!(a.alloc(), ObjectId(12));
    }
}

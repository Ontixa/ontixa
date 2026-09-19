//! String interning.
//!
//! Identifiers and other repeated short strings are interned so that
//! semantic structures store a `u32` instead of a string. Interning starts
//! at the HIR boundary: the CST/AST keeps source text, and lowering to HIR
//! interns each distinct name once. This keeps AST serialization simple
//! while giving semantic passes cheap `Copy` keys.
//!
//! The interner is not thread-safe by itself; milestone 1 compilation is
//! single-threaded. `ontixad` will either shard interners per session or
//! gate them behind synchronization — see `docs/architecture.md`.

use crate::ids::InternId;
use rustc_hash::FxHashMap;
use std::fmt;

/// Maps distinct strings to dense [`InternId`]s.
#[derive(Default, Clone)]
pub struct Interner {
    map: FxHashMap<Box<str>, InternId>,
    strings: Vec<Box<str>>,
}

impl Interner {
    /// Creates an empty interner.
    pub fn new() -> Self {
        Self::default()
    }

    /// Interns `text`, returning the existing ID when already interned.
    pub fn intern(&mut self, text: &str) -> InternId {
        if let Some(id) = self.map.get(text) {
            return *id;
        }
        let id = InternId::new(self.strings.len() as u32);
        let owned: Box<str> = text.into();
        self.strings.push(owned.clone());
        self.map.insert(owned, id);
        id
    }

    /// Resolves text to an ID without interning — `None` when the text
    /// was never interned. Read-only lookups use this.
    pub fn get(&self, text: &str) -> Option<InternId> {
        self.map.get(text).copied()
    }

    /// Resolves an ID back to its text. Panics for out-of-range IDs —
    /// an internal invariant violation, never reachable from user input.
    pub fn resolve(&self, id: InternId) -> &str {
        &self.strings[id.index()]
    }

    /// Number of interned strings.
    pub fn len(&self) -> usize {
        self.strings.len()
    }

    /// Whether the interner is empty.
    pub fn is_empty(&self) -> bool {
        self.strings.is_empty()
    }
}

impl fmt::Debug for Interner {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Interner")
            .field("len", &self.strings.len())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interning_is_stable_and_dedupes() {
        let mut i = Interner::new();
        let a = i.intern("alpha");
        let b = i.intern("beta");
        let a2 = i.intern("alpha");
        assert_eq!(a, a2);
        assert_ne!(a, b);
        assert_eq!(i.resolve(a), "alpha");
        assert_eq!(i.resolve(b), "beta");
        assert_eq!(i.len(), 2);
    }
}

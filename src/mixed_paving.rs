use std::{
    ops::{Deref, DerefMut},
    rc::Rc,
};

use crate::{BumpMember, Paving, RcBumpMember};

/// A pointer to a mixed paving owning its pointee
pub enum OwnedMixedPavingMember<T> {
    /// The object was allocated in a bump
    BumpMember(BumpMember<T>),
    /// The object is allocated on its own
    Box(Box<T>),
}

impl<T> DerefMut for OwnedMixedPavingMember<T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        match self {
            OwnedMixedPavingMember::BumpMember(sm) => sm,
            OwnedMixedPavingMember::Box(b) => b,
        }
    }
}

impl<T> Deref for OwnedMixedPavingMember<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        match self {
            OwnedMixedPavingMember::BumpMember(sm) => sm,
            OwnedMixedPavingMember::Box(b) => b,
        }
    }
}

/// A pointer to a mixed paving sharing ownership of its pointee
pub enum SharedMixedPavingMember<T> {
    /// The object was allocated in a bump
    RcBumpMember(RcBumpMember<T>),
    /// The object is allocated on its own
    Rc(Rc<T>),
}

impl<T> Deref for SharedMixedPavingMember<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        match self {
            SharedMixedPavingMember::RcBumpMember(sm) => sm,
            SharedMixedPavingMember::Rc(rc) => rc,
        }
    }
}

/// A paving which will allocate objects too large out of any bump
pub struct MixedPaving(Paving);

impl MixedPaving {
    /// Creates a new mixed paving whose backing bumps will have the corresponding
    /// capacity and align.
    ///
    /// See [`Bump::new`](`crate::Bump::new`).
    pub fn new(capacity: usize, align: usize) -> Self {
        Self(Paving::new(capacity, align))
    }

    /// Alloc an object returning an owning pointer
    pub fn alloc<T>(&self, value: T) -> OwnedMixedPavingMember<T> {
        match self.0.try_alloc(value) {
            Ok(sm) => OwnedMixedPavingMember::BumpMember(sm),
            Err(val) => OwnedMixedPavingMember::Box(Box::new(val)),
        }
    }

    /// Alloc an object return an shareable pointer
    pub fn alloc_rc<T>(&self, value: T) -> SharedMixedPavingMember<T> {
        match self.0.try_alloc_rc(value) {
            Ok(sm) => SharedMixedPavingMember::RcBumpMember(sm),
            Err(val) => SharedMixedPavingMember::Rc(Rc::new(val)),
        }
    }
}

use std::{cell::UnsafeCell, mem::size_of};

use crate::{Bump, BumpMember, RcBumpMember};

/// A structure generating bumps as appropriated
pub struct Paving {
    capacity: usize,
    align: usize,
    current_bump: UnsafeCell<Bump>,
}

impl Paving {
    /// Creates a new paving, which will be backed by bumps
    /// created with correponding capacity and align.
    ///
    /// See [`Bump::new`]
    pub fn new(capacity: usize, align: usize) -> Self {
        let first_bump = Bump::new(capacity, align);
        Self {
            capacity,
            align,
            current_bump: first_bump.into(),
        }
    }

    /// Try to allocate an object in the paving
    ///
    /// Fails if no bump big enough can be created to accomodate
    /// the object
    pub fn try_alloc<T>(&self, value: T) -> Result<BumpMember<T>, T> {
        if size_of::<T>() * 2 > self.capacity {
            return Err(value);
        }

        // Safety: there is no other active reference
        match unsafe { (*self.current_bump.get()).try_alloc(value) } {
            Ok(sm) => Ok(sm),
            Err(value) => {
                // Safety: there is no other active reference
                unsafe { *self.current_bump.get() = Bump::new(self.capacity, self.align) };
                // Safety: there is no other active reference
                let res = unsafe { (*self.current_bump.get()).try_alloc(value) };
                debug_assert!(res.is_ok());
                res
            }
        }
    }

    /// Try to allocate a object with shared ownership in the bump.
    ///
    /// Fails if no bump big enough can be created to accomodate
    /// the object
    pub fn try_alloc_rc<T>(&self, value: T) -> Result<RcBumpMember<T>, T> {
        if size_of::<T>() * 2 > self.capacity {
            return Err(value);
        }

        // Safety: there is no other active reference
        match unsafe { (*self.current_bump.get()).try_alloc_rc(value) } {
            Ok(sm) => Ok(sm),
            Err(value) => {
                // Safety: there is no other active reference
                unsafe { *self.current_bump.get() = Bump::new(self.capacity, self.align) };
                // Safety: there is no other active reference
                let res = unsafe { (*self.current_bump.get()).try_alloc_rc(value) };
                debug_assert!(res.is_ok());
                res
            }
        }
    }
}

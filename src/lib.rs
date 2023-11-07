#![warn(missing_docs)]
#![warn(
    clippy::undocumented_unsafe_blocks,
    clippy::missing_safety_doc,
    clippy::multiple_unsafe_ops_per_block
)]
#![warn(clippy::cast_lossless)]

//! This crate offers fast and locality-aware allocation
//! similar to bumpalo but without using lifetimes, relying
//! instead on reference counting.

use std::{
    alloc::{alloc, dealloc, Layout, LayoutError},
    cell::{Cell, UnsafeCell},
    marker::PhantomData,
    mem::{align_of, needs_drop, size_of},
    ops::{Deref, DerefMut},
    ptr::{drop_in_place, NonNull},
    rc::Rc,
};

struct Metadata {
    count: u64,
    beg: NonNull<u8>,
    layout: Layout,
}

impl Metadata {
    // # Safety
    // - sself must not be dangling
    // - No live reference to sself pointee must exist
    unsafe fn decrement_and_drop(mut sself: NonNull<Self>) {
        sself.as_mut().count -= 1;
        if sself.as_ref().count == 0 {
            // It is ok to dealloc because nobody references this chunk
            // anymore
            dealloc(sself.as_ref().beg.as_ptr(), sself.as_ref().layout)
        }
    }
}

/// A zone of memory to allocate into.
pub struct Bump {
    metadata: NonNull<Metadata>,
    first_free: Cell<NonNull<u8>>,
}

/// A pointer to a [`Bump`] owning the underlying object,
/// like a Box.
///
/// The obejct will be dropped when the pointer is dropped.
pub struct BumpMember<T> {
    metadata: NonNull<Metadata>,
    data: NonNull<T>,
}

impl<T> Deref for BumpMember<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        // # Safety:
        // self.data is aligned, valid,
        // and can only be accessed from BumpMember
        unsafe { self.data.as_ref() }
    }
}

impl<T> DerefMut for BumpMember<T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        // # Safety:
        // self.data is aligned, valid,
        // and can only be accessed from BumpMember
        // Which cannot be cloned
        unsafe { self.data.as_mut() }
    }
}

struct BumpRcEntry<T> {
    count: usize,
    value: T,
}

enum NeedsDrop<T> {
    Yes(NonNull<BumpRcEntry<T>>),
    No(NonNull<T>),
}

impl<T> NeedsDrop<T> {
    fn from_rc_data(rc_data: NonNull<u8>) -> NeedsDrop<T> {
        if needs_drop::<T>() {
            NeedsDrop::Yes(rc_data.cast())
        } else {
            NeedsDrop::No(rc_data.cast())
        }
    }
}

/// A pointer to a [`Bump`] offering shared ownership of
/// the pointed object, similar to [`std::rc::Rc`].
///
/// The object is dropped once all pointers are dropped.
///
/// If `!T::needs_drop()`, most of the dropping code for
/// the `T` itself is optimized away.
pub struct RcBumpMember<T> {
    metadata: NonNull<Metadata>,
    rc_data: NonNull<u8>,
    _marker: PhantomData<T>,
}

impl<T> RcBumpMember<T> {
    fn rc_data(&self) -> NeedsDrop<T> {
        NeedsDrop::from_rc_data(self.rc_data)
    }
}

fn inner_bump_layout(capacity: usize, align: usize) -> Result<(Layout, usize), LayoutError> {
    Layout::from_size_align(capacity, align)?.extend(Layout::new::<Metadata>())
}

struct RawBumpMember<T> {
    metadata: NonNull<Metadata>,
    data: NonNull<T>,
}

impl Bump {
    /// Create a new Bump.
    ///
    /// # Arguments
    ///
    /// capacity: the capacity in bytes of the bump
    ///
    /// alignment: an indicative alignment for the
    /// first object of the bump
    pub fn new(capacity: usize, align: usize) -> Self {
        if capacity == 0 {
            panic!("Trying to create a Bump with null capacity")
        }
        let (layout, metadata_offset) = inner_bump_layout(capacity, align).unwrap();
        // # Safety:
        // layout has a non zero size
        let inner_ptr = unsafe { alloc(layout) };
        if inner_ptr.is_null() {
            panic!("Memory allocation failed")
        }
        let metadata_ptr = {
            // # Safety:
            // metadat_offset and inner_ptr result from the same Layout::extend call
            let metadata_ptr = unsafe { inner_ptr.add(metadata_offset) };
            let metadata_ptr = metadata_ptr.cast::<Metadata>();
            // # Safety:
            // metadata is not null
            unsafe { NonNull::new_unchecked(metadata_ptr) }
        };
        // Safety: inner_ptr has been tested to be non zero
        let first_free = unsafe { NonNull::new_unchecked(inner_ptr) };
        let metadata = Metadata {
            count: 1,
            beg: first_free,
            layout,
        };
        // Safety: metadata_ptr comes from Layout::extend in
        // inner_bump_layout and is valid to write Metadata to
        unsafe { metadata_ptr.as_ptr().write(metadata) }
        Bump {
            metadata: metadata_ptr,
            first_free: first_free.into(),
        }
    }

    // Returns two pointers:
    // - first one is valid to write T
    // - second one will be the new first free
    // Both are in the same allocated object
    fn can_fit<T>(&self) -> Option<(*mut T, *mut u8)> {
        let first_free: *mut u8 = self.first_free.get().as_ptr();
        let align_offset: usize = first_free.align_offset(align_of::<T>());
        let tentative_start: usize = (first_free as usize).checked_add(align_offset)?;
        let tentative_end: usize = tentative_start.checked_add(size_of::<T>())?;
        if tentative_end <= self.metadata.as_ptr() as usize {
            // Safety:
            // Because operations were done without overflow:
            // tentative_end = first_free + align_offset + size_of<T>
            // and tentative_and <= self.metadata
            // implies:
            // -  Both pointers are in the same allocation
            // - Sum fits a usize
            // Because it was done in an allocation from one Layout,
            // the offset between the two pointer, and even first_free
            // and tentative_end cannot be greater than isize::MAX
            let beg = unsafe { first_free.add(align_offset) };
            // Safety: same as above
            let end = unsafe { beg.add(size_of::<T>()) };
            Some((beg.cast(), end))
        } else {
            None
        }
    }

    fn try_alloc_inner<T>(&self, value: T) -> Result<RawBumpMember<T>, T> {
        let (start, end): (*mut T, *mut u8) = match self.can_fit::<T>() {
            Some(res) => res,
            None => return Err(value),
        };
        // Safety:
        // - start is valid for writes (see can_fit)
        unsafe { start.write(value) };
        // Safety: start is non zero
        let start = unsafe { NonNull::new_unchecked(start) };
        // Safety:
        // - metadata is valid for writes
        unsafe { (*self.metadata.as_ptr()).count += 1 }
        // Safety:
        // - can_fit returns a non zero pointer
        let new_end: NonNull<u8> = unsafe { NonNull::new_unchecked(end) };
        self.first_free.set(new_end);
        let res = RawBumpMember {
            metadata: self.metadata,
            data: start,
        };
        Ok(res)
    }

    /// Try to allocate an object in the bump
    ///
    /// Fails if there is not enough memory left
    pub fn try_alloc<T>(&self, value: T) -> Result<BumpMember<T>, T> {
        let RawBumpMember { metadata, data } = self.try_alloc_inner(value)?;
        Ok(BumpMember { metadata, data })
    }

    /// Try to allocate a object with shared ownership in the bump.
    ///
    /// Fails if there is not enough memory left
    pub fn try_alloc_rc<T>(&self, value: T) -> Result<RcBumpMember<T>, T> {
        if needs_drop::<T>() {
            let RawBumpMember { metadata, data } = self
                .try_alloc_inner(BumpRcEntry { count: 1, value })
                .map_err(|srce| srce.value)?;
            Ok(RcBumpMember {
                metadata,
                rc_data: data.cast(),
                _marker: PhantomData,
            })
        } else {
            let RawBumpMember { metadata, data } = self.try_alloc_inner(value)?;
            Ok(RcBumpMember {
                metadata,
                rc_data: data.cast(),
                _marker: PhantomData,
            })
        }
    }
}

impl Drop for Bump {
    fn drop(&mut self) {
        // Safety:
        // No other reference to metadata currently exists
        // (only pointers)
        unsafe { Metadata::decrement_and_drop(self.metadata) };
    }
}

impl<T> Drop for BumpMember<T> {
    fn drop(&mut self) {
        // Safety:
        // We are the only access to BumpMember
        // which owns the T
        // The pointer is valid for read and writes
        // and non zero
        unsafe {
            drop_in_place(self.data.as_ptr());
        }
        // Safety:
        // No other reference to metadata currently exists
        // (only pointers)
        unsafe {
            Metadata::decrement_and_drop(self.metadata);
        }
    }
}

// #[allow(unused)]
// struct RcMdBumpMember<T> {
//     metadata: NonNull<Metadata>,
//     data: NonNull<T>,
// }

// impl<T> Deref for RcMdBumpMember<T> {
//     type Target = T;

//     fn deref(&self) -> &Self::Target {
//         // Safety:
//         //
//         unsafe { self.data.as_ref() }
//     }
// }

// impl<T> Drop for RcMdBumpMember<T> {
//     fn drop(&mut self) {
//         unsafe {
//             Metadata::decrement_and_drop(self.metadata);
//         }
//     }
// }

// impl<T> BumpMember<T> {
//     #[allow(unused)]
//     fn into_rcmd(self) -> RcMdBumpMember<T> {
//         RcMdBumpMember {
//             metadata: self.metadata,
//             data: self.data,
//         }
//     }
// }

impl<T> Deref for RcBumpMember<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        match self.rc_data() {
            // Safety: self contains a valid data entry
            NeedsDrop::Yes(rc_entry) => unsafe { &rc_entry.as_ref().value },
            // Safety: self contains a valid data entry
            NeedsDrop::No(value) => unsafe { value.as_ref() },
        }
    }
}

impl<T> Drop for RcBumpMember<T> {
    fn drop(&mut self) {
        match self.rc_data() {
            NeedsDrop::Yes(mut rc_entry) => {
                // Safety: rc_entry points to a valid BumpRcEntry
                unsafe { rc_entry.as_mut().count -= 1 };
                // Safety: rc_entry points to a valid BumpRcEntry
                if unsafe { rc_entry.as_ref().count == 0 } {
                    #[allow(clippy::multiple_unsafe_ops_per_block)]
                    // Safety: rc entry points to valid data
                    unsafe {
                        drop_in_place(&mut (*rc_entry.as_ptr()).value)
                    };
                    // Safety:
                    // No other reference to metadata currently exists
                    // (only pointers)
                    unsafe { Metadata::decrement_and_drop(self.metadata) };
                }
            }
            // Safety:
            // No other reference to metadata currently exists
            // (only pointers)
            NeedsDrop::No(_) => unsafe { Metadata::decrement_and_drop(self.metadata) },
        }
    }
}

impl<T> Clone for RcBumpMember<T> {
    fn clone(&self) -> Self {
        match self.rc_data() {
            // Safety: self contains a valid rc_data entry
            NeedsDrop::Yes(mut rc_data) => unsafe { rc_data.as_mut().count += 1 },
            // Safety: metadata is valid
            NeedsDrop::No(_) => unsafe { (*self.metadata.as_ptr()).count += 1 },
        }
        Self {
            metadata: self.metadata,
            rc_data: self.rc_data,
            _marker: PhantomData,
        }
    }
}

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
    /// See [`Bump::new`]
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

#[cfg(test)]
mod test {
    use std::mem::{align_of, size_of};

    use crate::{Bump, Paving};

    #[test]
    fn test_creation_bump() {
        {
            let mut bump_member1;
            let bump_member2;
            {
                let bump = Bump::new(2 * size_of::<u64>(), align_of::<u64>());
                bump_member1 = bump.try_alloc(123_u64).unwrap();
                bump_member2 = bump.try_alloc(456_u64).unwrap();
            }
            assert_eq!(*bump_member2, 456);
            assert_eq!(*bump_member1, 123);
            *bump_member1 += 1;
            assert_eq!(*bump_member1, 124);
        }
    }

    #[test]
    fn test_creation_paving() {
        {
            let bump_member1;
            let bump_member2;
            {
                let bump = Paving::new(2 * size_of::<u64>(), align_of::<u64>());
                bump_member1 = bump.try_alloc(123_u64).unwrap();
                bump.try_alloc(0_u64).unwrap();
                bump.try_alloc(0_u64).unwrap();
                bump_member2 = bump.try_alloc(456_u64).unwrap();
            }
            assert_eq!(*bump_member1, 123);
            assert_eq!(*bump_member2, 456);
        }
    }
}

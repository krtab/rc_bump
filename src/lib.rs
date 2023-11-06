#![warn(missing_docs)]

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
    unsafe fn decrement_and_drop(mut sself: NonNull<Self>) {
        sself.as_mut().count -= 1;
        if sself.as_mut().count == 0 {
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
        unsafe { self.data.as_ref() }
    }
}

impl<T> DerefMut for BumpMember<T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
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
        let (layout, metadata_offset) = inner_bump_layout(capacity, align).unwrap();
        let inner_ptr = unsafe { alloc(layout) };
        let metadata =
            unsafe { NonNull::new_unchecked(inner_ptr.add(metadata_offset).cast::<Metadata>()) };
        let first_free = unsafe { NonNull::new_unchecked(inner_ptr) };
        unsafe {
            metadata.as_ptr().write(Metadata {
                count: 1,
                beg: first_free,
                layout,
            })
        }
        Bump {
            metadata,
            first_free: first_free.into(),
        }
    }

    fn can_fit<T>(&self) -> Option<(*mut T, *mut u8)> {
        let first_free_offset = self.first_free.get().as_ptr();
        let align_offset = first_free_offset.align_offset(align_of::<T>());
        let tentative_start = (first_free_offset as usize).checked_add(align_offset)?;
        let tentative_end = tentative_start.checked_add(size_of::<T>())?;
        if tentative_end <= self.metadata.as_ptr() as usize {
            unsafe {
                let beg = first_free_offset.add(align_offset);
                Some((beg.cast(), beg.add(size_of::<T>())))
            }
        } else {
            None
        }
    }

    fn try_alloc_inner<T>(&self, value: T) -> Result<RawBumpMember<T>, T> {
        let (start, end) = match self.can_fit::<T>() {
            Some(res) => res,
            None => return Err(value),
        };
        unsafe { start.write(value) };
        unsafe {
            (*self.metadata.as_ptr()).count += 1;
            self.first_free.set(NonNull::new_unchecked(end));
        };
        let res = RawBumpMember {
            metadata: self.metadata,
            data: unsafe { NonNull::new_unchecked(start) },
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
        unsafe { Metadata::decrement_and_drop(self.metadata) };
    }
}

impl<T> Drop for BumpMember<T> {
    fn drop(&mut self) {
        unsafe {
            drop_in_place(self.data.as_mut());
            Metadata::decrement_and_drop(self.metadata);
        }
    }
}


#[allow(unused)]
struct RcMdBumpMember<T> {
    metadata: NonNull<Metadata>,
    data: NonNull<T>,
}

impl<T> Deref for RcMdBumpMember<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        unsafe { self.data.as_ref() }
    }
}

impl<T> Drop for RcMdBumpMember<T> {
    fn drop(&mut self) {
        unsafe {
            Metadata::decrement_and_drop(self.metadata);
        }
    }
}

impl<T> BumpMember<T> {
    #[allow(unused)]
    fn into_rcmd(self) -> RcMdBumpMember<T> {
        RcMdBumpMember {
            metadata: self.metadata,
            data: self.data,
        }
    }
}

impl<T> Deref for RcBumpMember<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        unsafe {
            match self.rc_data() {
                NeedsDrop::Yes(rc_data) => &rc_data.as_ref().value,
                NeedsDrop::No(value) => value.as_ref(),
            }
        }
    }
}

impl<T> Drop for RcBumpMember<T> {
    fn drop(&mut self) {
        unsafe {
            match self.rc_data() {
                NeedsDrop::Yes(mut rc_data) => {
                    rc_data.as_mut().count -= 1;
                    if rc_data.as_ref().count == 0 {
                        drop_in_place(&mut rc_data.as_mut().value);
                        Metadata::decrement_and_drop(self.metadata);
                    }
                }
                NeedsDrop::No(_) => Metadata::decrement_and_drop(self.metadata),
            }
        }
    }
}

impl<T> Clone for RcBumpMember<T> {
    fn clone(&self) -> Self {
        unsafe {
            match self.rc_data() {
                NeedsDrop::Yes(mut rc_data) => rc_data.as_mut().count += 1,
                NeedsDrop::No(_) => (*self.metadata.as_ptr()).count += 1,
            }
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
        unsafe {
            match (*self.current_bump.get()).try_alloc(value) {
                Ok(sm) => Ok(sm),
                Err(value) => {
                    *self.current_bump.get() = Bump::new(self.capacity, self.align);
                    let res = (*self.current_bump.get()).try_alloc(value);
                    debug_assert!(res.is_ok());
                    res
                }
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
        unsafe {
            match (*self.current_bump.get()).try_alloc_rc(value) {
                Ok(sm) => Ok(sm),
                Err(value) => {
                    *self.current_bump.get() = Bump::new(self.capacity, self.align);
                    let res = (*self.current_bump.get()).try_alloc_rc(value);
                    debug_assert!(res.is_ok());
                    res
                }
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

    use crate::{Paving, Bump};

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

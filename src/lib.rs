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
pub struct Slab {
    metadata: NonNull<Metadata>,
    first_free: Cell<NonNull<u8>>,
}

/// A pointer to a [`Slab`] owning the underlying object,
/// like a Box.
/// 
/// The obejct will be dropped when the pointer is dropped.
pub struct SlabMember<T> {
    metadata: NonNull<Metadata>,
    data: NonNull<T>,
}

impl<T> Deref for SlabMember<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        unsafe { self.data.as_ref() }
    }
}

impl<T> DerefMut for SlabMember<T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        unsafe { self.data.as_mut() }
    }
}

struct SlabRcEntry<T> {
    count: usize,
    value: T,
}

enum NeedsDrop<T> {
    Yes(NonNull<SlabRcEntry<T>>),
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

/// A pointer to a [`Slab`] offering shared ownership of
/// the pointed object, similar to [`std::rc::Rc`].
/// 
/// The object is dropped once all pointers are dropped.
/// 
/// If `!T::needs_drop()`, most of the dropping code for
/// the `T` itself is optimized away.
pub struct RcSlabMember<T> {
    metadata: NonNull<Metadata>,
    rc_data: NonNull<u8>,
    _marker: PhantomData<T>,
}

impl<T> RcSlabMember<T> {
    fn rc_data(&self) -> NeedsDrop<T> {
        NeedsDrop::from_rc_data(self.rc_data)
    }
}

fn inner_slab_layout(capacity: usize, align: usize) -> Result<(Layout, usize), LayoutError> {
    Layout::from_size_align(capacity, align)?.extend(Layout::new::<Metadata>())
}

struct RawSlabMember<T> {
    metadata: NonNull<Metadata>,
    data: NonNull<T>,
}

impl Slab {
    /// Create a new Slab.
    /// 
    /// # Arguments
    /// 
    /// capacity: the capacity in bytes of the slab
    /// 
    /// alignment: an indicative alignment for the
    /// first object of the slab
    pub fn new(capacity: usize, align: usize) -> Self {
        let (layout, metadata_offset) = inner_slab_layout(capacity, align).unwrap();
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
        Slab {
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

    fn try_alloc_inner<T>(&self, value: T) -> Result<RawSlabMember<T>, T> {
        let (start, end) = match self.can_fit::<T>() {
            Some(res) => res,
            None => return Err(value),
        };
        unsafe { start.write(value) };
        unsafe {
            (*self.metadata.as_ptr()).count += 1;
            self.first_free.set(NonNull::new_unchecked(end));
        };
        let res = RawSlabMember {
            metadata: self.metadata,
            data: unsafe { NonNull::new_unchecked(start) },
        };
        Ok(res)
    }

    /// Try to allocate an object in the slab
    /// 
    /// Fails if there is not enough memory left
    pub fn try_alloc<T>(&self, value: T) -> Result<SlabMember<T>, T> {
        let RawSlabMember { metadata, data } = self.try_alloc_inner(value)?;
        Ok(SlabMember { metadata, data })
    }

    /// Try to allocate a object with shared ownership in the slab.
    /// 
    /// Fails if there is not enough memory left
    pub fn try_alloc_rc<T>(&self, value: T) -> Result<RcSlabMember<T>, T> {
        if needs_drop::<T>() {
            let RawSlabMember { metadata, data } = self
                .try_alloc_inner(SlabRcEntry { count: 1, value })
                .map_err(|srce| srce.value)?;
            Ok(RcSlabMember {
                metadata,
                rc_data: data.cast(),
                _marker: PhantomData,
            })
        } else {
            let RawSlabMember { metadata, data } = self.try_alloc_inner(value)?;
            Ok(RcSlabMember {
                metadata,
                rc_data: data.cast(),
                _marker: PhantomData,
            })
        }
    }
}

impl Drop for Slab {
    fn drop(&mut self) {
        unsafe { Metadata::decrement_and_drop(self.metadata) };
    }
}

impl<T> Drop for SlabMember<T> {
    fn drop(&mut self) {
        unsafe {
            drop_in_place(self.data.as_mut());
            Metadata::decrement_and_drop(self.metadata);
        }
    }
}


#[allow(unused)]
struct RcMdSlabMember<T> {
    metadata: NonNull<Metadata>,
    data: NonNull<T>,
}

impl<T> Deref for RcMdSlabMember<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        unsafe { self.data.as_ref() }
    }
}

impl<T> Drop for RcMdSlabMember<T> {
    fn drop(&mut self) {
        unsafe {
            Metadata::decrement_and_drop(self.metadata);
        }
    }
}

impl<T> SlabMember<T> {
    #[allow(unused)]
    fn into_rcmd(self) -> RcMdSlabMember<T> {
        RcMdSlabMember {
            metadata: self.metadata,
            data: self.data,
        }
    }
}

impl<T> Deref for RcSlabMember<T> {
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

impl<T> Drop for RcSlabMember<T> {
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

impl<T> Clone for RcSlabMember<T> {
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

/// A structure generating slabs as appropriated
pub struct Paving {
    capacity: usize,
    align: usize,
    current_slab: UnsafeCell<Slab>,
}

impl Paving {
    /// Creates a new paving, which will be backed by slabs
    /// created with correponding capacity and align.
    /// 
    /// See [`Slab::new`]
    pub fn new(capacity: usize, align: usize) -> Self {
        let first_slab = Slab::new(capacity, align);
        Self {
            capacity,
            align,
            current_slab: first_slab.into(),
        }
    }

    /// Try to allocate an object in the paving
    /// 
    /// Fails if no slab big enough can be created to accomodate
    /// the object
    pub fn try_alloc<T>(&self, value: T) -> Result<SlabMember<T>, T> {
        if size_of::<T>() * 2 > self.capacity {
            return Err(value);
        }
        unsafe {
            match (*self.current_slab.get()).try_alloc(value) {
                Ok(sm) => Ok(sm),
                Err(value) => {
                    *self.current_slab.get() = Slab::new(self.capacity, self.align);
                    let res = (*self.current_slab.get()).try_alloc(value);
                    debug_assert!(res.is_ok());
                    res
                }
            }
        }
    }

    /// Try to allocate a object with shared ownership in the slab.
    /// 
    /// Fails if no slab big enough can be created to accomodate
    /// the object
    pub fn try_alloc_rc<T>(&self, value: T) -> Result<RcSlabMember<T>, T> {
        if size_of::<T>() * 2 > self.capacity {
            return Err(value);
        }
        unsafe {
            match (*self.current_slab.get()).try_alloc_rc(value) {
                Ok(sm) => Ok(sm),
                Err(value) => {
                    *self.current_slab.get() = Slab::new(self.capacity, self.align);
                    let res = (*self.current_slab.get()).try_alloc_rc(value);
                    debug_assert!(res.is_ok());
                    res
                }
            }
        }
    }
}

/// A pointer to a mixed paving owning its pointee
pub enum OwnedMixedPavingMember<T> {
    /// The object was allocated in a slab
    SlabMember(SlabMember<T>),
    /// The object is allocated on its own
    Box(Box<T>),
}

impl<T> DerefMut for OwnedMixedPavingMember<T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        match self {
            OwnedMixedPavingMember::SlabMember(sm) => sm,
            OwnedMixedPavingMember::Box(b) => b,
        }
    }
}

impl<T> Deref for OwnedMixedPavingMember<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        match self {
            OwnedMixedPavingMember::SlabMember(sm) => sm,
            OwnedMixedPavingMember::Box(b) => b,
        }
    }
}

/// A pointer to a mixed paving sharing ownership of its pointee
pub enum SharedMixedPavingMember<T> {
    /// The object was allocated in a slab
    RcSlabMember(RcSlabMember<T>),
    /// The object is allocated on its own
    Rc(Rc<T>),
}

impl<T> Deref for SharedMixedPavingMember<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        match self {
            SharedMixedPavingMember::RcSlabMember(sm) => sm,
            SharedMixedPavingMember::Rc(rc) => rc,
        }
    }
}

/// A paving which will allocate objects too large out of any slab
pub struct MixedPaving(Paving);

impl MixedPaving {
    /// Creates a new mixed paving whose backing slabs will have the corresponding
    /// capacity and align.
    /// 
    /// See [`Slab::new`]
    pub fn new(capacity: usize, align: usize) -> Self {
        Self(Paving::new(capacity, align))
    }

    /// Alloc an object returning an owning pointer
    pub fn alloc<T>(&self, value: T) -> OwnedMixedPavingMember<T> {
        match self.0.try_alloc(value) {
            Ok(sm) => OwnedMixedPavingMember::SlabMember(sm),
            Err(val) => OwnedMixedPavingMember::Box(Box::new(val)),
        }
    }

    /// Alloc an object return an shareable pointer
    pub fn alloc_rc<T>(&self, value: T) -> SharedMixedPavingMember<T> {
        match self.0.try_alloc_rc(value) {
            Ok(sm) => SharedMixedPavingMember::RcSlabMember(sm),
            Err(val) => SharedMixedPavingMember::Rc(Rc::new(val)),
        }
    }
}

#[cfg(test)]
mod test {
    use std::mem::{align_of, size_of};

    use crate::{Paving, Slab};

    #[test]
    fn test_creation_slab() {
        {
            let mut slab_member1;
            let slab_member2;
            {
                let slab = Slab::new(2 * size_of::<u64>(), align_of::<u64>());
                slab_member1 = slab.try_alloc(123_u64).unwrap();
                slab_member2 = slab.try_alloc(456_u64).unwrap();
            }
            assert_eq!(*slab_member2, 456);
            assert_eq!(*slab_member1, 123);
            *slab_member1 += 1;
            assert_eq!(*slab_member1, 124);
        }
    }

    #[test]
    fn test_creation_paving() {
        {
            let slab_member1;
            let slab_member2;
            {
                let slab = Paving::new(2 * size_of::<u64>(), align_of::<u64>());
                slab_member1 = slab.try_alloc(123_u64).unwrap();
                slab.try_alloc(0_u64).unwrap();
                slab.try_alloc(0_u64).unwrap();
                slab_member2 = slab.try_alloc(456_u64).unwrap();
            }
            assert_eq!(*slab_member1, 123);
            assert_eq!(*slab_member2, 456);
        }
    }
}

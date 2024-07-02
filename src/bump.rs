use std::{
    alloc::{alloc, dealloc, Layout, LayoutError},
    cell::Cell,
    marker::PhantomData,
    mem::{align_of, needs_drop, size_of},
    ops::{Deref, DerefMut},
    ptr::{addr_of_mut, drop_in_place, NonNull},
};

/// The metadata of a Bump
struct Metadata {
    /// The number of pointer keeping this bump alive
    count: u64,
    /// The beginning of the Bump containing this Metadata
    beg: NonNull<u8>,
    /// The Layout that was obtained from [`Bump::inner_layout`]
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

// A Bump is a single object in memory containing first the data, then the metadata.
// Two pointers are kept: one constant to the Metadata (and hence right limit of
// the data), and the other to the first byte of the right, non allocated part
// of the data.
//
//

/// A zone of memory to allocate into.
pub struct Bump {
    metadata: NonNull<Metadata>,
    first_free: Cell<NonNull<u8>>,
}

impl Drop for Bump {
    fn drop(&mut self) {
        // Safety:
        // No other reference to metadata currently exists
        // (only pointers)
        unsafe { Metadata::decrement_and_drop(self.metadata) };
    }
}

impl Bump {
    fn inner_layout(capacity: usize, align: usize) -> Result<(Layout, usize), LayoutError> {
        Layout::from_size_align(capacity, align)?.extend(Layout::new::<Metadata>())
    }

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

        let (layout, metadata_offset) = Self::inner_layout(capacity, align).unwrap();
        // # Safety:
        // layout has a non zero size
        let inner_ptr = unsafe { alloc(layout) };
        if inner_ptr.is_null() {
            panic!("Memory allocation failed")
        }
        let metadata_ptr = {
            // # Safety:
            // metadat_offset and inner_ptr result from the same Layout::extend call
            let metadata_ptr = unsafe { inner_ptr.add(metadata_offset).cast::<Metadata>() };
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
            first_free: Cell::new(first_free),
        }
    }

    // Returns two pointers:
    // - first one is valid to write T
    // - second one will be the new first free
    // Both are in the same allocated object
    fn can_fit<T>(&self) -> Option<(*mut T, NonNull<u8>)> {
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
            #[allow(clippy::multiple_unsafe_ops_per_block)]
            let end = unsafe { NonNull::new_unchecked(beg.add(size_of::<T>())) };
            Some((beg.cast(), end))
        } else {
            None
        }
    }
}

struct RawBumpMember<T> {
    metadata: NonNull<Metadata>,
    data: NonNull<T>,
}

impl Bump {
    fn try_alloc_inner<T>(&self, value: T) -> Result<RawBumpMember<T>, T> {
        let (start, end): (*mut T, NonNull<u8>) = match self.can_fit::<T>() {
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
        self.first_free.set(end);
        let res = RawBumpMember {
            metadata: self.metadata,
            data: start,
        };
        Ok(res)
    }
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

impl Bump {
    /// Try to allocate an object in the bump
    ///
    /// Fails if there is not enough memory left
    pub fn try_alloc<T>(&self, value: T) -> Result<BumpMember<T>, T> {
        let RawBumpMember { metadata, data } = self.try_alloc_inner(value)?;
        Ok(BumpMember { metadata, data })
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

impl Bump {
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
                        drop_in_place(addr_of_mut!((*rc_entry.as_ptr()).value))
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

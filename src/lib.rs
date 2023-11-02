use std::{
    alloc::{alloc, dealloc, Layout, LayoutError},
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

pub struct Slab {
    metadata: NonNull<Metadata>,
    first_free: NonNull<u8>,
}

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
            first_free,
        }
    }

    pub fn can_fit<T>(&self) -> Option<(*mut T, *mut u8)> {
        let first_free_offset = self.first_free.as_ptr();
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

    fn try_alloc_inner<T>(&mut self, value: T) -> Result<RawSlabMember<T>, T> {
        let (start, end) = match self.can_fit::<T>() {
            Some(res) => res,
            None => return Err(value),
        };
        unsafe { start.write(value) };
        unsafe {
            self.metadata.as_mut().count += 1;
            self.first_free = NonNull::new_unchecked(end);
        };
        let res = RawSlabMember {
            metadata: self.metadata,
            data: unsafe { NonNull::new_unchecked(start) },
        };
        Ok(res)
    }

    pub fn try_alloc<T>(&mut self, value: T) -> Result<SlabMember<T>, T> {
        let RawSlabMember { metadata, data } = self.try_alloc_inner(value)?;
        Ok(SlabMember { metadata, data })
    }

    pub fn try_alloc_rc<T>(&mut self, value: T) -> Result<RcSlabMember<T>, T> {
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

pub struct RcMdSlabMember<T> {
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
    pub fn into_rcmd(self) -> RcMdSlabMember<T> {
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

pub struct Paving {
    capacity: usize,
    align: usize,
    current_slab: Slab,
}

impl Paving {
    pub fn new(capacity: usize, align: usize) -> Self {
        let first_slab = Slab::new(capacity, align);
        Self {
            capacity,
            align,
            current_slab: first_slab,
        }
    }

    pub fn try_alloc<T>(&mut self, value: T) -> Result<SlabMember<T>, T> {
        if size_of::<T>() * 2 > self.capacity {
            return Err(value);
        }
        match self.current_slab.try_alloc(value) {
            Ok(sm) => Ok(sm),
            Err(value) => {
                self.current_slab = Slab::new(self.capacity, self.align);
                let res = self.current_slab.try_alloc(value);
                debug_assert!(res.is_ok());
                res
            }
        }
    }

    pub fn try_alloc_rc<T>(&mut self, value: T) -> Result<RcSlabMember<T>, T> {
        if size_of::<T>() * 2 > self.capacity {
            return Err(value);
        }
        match self.current_slab.try_alloc_rc(value) {
            Ok(sm) => Ok(sm),
            Err(value) => {
                self.current_slab = Slab::new(self.capacity, self.align);
                let res = self.current_slab.try_alloc_rc(value);
                debug_assert!(res.is_ok());
                res
            }
        }
    }
}

pub enum BoxingPavingMember<T> {
    SlabMember(SlabMember<T>),
    Box(Box<T>),
}

pub enum RcBoxingPavingMember<T> {
    RcSlabMember(RcSlabMember<T>),
    Rc(Rc<T>),
}

pub struct BoxingPaving(Paving);

impl BoxingPaving {
    pub fn new(capacity: usize, align: usize) -> Self {
        Self(Paving::new(capacity, align))
    }

    pub fn alloc<T>(&mut self, value: T) -> BoxingPavingMember<T> {
        match self.0.try_alloc(value) {
            Ok(sm) => BoxingPavingMember::SlabMember(sm),
            Err(val) => BoxingPavingMember::Box(Box::new(val)),
        }
    }

    pub fn alloc_rc<T>(&mut self, value: T) -> RcBoxingPavingMember<T> {
        match self.0.try_alloc_rc(value) {
            Ok(sm) => RcBoxingPavingMember::RcSlabMember(sm),
            Err(val) => RcBoxingPavingMember::Rc(Rc::new(val)),
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
                let mut slab = Slab::new(2 * size_of::<u64>(), align_of::<u64>());
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
                let mut slab = Paving::new(2 * size_of::<u64>(), align_of::<u64>());
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

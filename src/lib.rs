use std::{
    alloc::{alloc, dealloc, Layout, LayoutError},
    mem::{align_of, needs_drop, size_of},
    ops::{Deref, DerefMut},
    ptr::{drop_in_place, NonNull}, rc::Rc,
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

pub struct RcSlabMember<T> {
    metadata: NonNull<Metadata>,
    rc_data: NonNull<SlabRcEntry<T>>,
}

fn inner_slab_layout(capacity: usize, align: usize) -> Result<(Layout, usize), LayoutError> {
    Layout::from_size_align(capacity, align)?.extend(Layout::new::<Metadata>())
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

    pub fn try_alloc<T>(&mut self, value: T) -> Result<SlabMember<T>, T> {
        let first_free_offset = self.first_free.as_ptr().cast::<u8>();
        let align_offset = first_free_offset.align_offset(align_of::<T>());
        let new_first_free = first_free_offset
            .wrapping_add(align_offset)
            .wrapping_add(size_of::<T>());
        if new_first_free as usize > self.metadata.as_ptr() as usize {
            return Err(value);
        }
        let object_beg = first_free_offset.wrapping_add(align_offset).cast::<T>();
        unsafe { object_beg.write(value) };
        unsafe {
            self.metadata.as_mut().count += 1;
            self.first_free = NonNull::new_unchecked(new_first_free.cast());
        };
        let res = SlabMember {
            metadata: self.metadata,
            data: unsafe { NonNull::new_unchecked(object_beg) },
        };
        Ok(res)
    }

    pub fn try_alloc_rc<T>(&mut self, value: T) -> Result<RcSlabMember<T>, T> {
        let SlabMember { metadata, data } = self
            .try_alloc(SlabRcEntry { count: 1, value })
            .map_err(|srce| srce.value)?;
        Ok(RcSlabMember {
            metadata,
            rc_data: data,
        })
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
        unsafe { &self.rc_data.as_ref().value }
    }
}

impl<T> Drop for RcSlabMember<T> {
    fn drop(&mut self) {
        unsafe {
            if needs_drop::<T>() {
                self.rc_data.as_mut().count -= 1;
                if self.rc_data.as_ref().count == 0 {
                    Metadata::decrement_and_drop(self.metadata);
                }
            } else {
                    Metadata::decrement_and_drop(self.metadata);
            }
        }
    }
}

impl<T> Clone for RcSlabMember<T> {
    fn clone(&self) -> Self {
        unsafe {
            if needs_drop::<T>() {
                (*self.rc_data.as_ptr()).count += 1
            } else {
                (*self.metadata.as_ptr()).count += 1
            }
        }
        Self { metadata: self.metadata, rc_data: self.rc_data }
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
        match self.current_slab.try_alloc(value) {
            Ok(sm) => Ok(sm),
            Err(value) => {
                self.current_slab = Slab::new(self.capacity, self.align);
                self.current_slab.try_alloc(value)
            }
        }
    }

    pub fn try_alloc_rc<T>(&mut self, value: T) -> Result<RcSlabMember<T>, T> {
        match self.current_slab.try_alloc_rc(value) {
            Ok(sm) => Ok(sm),
            Err(value) => {
                self.current_slab = Slab::new(self.capacity, self.align);
                self.current_slab.try_alloc_rc(value)
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

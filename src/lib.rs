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

mod bump;
pub use bump::*;

mod paving;
pub use paving::*;

mod mixed_paving;
pub use mixed_paving::*;

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

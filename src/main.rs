use std::mem::align_of;

use rc_slab::Slab;

fn main() {
    let mut slab_member1;
    let mut slab_member2;
    {
        let mut slab = Slab::new(16,align_of::<u64>());
        slab_member1 = slab.try_alloc(123_u64).unwrap();
        slab_member2 = slab.try_alloc(456_u64).unwrap();
    }
    *slab_member2 += 44;
    dbg!(*slab_member1);
    *slab_member1 += 1;
    dbg!(*slab_member1);
    dbg!(*slab_member2);
}
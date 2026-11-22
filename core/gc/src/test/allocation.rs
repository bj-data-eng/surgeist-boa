mod miri {
    use std::cell::Cell;

    use boa_macros::{Finalize, Trace};

    use super::super::{Harness, run_test};
    use crate::{
        Gc, GcBox, GcRefCell, HeapStatistics, WeakGc, force_collect, heap_statistics,
        internals::EphemeronBox,
    };

    #[test]
    fn heap_statistics_observe_shallow_boxes_without_collecting() {
        run_test(|| {
            assert_eq!(
                heap_statistics(),
                Some(HeapStatistics {
                    bytes_allocated: 0,
                    collections: 0,
                })
            );

            let gc = Gc::new(vec![0_u8; 4096]);
            let weak = WeakGc::new(&gc);
            let bytes = size_of::<GcBox<Vec<u8>>>() + size_of::<EphemeronBox<Vec<u8>, ()>>();
            let allocated = HeapStatistics {
                bytes_allocated: bytes,
                collections: 0,
            };
            assert_eq!(heap_statistics(), Some(allocated));
            drop(weak);
            drop(gc);
            assert_eq!(heap_statistics(), Some(allocated));

            force_collect();
            assert_eq!(
                heap_statistics(),
                Some(HeapStatistics {
                    bytes_allocated: 0,
                    collections: 1,
                })
            );
        });
    }

    #[test]
    fn heap_statistics_refuse_reentrant_collection_observation() {
        thread_local!(static FINALIZED: Cell<bool> = const { Cell::new(false) });

        #[derive(Trace)]
        struct Observer;

        impl crate::Finalize for Observer {
            fn finalize(&self) {
                assert_eq!(heap_statistics(), None);
                FINALIZED.set(true);
            }
        }

        run_test(|| {
            drop(Gc::new(Observer));
            force_collect();
            assert!(FINALIZED.get());
            assert_eq!(heap_statistics().map(|stats| stats.collections), Some(1));
        });
    }

    #[test]
    fn gc_basic_cell_allocation() {
        run_test(|| {
            let gc_cell = Gc::new(GcRefCell::new(16_u16));

            force_collect();
            Harness::assert_collections(1);
            Harness::assert_bytes_allocated();
            assert_eq!(*gc_cell.borrow_mut(), 16);
        });
    }

    #[test]
    fn gc_basic_pointer_alloc() {
        run_test(|| {
            let gc = Gc::new(16_u8);

            force_collect();
            Harness::assert_collections(1);
            Harness::assert_bytes_allocated();
            assert_eq!(*gc, 16);

            drop(gc);
            force_collect();
            Harness::assert_collections(2);
            Harness::assert_empty_gc();
        });
    }

    #[test]
    // Takes too long to finish in miri
    #[cfg_attr(miri, ignore)]
    fn gc_recursion() {
        run_test(|| {
            #[derive(Debug, Finalize, Trace)]
            struct S {
                i: usize,
                next: Option<Gc<S>>,
            }

            const SIZE: usize = size_of::<GcBox<S>>();
            const COUNT: usize = 1_000_000;

            let mut root = Gc::new(S { i: 0, next: None });
            for i in 1..COUNT {
                root = Gc::new(S {
                    i,
                    next: Some(root),
                });
            }

            Harness::assert_bytes_allocated();
            Harness::assert_exact_bytes_allocated(SIZE * COUNT);

            drop(root);
            force_collect();
            Harness::assert_empty_gc();
        });
    }
}

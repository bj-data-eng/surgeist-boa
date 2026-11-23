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
                    collections: Some(0),
                    cumulative_bytes_allocated: Some(0),
                })
            );

            let gc = Gc::new(vec![0_u8; 4096]);
            let weak = WeakGc::new(&gc);
            let bytes = size_of::<GcBox<Vec<u8>>>() + size_of::<EphemeronBox<Vec<u8>, ()>>();
            let allocated = HeapStatistics {
                bytes_allocated: bytes,
                collections: Some(0),
                cumulative_bytes_allocated: u64::try_from(bytes).ok(),
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
                    collections: Some(1),
                    cumulative_bytes_allocated: u64::try_from(bytes).ok(),
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
            assert_eq!(
                heap_statistics().and_then(|stats| stats.collections),
                Some(1)
            );
        });
    }

    #[test]
    fn heap_statistics_separate_high_churn_from_retained_bytes() {
        run_test(|| {
            let fixed = Gc::new(17_u8);
            let fixed_bytes = size_of::<GcBox<u8>>();
            let temporary_bytes = size_of::<GcBox<[u8; 64]>>();
            let mut total = fixed_bytes;
            for collections in 1_u64..=3 {
                for _ in 0..32 {
                    drop(Gc::new([0_u8; 64]));
                }
                total += 32 * temporary_bytes;
                force_collect();
                assert_eq!(
                    heap_statistics(),
                    Some(HeapStatistics {
                        bytes_allocated: fixed_bytes,
                        collections: Some(collections),
                        cumulative_bytes_allocated: Some(u64::try_from(total).unwrap()),
                    })
                );
            }
            assert_eq!(*fixed, 17);
        });
    }

    #[test]
    fn heap_statistics_report_overflow_without_losing_the_live_gauge() {
        run_test(|| {
            let bytes = size_of::<GcBox<u8>>();
            let total_before = u64::MAX - u64::try_from(bytes).unwrap();
            crate::BOA_GC.with_borrow_mut(|gc| {
                gc.runtime.cumulative_bytes_allocated = Some(total_before);
                gc.runtime.collections = Some(u64::MAX - 1);
            });
            let retained = Gc::new(7_u8);
            force_collect();
            assert_eq!(
                heap_statistics(),
                Some(HeapStatistics {
                    bytes_allocated: bytes,
                    collections: Some(u64::MAX),
                    cumulative_bytes_allocated: Some(u64::MAX),
                })
            );
            drop(Gc::new(8_u8));
            assert_eq!(
                heap_statistics(),
                Some(HeapStatistics {
                    bytes_allocated: 2 * bytes,
                    collections: Some(u64::MAX),
                    cumulative_bytes_allocated: None,
                })
            );
            force_collect();
            drop(Gc::new(9_u8));
            force_collect();
            assert_eq!(
                heap_statistics(),
                Some(HeapStatistics {
                    bytes_allocated: bytes,
                    collections: None,
                    cumulative_bytes_allocated: None,
                })
            );
            assert_eq!(*retained, 7);
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

use std::{
    cell::{Cell, RefCell},
    panic::{AssertUnwindSafe, catch_unwind},
    pin::pin,
    rc::Rc,
};

use futures_lite::future;

use crate::{
    Context, JsValue, Source, TestAction,
    context::{ContextBuilder, time::FixedClock},
    job::{
        GenericJob, JobExecutor, MicrotaskProgress, NativeAsyncJob, PromiseJob, SimpleJobExecutor,
    },
    run_test_actions_with,
};

#[test]
fn suspended_host_microtask_preserves_promise_suffix_and_async_jobs() {
    let mut context = Context::default();
    let realm = context.realm().clone();
    let async_runs = Rc::new(Cell::new(0));
    let observed = async_runs.clone();
    context.enqueue_job(
        NativeAsyncJob::new(async move |_| {
            observed.set(observed.get() + 1);
            Ok(JsValue::undefined())
        })
        .into(),
    );
    context
        .eval(Source::from_bytes(concat!(
            "globalThis.order = []; globalThis.anchor = new WeakRef({}); ",
            "Promise.resolve().then(() => order.push('before'));",
        )))
        .unwrap();
    let original_realm = realm.clone();
    let mut resumed = false;
    context
        .enqueue_host_microtask(move |context| {
            assert_eq!(context.realm(), &original_realm);
            if resumed {
                assert!(
                    context
                        .eval(Source::from_bytes("anchor.deref()"))?
                        .is_object()
                );
                context.eval(Source::from_bytes("order.push('host-resume')"))?;
                return Ok(MicrotaskProgress::Complete);
            }
            resumed = true;
            context.eval(Source::from_bytes(
                "order.push('host-prefix'); Promise.resolve().then(() => order.push('nested'));",
            ))?;
            assert!(context.run_jobs().is_err());
            Ok(MicrotaskProgress::Suspended)
        })
        .unwrap();
    context
        .eval(Source::from_bytes(
            "Promise.resolve().then(() => order.push('after'));",
        ))
        .unwrap();
    let outer = context.create_realm().unwrap();
    context.enter_realm(outer.clone());
    let read_order = |context: &mut Context| {
        assert_eq!(context.realm(), &outer);
        let previous = context.enter_realm(realm.clone());
        let order = context
            .eval(Source::from_bytes("order.join(',')"))
            .unwrap()
            .as_string()
            .unwrap()
            .to_std_string_escaped();
        context.enter_realm(previous);
        order
    };
    assert!(context.run_jobs().is_err());
    assert_eq!(read_order(&mut context), "");
    assert_eq!(
        context.run_microtask_checkpoint().unwrap(),
        MicrotaskProgress::Suspended
    );
    assert_eq!(read_order(&mut context), "before,host-prefix");
    assert_eq!(async_runs.get(), 0);
    assert!(context.run_jobs().is_err());
    assert_eq!(read_order(&mut context), "before,host-prefix");
    boa_gc::force_collect();
    assert_eq!(
        context.run_microtask_checkpoint().unwrap(),
        MicrotaskProgress::Complete
    );
    assert_eq!(
        read_order(&mut context),
        "before,host-prefix,host-resume,after,nested"
    );
    assert_eq!(async_runs.get(), 0);
    boa_gc::force_collect();
    let previous = context.enter_realm(realm.clone());
    assert!(
        context
            .eval(Source::from_bytes("anchor.deref()"))
            .unwrap()
            .is_undefined()
    );
    context.enter_realm(previous);
    context.run_jobs().unwrap();
    assert_eq!(async_runs.get(), 1);
    assert_eq!(
        read_order(&mut context),
        "before,host-prefix,host-resume,after,nested"
    );
}

#[test]
fn retiring_a_suspended_microtask_releases_its_captures_without_entering_the_suffix() {
    let mut context = Context::default();
    let capture = Rc::new(Cell::new(0));
    let occurrence = capture.clone();
    context
        .enqueue_host_microtask(move |_| {
            occurrence.set(occurrence.get() + 1);
            Ok(MicrotaskProgress::Suspended)
        })
        .unwrap();
    let tail_runs = Rc::new(Cell::new(0));
    let observed = tail_runs.clone();
    context.enqueue_job(
        PromiseJob::new(move |_| {
            observed.set(observed.get() + 1);
            Ok(JsValue::undefined())
        })
        .into(),
    );
    assert_eq!(
        context.run_microtask_checkpoint().unwrap(),
        MicrotaskProgress::Suspended
    );
    assert_eq!(Rc::strong_count(&capture), 2);
    let executor = context
        .downcast_job_executor::<SimpleJobExecutor>()
        .unwrap();
    let mut foreign = ContextBuilder::default()
        .job_executor(executor)
        .build()
        .unwrap();
    assert!(foreign.run_microtask_checkpoint().is_err());
    assert!(
        foreign
            .enqueue_host_microtask(|_| Ok(MicrotaskProgress::Complete))
            .is_err()
    );
    assert!(foreign.run_jobs().is_err());
    drop(context);
    assert_eq!(Rc::strong_count(&capture), 1);
    assert_eq!(Rc::strong_count(&tail_runs), 1);
    assert_eq!(capture.get(), 1);
    foreign.run_jobs().unwrap();
    assert_eq!(tail_runs.get(), 0);
}

#[test]
fn unwind_abandons_the_host_occurrence_without_entering_its_promise_suffix() {
    let mut context = Context::default();
    let host_runs = Rc::new(Cell::new(0));
    let counter = host_runs.clone();
    context
        .enqueue_host_microtask(move |_| {
            counter.set(counter.get() + 1);
            assert_ne!(counter.get(), 1, "first host entry unwinds");
            Ok(MicrotaskProgress::Complete)
        })
        .unwrap();
    context
        .eval(Source::from_bytes(
            "globalThis.tail = 0; Promise.resolve().then(() => ++tail);",
        ))
        .unwrap();
    let outer = context.create_realm().unwrap();
    let source = context.enter_realm(outer.clone());
    let result = catch_unwind(AssertUnwindSafe(|| context.run_microtask_checkpoint()));
    assert!(result.is_err());
    assert_eq!(context.realm(), &outer);
    context.enter_realm(source);
    let read_tail = |context: &mut Context| {
        context
            .eval(Source::from_bytes("tail"))
            .unwrap()
            .as_number()
            .unwrap()
    };
    assert_eq!(read_tail(&mut context), 0.0);
    assert_eq!(
        context.run_microtask_checkpoint().unwrap(),
        MicrotaskProgress::Complete
    );
    assert_eq!(host_runs.get(), 1);
    assert_eq!(read_tail(&mut context), 1.0);
}

#[test]
fn stopping_a_microtask_turn_preserves_unselected_engine_tasks() {
    let mut context = Context::default();
    let executor = context
        .downcast_job_executor::<SimpleJobExecutor>()
        .unwrap();
    let async_runs = Rc::new(Cell::new(0));
    let observed = async_runs.clone();
    context.enqueue_job(
        NativeAsyncJob::new(async move |_| {
            observed.set(observed.get() + 1);
            Ok(JsValue::undefined())
        })
        .into(),
    );
    context
        .enqueue_host_microtask(|_| Ok(MicrotaskProgress::Suspended))
        .unwrap();
    assert_eq!(
        context.run_microtask_checkpoint().unwrap(),
        MicrotaskProgress::Suspended
    );
    executor
        .get_cancellation_token()
        .store(true, std::sync::atomic::Ordering::Relaxed);
    assert!(context.run_microtask_checkpoint().is_err());
    assert!(!context.has_pending_host_microtasks());
    assert_eq!(async_runs.get(), 0);
    context.run_jobs().unwrap();
    assert_eq!(async_runs.get(), 1);
}

#[test]
fn full_drivers_dropped_in_start_order_keep_the_live_turn_exclusive() {
    let mut context = Context::default();
    let executor = context
        .downcast_job_executor::<SimpleJobExecutor>()
        .unwrap();
    let context = RefCell::new(&mut context);
    let enqueue_pending = |context: &mut Context| {
        context.enqueue_job(
            NativeAsyncJob::new(async |_| {
                loop {
                    future::yield_now().await;
                }
            })
            .into(),
        );
    };
    enqueue_pending(&mut context.borrow_mut());
    let mut first = Box::pin(executor.clone().run_jobs_async(&context));
    assert!(future::block_on(future::poll_once(first.as_mut())).is_none());
    enqueue_pending(&mut context.borrow_mut());
    let mut second = Box::pin(executor.run_jobs_async(&context));
    assert!(future::block_on(future::poll_once(second.as_mut())).is_none());

    drop(first);
    assert!(context.borrow_mut().run_microtask_checkpoint().is_err());
    assert!(
        context
            .borrow_mut()
            .enqueue_host_microtask(|_| Ok(MicrotaskProgress::Complete))
            .is_err()
    );
    drop(second);
    context
        .borrow_mut()
        .enqueue_host_microtask(|_| Ok(MicrotaskProgress::Complete))
        .unwrap();
    assert_eq!(
        context.borrow_mut().run_microtask_checkpoint().unwrap(),
        MicrotaskProgress::Complete
    );
}

#[test]
fn test_async_job_not_blocking_event_loop() {
    let clock = Rc::new(FixedClock::default());
    let context = &mut ContextBuilder::default()
        .clock(clock.clone())
        .build()
        .unwrap();

    run_test_actions_with(
        [TestAction::inspect_context_async(async move |ctx| {
            let executor = ctx.downcast_job_executor::<SimpleJobExecutor>().unwrap();
            let ctx = &RefCell::new(ctx);

            let mut event_loop = pin!(future::poll_once(executor.run_jobs_async(ctx)));

            // There are no jobs in our queue. Push
            // an async job that will consistently yield to the executor.
            ctx.borrow_mut().enqueue_job(
                NativeAsyncJob::new(async |_| {
                    loop {
                        future::yield_now().await;
                    }
                })
                .into(),
            );

            // Then, start the event loop
            assert!(event_loop.as_mut().await.is_none());

            let checker = Rc::new(Cell::new(false));
            {
                let checker = checker.clone();
                // At this point, the event loop should have yielded again to the async executor.
                // Thus, enqueue a generic job that should resolve in the next loop.
                let realm = ctx.borrow().realm().clone();
                ctx.borrow_mut().enqueue_job(
                    GenericJob::new(
                        move |_| {
                            checker.set(true);
                            Ok(JsValue::undefined())
                        },
                        realm,
                    )
                    .into(),
                );
            }

            // Next iteration of the event loop
            assert!(event_loop.as_mut().await.is_none());

            // At this point, our generic job should have been executed.
            assert!(checker.get());
        })],
        context,
    );
}

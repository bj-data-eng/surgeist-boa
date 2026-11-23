use boa_gc::Gc;
use boa_parser::Source;
use futures_lite::future;

use super::{
    CallFrame, CallFrameFlags, CodeBlock,
    opcode::{Address, BytecodeEmitter, RegisterOperand},
};
use crate::{
    Context, JsResult, JsValue, NativeFunction, context::InstructionCount,
    environments::EnvironmentStack, js_string,
};

fn context() -> Context {
    let builder = Context::builder();
    // The provider's fuzz-only budget defaults to zero. Except in the refusal
    // test, leave enough room for every deliberately bounded fixture to finish.
    #[cfg(feature = "fuzz")]
    let builder = builder.instructions_remaining(usize::MAX);
    builder.build().unwrap()
}

fn push_fixture(context: &mut Context, emitter: BytecodeEmitter) {
    let mut block = CodeBlock::new(js_string!("instruction-count fixture"), 0, true);
    block.register_count = 1;
    block.bytecode = emitter.into_bytecode();
    let frame = CallFrame::new(
        Gc::new(block),
        None,
        EnvironmentStack::new(),
        context.realm().clone(),
    )
    .with_flags(CallFrameFlags::EXIT_EARLY);
    context
        .vm
        .push_frame_with_stack(frame, JsValue::undefined(), JsValue::null());
}

fn pop_fixture(context: &mut Context) {
    let frame = context.vm.pop_frame().unwrap();
    // Return and Throw already restore the stack. A refusal before dispatch
    // can exit without executing either handler.
    context.vm.stack.truncate_to_frame(&frame);
}

const FIXTURE_RESULT: i32 = 65_537;
// Reached path: StoreZero, JumpTable, StoreInt32, SetAccumulator, Return.
const FIXTURE_DISPATCHES: u64 = 5;

fn push_jump_fixture(context: &mut Context) {
    let mut emitter = BytecodeEmitter::new();
    let register = RegisterOperand::new(0);
    emitter.emit_store_zero(register);
    // patch_jump_table takes the opcode offset plus its index operand width.
    let table = emitter.next_opcode_location() + 4;
    emitter.emit_jump_table(0, [Address::new(0); 4].into_iter().collect());

    // If dispatch falls through, this branch returns a different value. Its
    // instructions are present in the code block and must remain uncounted.
    emitter.emit_store_int32(register, -1);
    emitter.emit_set_accumulator(register);
    emitter.emit_return();

    let target = emitter.next_opcode_location();
    emitter.patch_jump_table(table, &[target; 4]);
    emitter.emit_store_int32(register, FIXTURE_RESULT);
    emitter.emit_set_accumulator(register);
    emitter.emit_return();
    push_fixture(context, emitter);
}

fn run_jump_fixture(context: &mut Context) -> JsResult<JsValue> {
    push_jump_fixture(context);
    let result = context.run().consume();
    pop_fixture(context);
    result
}

#[test]
fn instruction_count_is_opt_in_and_context_local() {
    let mut first = context();
    let mut second = context();
    assert_eq!(first.instruction_count(), InstructionCount::Disabled);
    assert_eq!(
        run_jump_fixture(&mut first).unwrap().as_i32(),
        Some(FIXTURE_RESULT)
    );
    assert_eq!(first.instruction_count(), InstructionCount::Disabled);

    first.enable_instruction_counting();
    assert_eq!(first.instruction_count(), InstructionCount::Exact(0));
    assert_eq!(
        run_jump_fixture(&mut first).unwrap().as_i32(),
        Some(FIXTURE_RESULT)
    );
    assert_eq!(
        first.instruction_count(),
        InstructionCount::Exact(FIXTURE_DISPATCHES)
    );
    assert_eq!(second.instruction_count(), InstructionCount::Disabled);

    second.enable_instruction_counting();
    assert_eq!(second.instruction_count(), InstructionCount::Exact(0));
    assert_eq!(
        run_jump_fixture(&mut second).unwrap().as_i32(),
        Some(FIXTURE_RESULT)
    );
    first.enable_instruction_counting();
    assert_eq!(
        run_jump_fixture(&mut first).unwrap().as_i32(),
        Some(FIXTURE_RESULT)
    );
    assert_eq!(
        first.instruction_count(),
        InstructionCount::Exact(2 * FIXTURE_DISPATCHES)
    );
    assert_eq!(
        second.instruction_count(),
        InstructionCount::Exact(FIXTURE_DISPATCHES)
    );
}

#[test]
fn instruction_count_survives_cooperative_yield_with_the_same_dispatch_total() {
    let mut sync = context();
    sync.enable_instruction_counting();
    let expected = run_jump_fixture(&mut sync).unwrap();
    assert_eq!(expected.as_i32(), Some(FIXTURE_RESULT));
    assert_eq!(
        sync.instruction_count(),
        InstructionCount::Exact(FIXTURE_DISPATCHES)
    );

    let mut asynchronous = context();
    asynchronous.enable_instruction_counting();
    push_jump_fixture(&mut asynchronous);
    let result = {
        let mut execution = std::pin::pin!(asynchronous.run_async_with_budget(1));
        assert!(future::block_on(future::poll_once(execution.as_mut())).is_none());
        future::block_on(execution).consume().unwrap()
    };
    pop_fixture(&mut asynchronous);
    assert_eq!(result.as_i32(), expected.as_i32());
    assert_eq!(asynchronous.instruction_count(), sync.instruction_count());
}

#[test]
fn instruction_count_includes_the_reached_throw_and_excludes_its_suffix() {
    let mut context = context();
    let register = RegisterOperand::new(0);
    let mut emitter = BytecodeEmitter::new();
    emitter.emit_store_int32(register, 73);
    emitter.emit_throw(register);
    emitter.emit_return();
    context.enable_instruction_counting();
    push_fixture(&mut context, emitter);
    let error = context.run().consume().unwrap_err();
    pop_fixture(&mut context);
    assert_eq!(error.as_opaque().and_then(JsValue::as_i32), Some(73));
    assert_eq!(context.instruction_count(), InstructionCount::Exact(2));
}

#[test]
fn instruction_count_includes_reentry_and_excludes_pure_native_work() {
    let mut context = context();
    context
        .register_global_builtin_callable(
            js_string!("nativeOnly"),
            0,
            NativeFunction::from_copy_closure(|_, _, _| Ok(JsValue::new(12))),
        )
        .unwrap();
    context
        .register_global_builtin_callable(
            js_string!("reenter"),
            0,
            NativeFunction::from_copy_closure(|_, _, context| {
                let InstructionCount::Exact(before) = context.instruction_count() else {
                    panic!("the outer evaluation has instruction counting enabled")
                };
                assert!(before > 0);
                let result = run_jump_fixture(context)?;
                assert_eq!(
                    context.instruction_count(),
                    InstructionCount::Exact(before + FIXTURE_DISPATCHES)
                );
                Ok(result)
            }),
        )
        .unwrap();

    context.enable_instruction_counting();
    let native = context
        .global_object()
        .get(js_string!("nativeOnly"), &mut context)
        .unwrap()
        .as_callable()
        .unwrap();
    assert_eq!(
        native
            .call(&JsValue::undefined(), &[], &mut context)
            .unwrap()
            .as_i32(),
        Some(12)
    );
    assert_eq!(context.instruction_count(), InstructionCount::Exact(0));
    assert_eq!(
        context
            .eval(Source::from_bytes("reenter()"))
            .unwrap()
            .as_i32(),
        Some(FIXTURE_RESULT)
    );
}

#[test]
fn instruction_count_overflow_is_sticky_at_dispatch() {
    let mut context = context();
    context.enable_instruction_counting();
    context.instruction_count = InstructionCount::Exact(u64::MAX - FIXTURE_DISPATCHES);
    assert_eq!(
        run_jump_fixture(&mut context).unwrap().as_i32(),
        Some(FIXTURE_RESULT)
    );
    assert_eq!(
        context.instruction_count(),
        InstructionCount::Exact(u64::MAX)
    );

    assert_eq!(
        run_jump_fixture(&mut context).unwrap().as_i32(),
        Some(FIXTURE_RESULT)
    );
    context.enable_instruction_counting();
    assert_eq!(context.instruction_count(), InstructionCount::Overflowed);
}

#[cfg(feature = "fuzz")]
#[test]
fn instruction_count_excludes_dispatch_refused_by_the_fuzz_budget() {
    for budget in [0, 1] {
        let mut context = Context::builder()
            .instructions_remaining(budget)
            .build()
            .unwrap();
        context.enable_instruction_counting();
        let error = run_jump_fixture(&mut context).unwrap_err();
        assert_eq!(
            error.as_engine(),
            Some(&crate::error::EngineError::NoInstructionsRemain)
        );
        assert_eq!(
            context.instruction_count(),
            InstructionCount::Exact(u64::try_from(budget).unwrap())
        );
    }
}

use boa_ast::scope::{BindingLocator, BindingLocatorScope};

use crate::{
    Context, JsError, JsNativeError, JsResult, JsString,
    environments::Environment,
    vm::{
        call_frame::BindingReference,
        opcode::{IndexOperand, Operation, RegisterOperand},
    },
};

/// `ThrowMutateImmutable` implements the Opcode Operation for `Opcode::ThrowMutateImmutable`
///
/// Operation:
///  - Throws an error because the binding access is illegal.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ThrowMutateImmutable;

impl ThrowMutateImmutable {
    #[inline(always)]
    pub(crate) fn operation(index: IndexOperand, context: &mut Context) -> JsError {
        let name = context
            .vm
            .frame()
            .code_block()
            .constant_string(index.into());

        JsNativeError::typ()
            .with_message(format!(
                "cannot mutate an immutable binding '{}'",
                name.to_std_string_escaped()
            ))
            .into()
    }
}

impl Operation for ThrowMutateImmutable {
    const NAME: &'static str = "ThrowMutateImmutable";
    const INSTRUCTION: &'static str = "INST - ThrowMutateImmutable";
    const COST: u8 = 2;
}

/// `SetName` implements the Opcode Operation for `Opcode::SetName`
///
/// Operation:
///  - Find a binding on the environment chain and assign its value.
#[derive(Debug, Clone, Copy)]
pub(crate) struct SetName;

impl SetName {
    #[inline(always)]
    pub(crate) fn operation(
        (value, index): (RegisterOperand, IndexOperand),
        context: &mut Context,
    ) -> JsResult<()> {
        let value = context.vm.get_register(value.into()).clone();
        let code_block = context.vm.frame().code_block();
        let mut binding_locator = code_block.bindings[usize::from(index)].clone();
        let strict = code_block.strict();

        context.find_runtime_binding(&mut binding_locator)?;

        verify_initialized(&binding_locator, context)?;

        context.set_binding(&binding_locator, value.clone(), strict)?;

        Ok(())
    }
}

impl Operation for SetName {
    const NAME: &'static str = "SetName";
    const INSTRUCTION: &'static str = "INST - SetName";
    const COST: u8 = 4;
}

/// `SetNameByLocator` implements the Opcode Operation for `Opcode::SetNameByLocator`
///
/// Operation:
///  - Assigns a value using the pending reference in the current frame.
#[derive(Debug, Clone, Copy)]
pub(crate) struct SetNameByLocator;

impl SetNameByLocator {
    #[inline(always)]
    pub(crate) fn operation(value: RegisterOperand, context: &mut Context) -> JsResult<()> {
        let frame = context.vm.frame_mut();
        let strict = frame.code_block.strict();
        let reference = frame.pop_binding()?;
        let value = context.vm.get_register(value.into()).clone();

        let mut binding_locator = match reference {
            BindingReference::Resolved(locator) => locator,
            BindingReference::UnresolvableGlobal(name) => {
                if strict {
                    return Err(uninitialized_global_property(&name));
                }

                context.global_object().set(name, value, false, context)?;
                return Ok(());
            }
        };

        if context.is_deleted_binding(&binding_locator) {
            context.find_runtime_binding(&mut binding_locator)?;
        }

        verify_initialized(&binding_locator, context)?;

        context.set_binding(&binding_locator, value.clone(), strict)?;

        Ok(())
    }
}

impl Operation for SetNameByLocator {
    const NAME: &'static str = "SetNameByLocator";
    const INSTRUCTION: &'static str = "INST - SetNameByLocator";
    const COST: u8 = 4;
}

/// Discards a saved reference when a logical assignment skips its write.
#[derive(Debug, Clone, Copy)]
pub(crate) struct PopBinding;

impl PopBinding {
    #[inline(always)]
    pub(crate) fn operation((): (), context: &mut Context) -> JsResult<()> {
        context.vm.frame_mut().pop_binding()?;
        Ok(())
    }
}

impl Operation for PopBinding {
    const NAME: &'static str = "PopBinding";
    const INSTRUCTION: &'static str = "INST - PopBinding";
    const COST: u8 = 1;
}

fn uninitialized_global_property(key: &JsString) -> JsError {
    JsNativeError::reference()
        .with_message(format!(
            "cannot assign to uninitialized global property `{}`",
            key.to_std_string_escaped()
        ))
        .into()
}

/// Checks that the binding pointed by `locator` exists and is initialized.
fn verify_initialized(locator: &BindingLocator, context: &mut Context) -> JsResult<()> {
    if !context.is_initialized_binding(locator)? {
        let key = locator.name();
        let strict = context.vm.frame().code_block.strict();

        let message = match locator.scope() {
            BindingLocatorScope::GlobalObject if strict => {
                return Err(uninitialized_global_property(key));
            }
            BindingLocatorScope::GlobalObject => None,
            BindingLocatorScope::GlobalDeclarative => Some(format!(
                "cannot assign to uninitialized binding `{}`",
                key.to_std_string_escaped()
            )),
            BindingLocatorScope::Stack(index) => match context.environment_expect(index) {
                Environment::Declarative(_) => Some(format!(
                    "cannot assign to uninitialized binding `{}`",
                    key.to_std_string_escaped()
                )),
                Environment::Object(_) if strict => Some(format!(
                    "cannot assign to uninitialized property `{}`",
                    key.to_std_string_escaped()
                )),
                Environment::Object(_) => None,
            },
        };

        if let Some(message) = message {
            return Err(JsNativeError::reference().with_message(message).into());
        }
    }

    Ok(())
}

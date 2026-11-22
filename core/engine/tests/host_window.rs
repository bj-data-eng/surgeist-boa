#![allow(unused_crate_dependencies)]
//! Global bindings and descriptors of a hosted Window.

use std::{cell::Cell, rc::Rc};

use boa_engine::{
    Context, JsObject, JsValue, NativeFunction, Source,
    context::{HostHooks, intrinsics::Intrinsics},
    js_string,
    object::{
        FunctionObjectBuilder,
        host::{
            WindowOrigin, immutable_prototype, install_window_index_reader,
            install_window_name_reader, window_properties, window_proxy,
        },
    },
    property::Attribute,
};

#[derive(Default)]
struct WindowHost {
    origin: WindowOrigin,
    proxy: Cell<Option<JsObject>>,
}

impl HostHooks for WindowHost {
    fn create_global_object(&self, intrinsics: &Intrinsics) -> JsObject {
        let shape = intrinsics.root_shape();
        let object_prototype = intrinsics.constructors().object().prototype();
        let window = immutable_prototype(shape, None);
        let names = window_properties(shape, window.clone(), object_prototype);
        let prototype = immutable_prototype(shape, Some(names));
        assert!(window.set_prototype(Some(prototype)));
        self.proxy.set(Some(window_proxy(
            shape,
            window.clone(),
            self.origin.clone(),
        )));
        window
    }

    fn create_global_this(&self, _: &Intrinsics) -> Option<JsObject> {
        self.proxy.take()
    }
}

#[test]
fn global_accessors_only_observe_the_window_proxy() {
    let mut context = Context::builder()
        .host_hooks(Rc::new(WindowHost::default()))
        .build()
        .unwrap();
    let result = context
        .eval(Source::from_bytes(
            r#"
        let actualReceiver;
        Object.defineProperty(globalThis, "windowAccess", {
            get() { return this; },
            set(value) { actualReceiver = [this, value]; }
        });
        function readBinding() { return windowAccess; }
        windowAccess = 42;
        readBinding() === globalThis && readBinding() === globalThis &&
            actualReceiver[0] === globalThis && actualReceiver[1] === 42
    "#,
        ))
        .unwrap();
    assert_eq!(result.as_boolean(), Some(true));
}

#[test]
fn a_global_accessor_can_remove_its_own_lookup_slot() {
    for mut context in [
        Context::default(),
        Context::builder()
            .host_hooks(Rc::new(WindowHost::default()))
            .build()
            .unwrap(),
    ] {
        let result = context
            .eval(Source::from_bytes(
                r#"
                Object.defineProperty(globalThis, "ephemeralName", {
                    get() {
                        if (this !== globalThis) throw Error("unexpected receiver");
                        Reflect.deleteProperty(this, "ephemeralName");
                        return 42;
                    },
                    configurable: true
                });
                function readBinding() { return ephemeralName; }
                if (readBinding() !== 42) throw Error("unexpected first result");
                try { readBinding(); false; }
                catch (error) { error instanceof ReferenceError; }
                "#,
            ))
            .unwrap();
        assert_eq!(result.as_boolean(), Some(true));
    }
}

#[test]
fn window_names_follow_the_current_ordinary_prototype_descriptors() {
    let mut context = Context::builder()
        .host_hooks(Rc::new(WindowHost::default()))
        .build()
        .unwrap();
    let window = context.global_object();
    let names = window.prototype().unwrap().prototype().unwrap();
    let reader = FunctionObjectBuilder::new(
        context.realm(),
        NativeFunction::from_fn_ptr(|_, arguments, _| {
            Ok(
                if arguments
                    .first()
                    .and_then(JsValue::as_string)
                    .is_some_and(|name| name == js_string!("exposedName"))
                {
                    JsValue::from(js_string!("native"))
                } else {
                    JsValue::undefined()
                },
            )
        }),
    )
    .build();
    assert!(install_window_name_reader(&names, reader.into()));

    let result = context
        .eval(Source::from_bytes(
            r#"
        const prototype = Object.getPrototypeOf(globalThis);
        Object.defineProperty(prototype, "exposedName", { value: "mask", configurable: true });
        function readBinding() { return exposedName; }
        if (readBinding() !== "mask" || readBinding() !== "mask") throw Error("fixture");
        Reflect.deleteProperty(prototype, "exposedName");
        readBinding() === "native" && globalThis.exposedName === "native" &&
            !Reflect.ownKeys(Object.getPrototypeOf(prototype)).includes("exposedName")
    "#,
        ))
        .unwrap();
    assert_eq!(result.as_boolean(), Some(true));
}

#[test]
fn numeric_window_reflection_uses_actual_child_proxies() {
    let mut context = Context::builder()
        .host_hooks(Rc::new(WindowHost::default()))
        .build()
        .unwrap();
    let realm = context.create_realm().unwrap();
    let current = context.enter_realm(realm);
    let child = context.global_this();
    context.enter_realm(current);
    context
        .register_global_property(js_string!("expectedChild"), child.clone(), Attribute::all())
        .unwrap();
    let reader = FunctionObjectBuilder::new(
        context.realm(),
        NativeFunction::from_copy_closure_with_captures(
            |_, arguments, child, _| {
                let Some(index) = arguments.first().and_then(JsValue::as_number) else {
                    return Ok(JsValue::new(1));
                };
                if arguments.len() == 2 {
                    return Ok(JsValue::new(index == 0.0));
                }
                Ok(if index == 0.0 {
                    child.clone().into()
                } else {
                    JsValue::undefined()
                })
            },
            child,
        ),
    )
    .build();
    assert!(install_window_index_reader(
        &context.global_this(),
        reader.into()
    ));
    let result = context
        .eval(Source::from_bytes(
            r#"
        const target = globalThis;
        const descriptor = Reflect.getOwnPropertyDescriptor(target, "0");
        target[0] === expectedChild && descriptor.value === expectedChild &&
            !descriptor.writable && descriptor.enumerable && descriptor.configurable &&
            Reflect.ownKeys(target)[0] === "0" && 0 in target && !(1 in target) &&
            !Reflect.set(target, "0", 2, {}) && !Reflect.set(target, "1", 2) &&
            !Reflect.defineProperty(target, "1", {value: 2}) &&
            !Reflect.deleteProperty(target, "0") && Reflect.deleteProperty(target, "1") &&
            Object.isExtensible(target) && !Reflect.preventExtensions(target)
    "#,
        ))
        .unwrap();
    assert_eq!(result.as_boolean(), Some(true));
}

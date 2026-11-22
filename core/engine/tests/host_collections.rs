#![allow(unused_crate_dependencies)]
//! Property operations on native legacy collection objects.

use boa_engine::{
    Context, JsError, JsObject, JsResult, JsString, JsValue, NativeFunction, Source, js_string,
    object::host::{
        IndexedProperties, NamedOverrideProperties, named_override, named_override_source,
        readonly_indexed,
    },
    property::Attribute,
};
use boa_gc::{Finalize, Gc, GcRefCell, Trace};

#[derive(Clone, Trace, Finalize)]
struct Collection {
    names: Gc<GcRefCell<Vec<JsString>>>,
    name_reads: Gc<GcRefCell<usize>>,
}

impl Collection {
    fn new(names: Vec<JsString>) -> Self {
        Self {
            names: Gc::new(GcRefCell::new(names)),
            name_reads: Gc::new(GcRefCell::new(0)),
        }
    }
}

impl IndexedProperties for Collection {
    const HAS_NAMED_PROPERTIES: bool = true;

    fn index_count(&self, _: &JsObject, _: &mut Context) -> JsResult<u32> {
        Ok(1)
    }

    fn indexed_value(
        &self,
        _: &JsObject,
        index: u32,
        _: &mut Context,
    ) -> JsResult<Option<JsValue>> {
        Ok((index == 0).then(|| js_string!("indexed").into()))
    }

    fn named_value(
        &self,
        _: &JsObject,
        name: &JsString,
        _: &mut Context,
    ) -> JsResult<Option<JsValue>> {
        *self.name_reads.borrow_mut() += 1;
        Ok(self
            .names
            .borrow()
            .contains(name)
            .then(|| js_string!("native").into()))
    }

    fn named_present(&self, _: &JsObject, name: &JsString, _: &mut Context) -> JsResult<bool> {
        *self.name_reads.borrow_mut() += 1;
        Ok(self.names.borrow().contains(name))
    }

    fn named_keys(&self, _: &JsObject, _: &mut Context) -> JsResult<Vec<JsString>> {
        Ok(self.names.borrow().clone())
    }
}

fn register_collection(context: &mut Context, names: Vec<JsString>) -> Collection {
    let source = Collection::new(names);
    let prototype = JsObject::with_object_proto(context.intrinsics());
    let collection = readonly_indexed(context.root_shape(), Some(prototype), source.clone());
    context
        .register_global_property(js_string!("collection"), collection, Attribute::all())
        .unwrap();
    source
}

impl NamedOverrideProperties for Collection {
    fn named_keys(&self, holder: &JsObject, context: &mut Context) -> JsResult<Vec<JsString>> {
        IndexedProperties::named_keys(self, holder, context)
    }

    fn named_value(
        &self,
        holder: &JsObject,
        name: &JsString,
        context: &mut Context,
    ) -> JsResult<Option<JsValue>> {
        if *name == js_string!("providerError")
            && IndexedProperties::named_present(self, holder, name, context)?
        {
            let error = context
                .global_object()
                .get(js_string!("nativeError"), context)?;
            return Err(JsError::from_opaque(error));
        }
        IndexedProperties::named_value(self, holder, name, context)
    }

    fn named_present(
        &self,
        holder: &JsObject,
        name: &JsString,
        context: &mut Context,
    ) -> JsResult<bool> {
        IndexedProperties::named_present(self, holder, name, context)
    }

    fn is_unforgeable_name(&self, name: &JsString) -> bool {
        *name == js_string!("unforgeable")
    }
}

fn register_named_override(context: &mut Context, names: Vec<JsString>) -> JsResult<Collection> {
    let source = Collection::new(names);
    let prototype = JsObject::with_object_proto(context.intrinsics());
    let host = named_override(context.root_shape(), Some(prototype), source.clone());
    assert!(named_override_source::<Collection>(&host).is_some());
    context.register_global_property(js_string!("host"), host, Attribute::all())?;
    context.register_global_builtin_callable(
        js_string!("supportName"),
        1,
        NativeFunction::from_copy_closure_with_captures(
            |_, args, names, _| {
                let name = args.first().and_then(JsValue::as_string).unwrap();
                names.borrow_mut().push(name);
                Ok(JsValue::undefined())
            },
            source.names.clone(),
        ),
    )?;
    Ok(source)
}

#[test]
fn named_override_uses_exact_names_and_ordinary_creation_order() -> JsResult<()> {
    let mut context = Context::default();
    let source = register_named_override(
        &mut context,
        ["first", "99", "0", "01", "4294967295", "99"]
            .into_iter()
            .map(JsString::from)
            .collect(),
    )?;
    let observed = context.eval(Source::from_bytes(
        r#"
        const prototype = Object.getPrototypeOf(host);
        prototype.first = "inherited";
        prototype[99] = "inherited";
        const sym = Symbol("ordinary");
        host[8] = "eight"; host[sym] = "symbol"; host.middle = "middle"; host[2] = "two";
        const keys = Reflect.ownKeys(new Proxy(host, {}));
        const descriptor = Object.getOwnPropertyDescriptor(host, 99);
        [
            host.first === "native" && host[99] === "native" && host[0] === "native"
                && host["01"] === "native" && host["4294967295"] === "native",
            descriptor?.value === "native" && !descriptor.writable
                && descriptor.enumerable && descriptor.configurable,
            keys.slice(0, -1).join() === "first,99,0,01,4294967295,8,middle,2"
                && keys.at(-1) === sym && host[2] === "two",
            Reflect.deleteProperty(host, 8) && Reflect.set(host, 8, "re-added")
                && Object.keys(host).join() === "first,99,0,01,4294967295,middle,2,8",
            !Reflect.preventExtensions(host) && Object.isExtensible(host)
        ].join(";")
        "#,
    ))?;
    assert_eq!(
        observed.as_string(),
        Some(js_string!("true;true;true;true;true"))
    );
    source
        .names
        .borrow_mut()
        .retain(|name| *name != js_string!("99"));
    let after = context.eval(Source::from_bytes(
        r#"host[99] === "inherited" && !Object.hasOwn(host, 99)"#,
    ))?;
    assert_eq!(after.as_boolean(), Some(true));
    Ok(())
}

#[test]
fn named_override_keeps_live_write_decisions_and_ordinary_receivers() -> JsResult<()> {
    let mut context = Context::default();
    register_named_override(&mut context, vec![js_string!("named")])?;
    let observed = context.eval(Source::from_bytes(
        r#"
        const prototype = Object.getPrototypeOf(host);
        prototype.inherited = 1;
        const read = () => host.inherited;
        function write(value) { "use strict"; host.warm = value; }
        let warm = true;
        for (let n = 0; n < 32; ++n) {
            warm &&= read() === 1;
            write(n);
        }
        supportName("inherited");
        supportName("warm");
        let refused = false;
        try { write(33); } catch (err) { refused = err instanceof TypeError; }
        const overrideRead = warm && read() === "native";
        const ownRemained = refused && host.warm === 31
            && !Reflect.defineProperty(host, "warm", {value: 34})
            && Reflect.deleteProperty(host, "warm") && host.warm === "native";

        Object.defineProperty(host, "unforgeable", {value: 1, writable: true});
        supportName("unforgeable");
        const unforgeable = Reflect.defineProperty(host, "unforgeable", {value: 2})
            && host.unforgeable === 2 && !Reflect.deleteProperty(host, "unforgeable");

        let setterReceiver;
        Object.defineProperty(prototype, "named", {set(_) { setterReceiver = this; }});
        const alternate = {};
        const setter = Reflect.set(host, "named", 3, alternate) && setterReceiver === alternate;
        const descendant = Object.create(host);
        const own = Reflect.set(host, "warm", 5, descendant)
            && descendant.warm === 5 && host.warm === "native";

        let descriptorConversions = 0;
        const reentered = !Reflect.defineProperty(host, 42, {get value() {
            ++descriptorConversions;
            supportName("42");
            return 7;
        }}) && descriptorConversions === 1 && host[42] === "native";

        const sentinel = {};
        Object.setPrototypeOf(host, new Proxy(prototype, {
            getOwnPropertyDescriptor() { throw sentinel; },
            get() { throw sentinel; },
            has() { throw sentinel; }
        }));
        let ordinaryThrow = false;
        try { void host.absent; } catch (err) { ordinaryThrow = err === sentinel; }
        const skipPrototype = host.named === "native" && Object.hasOwn(host, "named")
            && Reflect.has(host, 42) && !Reflect.deleteProperty(host, "named")
            && !Reflect.defineProperty(host, "named", {value: 8}) && ordinaryThrow;
        [overrideRead, ownRemained, unforgeable, setter, own, reentered, skipPrototype].join(";")
        "#,
    ))?;
    assert_eq!(
        observed.as_string(),
        Some(js_string!("true;true;true;true;true;true;true"))
    );
    Ok(())
}

#[test]
fn named_override_inherited_read_resolves_again_after_own_deletion() -> JsResult<()> {
    let mut context = Context::default();
    register_named_override(&mut context, Vec::new())?;
    let observed = context.eval(Source::from_bytes(
        r#"
        host.switchable = "own";
        host.spare = "spare";
        const child = Object.create(host);
        const ordinary = {switchable: "ordinary"};
        const ordinaryChild = Object.create(ordinary);
        const read = object => object.switchable;
        for (let n = 0; n < 32; ++n) {
            if (read(child) !== "own" || read(ordinaryChild) !== "ordinary") throw Error("warm read");
        }
        const deleted = Reflect.deleteProperty(host, "switchable");
        supportName("switchable");
        ordinary.switchable = "changed ordinary";
        [deleted, Reflect.get(child, "switchable"), read(child), read(ordinaryChild)].join(";")
        "#,
    ))?;
    assert_eq!(
        observed.as_string(),
        Some(js_string!("true;native;native;changed ordinary")),
    );
    Ok(())
}

#[test]
fn named_override_has_property_preserves_the_selected_getter_error() -> JsResult<()> {
    let mut context = Context::default();
    register_named_override(
        &mut context,
        vec![js_string!("present"), js_string!("providerError")],
    )?;
    let observed = context.eval(Source::from_bytes(
        r#"
        globalThis.nativeError = {};
        Object.getPrototypeOf(host).inherited = 3;
        let thrown = false;
        try { Reflect.has(host, "providerError"); } catch (err) { thrown = err === nativeError; }
        [
            "present" in host && !("missing" in host) && "inherited" in host,
            thrown,
            !Reflect.defineProperty(host, "providerError", {value: 1}),
            !Reflect.deleteProperty(host, "providerError")
        ].join(";")
        "#,
    ))?;
    assert_eq!(
        observed.as_string(),
        Some(js_string!("true;true;true;true"))
    );
    Ok(())
}

#[test]
fn named_override_super_write_uses_the_actual_receiver_after_warming() -> JsResult<()> {
    let mut context = Context::default();
    register_named_override(&mut context, vec![js_string!("blocked")])?;
    let observed = context.eval(Source::from_bytes(
        r#"
        const base = {blocked: -1};
        const write = {__proto__: base, write(value) { "use strict"; super.blocked = value; }}.write;
        for (let n = 0; n < 32; ++n) {
            Reflect.apply(write, base, [n]);
        }
        if (base.blocked !== 31) throw Error("warm write");
        const ordinary = {};
        Reflect.apply(write, ordinary, [32]);
        const ordinaryCorrect = ordinary.blocked === 32 && base.blocked === 31;
        let refused = false;
        try { Reflect.apply(write, host, [33]); } catch (err) { refused = err instanceof TypeError; }
        [ordinaryCorrect, refused, host.blocked, base.blocked].join(";")
        "#,
    ))?;
    assert_eq!(
        observed.as_string(),
        Some(js_string!("true;true;native;31"))
    );
    Ok(())
}

#[test]
fn value_iterable_intrinsics_are_original_and_realm_specific() -> JsResult<()> {
    fn observe_realm(context: &mut Context) -> JsResult<[JsValue; 4]> {
        let names = ["entries", "keys", "forEach", "values"];
        let originals = context
            .eval(Source::from_bytes(
                r"
                const originalArrayMethods = Array.prototype;
                const descriptors = Object.getOwnPropertyDescriptors(originalArrayMethods);
                for (const [name, length] of [['entries', 0], ['keys', 0], ['forEach', 1], ['values', 0]]) {
                    const {value, writable, enumerable, configurable} = descriptors[name];
                    if (value.name !== name || value.length !== length || !writable || enumerable || !configurable)
                        throw new Error('unexpected Array method: ' + name);
                    Object.defineProperty(originalArrayMethods, name, {
                        get() { throw new Error('guest Array getter ran'); }
                    });
                }
                globalThis.Array = null;
                Object.fromEntries(Object.entries(descriptors).map(([name, desc]) => [name, desc.value]));
                ",
            ))?
            .as_object()
            .expect("the witness returns its saved original functions");
        let intrinsics = context.intrinsics().objects();
        let recorded: [JsValue; 4] = [
            intrinsics.array_prototype_entries().into(),
            intrinsics.array_prototype_keys().into(),
            intrinsics.array_prototype_for_each().into(),
            intrinsics.array_prototype_values().into(),
        ];
        for (name, actual) in names.into_iter().zip(&recorded) {
            assert!(
                originals
                    .get(JsString::from(name), context)?
                    .strict_equals(actual),
                "{name}"
            );
        }
        Ok(recorded)
    }

    let mut context = Context::default();
    let first = observe_realm(&mut context)?;
    let other = context.create_realm()?;
    let initial = context.enter_realm(other);
    let second = observe_realm(&mut context)?;
    context.enter_realm(initial);
    for (left, right) in first.iter().zip(&second) {
        assert!(!left.strict_equals(right));
    }
    Ok(())
}

#[test]
fn named_collection_assignment_uses_ordinary_receivers() {
    let mut context = Context::default();
    let source = register_collection(&mut context, vec![js_string!("named")]);

    let assigned = context
        .eval(Source::from_bytes(
            r#"
            const descendant = Object.create(collection);
            descendant.named = "descendant";
            const receiver = {};
            const accepted = Reflect.set(collection, "named", "receiver", receiver);
            const own = Object.getOwnPropertyDescriptor(descendant, "named");
            accepted && own?.value === "descendant" &&
                own.writable && own.enumerable && own.configurable &&
                Object.hasOwn(receiver, "named") && receiver.named === "receiver"
            "#,
        ))
        .unwrap();
    assert_eq!(assigned.as_boolean(), Some(true));
    assert_eq!(
        *source.name_reads.borrow(),
        0,
        "assignment must skip the native name"
    );

    let controls = context
        .eval(Source::from_bytes(
            r#"
            !Reflect.set(collection, "named", "overwrite") &&
                collection.named === "native" &&
                !Reflect.set(descendant, "0", "overwrite index") &&
                !Object.hasOwn(descendant, "0") &&
                collection[0] === "indexed" &&
                Reflect.set(collection, "ordinary", 42) && collection.ordinary === 42
            "#,
        ))
        .unwrap();
    assert_eq!(controls.as_boolean(), Some(true));

    let setter_calls = context
        .eval(Source::from_bytes(
            r#"
            const prototype = Object.getPrototypeOf(collection);
            let inheritedCalls = 0;
            let inheritedReceiver;
            Object.defineProperty(prototype, "inherited", {
                set(value) {
                    inheritedCalls++;
                    inheritedReceiver = this;
                    Object.defineProperty(this, "inherited", { value, writable: true });
                }
            });
            function writeInherited(value) { collection.inherited = value; }
            writeInherited(17);
            writeInherited(18);
            inheritedCalls
            "#,
        ))
        .unwrap();
    assert_eq!(setter_calls.as_number(), Some(1.0));
    let own = context
        .eval(Source::from_bytes(
            r#"inheritedReceiver === collection &&
                Object.hasOwn(collection, "inherited") && collection.inherited === 18"#,
        ))
        .unwrap();
    assert_eq!(own.as_boolean(), Some(true));
}

#[test]
fn supported_collection_name_refuses_definition_even_when_inherited() -> JsResult<()> {
    let mut context = Context::default();
    let source = register_collection(&mut context, vec![js_string!("item"), js_string!("masked")]);
    context.eval(Source::from_bytes(
        r#"
        const prototype = Object.getPrototypeOf(collection);
        prototype.item = function item() {};
        prototype.masked = "inherited";
        Object.defineProperty(collection, "shadow", { value: 1, writable: true });
        if (collection.item !== prototype.item || Object.hasOwn(collection, "item")) {
            throw Error("an inherited method must mask the supported name");
        }
        "#,
    ))?;
    source.names.borrow_mut().push(js_string!("shadow"));
    let observation = context.eval(Source::from_bytes(
        r#"
        const shadowUpdated = Reflect.defineProperty(collection, "shadow", { value: 2 });
        const inheritedDefined = Reflect.defineProperty(collection, "item", { value: 3 });
        let descriptorProbes = 0;
        Object.setPrototypeOf(collection, new Proxy(prototype, {
            getOwnPropertyDescriptor(target, key) {
                descriptorProbes++;
                return Reflect.getOwnPropertyDescriptor(target, key);
            }
        }));
        let descriptorConversions = 0;
        const proxyDefined = Reflect.defineProperty(collection, "masked", {
            get value() { descriptorConversions++; return 4; }
        });
        [shadowUpdated, collection.shadow, inheritedDefined, proxyDefined,
            descriptorProbes, descriptorConversions].join(",")
        "#,
    ))?;
    assert_eq!(
        observation.as_string(),
        Some(js_string!("true,2,false,false,0,1")),
        "a real own shadow stays writable, while supported-name definition refuses without consulting the prototype",
    );
    Ok(())
}

#[test]
fn numeric_collection_names_are_enumerated_after_indices_without_duplicates() -> JsResult<()> {
    let mut context = Context::default();
    register_collection(
        &mut context,
        vec![
            js_string!("before"),
            js_string!("99"),
            js_string!("0"),
            js_string!("01"),
            js_string!("4294967295"),
        ],
    );
    let reads = context.eval(Source::from_bytes(
        r#"
        collection[0] === "indexed" &&
            collection[99] === undefined &&
            Object.getOwnPropertyDescriptor(collection, "99") === undefined &&
            collection["01"] === "native" &&
            collection["4294967295"] === "native"
        "#,
    ))?;
    assert_eq!(
        reads.as_boolean(),
        Some(true),
        "array-index lookup takes priority"
    );

    let keys = context.eval(Source::from_bytes(
        r#"Reflect.ownKeys(collection).join(",")"#,
    ))?;
    assert_eq!(
        keys.as_string(),
        Some(js_string!("0,before,99,01,4294967295")),
        "an out-of-range numeric name belongs to the named key group and an indexed collision appears once",
    );
    Ok(())
}

#[test]
fn repeated_collection_name_checks_visibility_once() -> JsResult<()> {
    let mut context = Context::default();
    register_collection(
        &mut context,
        vec![js_string!("99"), js_string!("99"), js_string!("visible")],
    );
    let keys = context.eval(Source::from_bytes(
        r#"
        let seen = 0;
        Object.setPrototypeOf(collection, new Proxy({}, {
            getOwnPropertyDescriptor(_, key) {
                if (key === "99" && ++seen === 1) {
                    return {value: "mask", configurable: true};
                }
                return undefined;
            }
        }));
        [Reflect.ownKeys(collection).join(","), seen].join(";")
        "#,
    ))?;
    assert_eq!(
        keys.as_string(),
        Some(js_string!("0,visible;1")),
        "one supported name gets one visibility observation",
    );
    Ok(())
}

#[test]
fn reentrant_collection_keys_keep_the_first_emission() -> JsResult<()> {
    let mut context = Context::default();
    let source = register_collection(&mut context, vec![js_string!("before"), js_string!("99")]);
    context.register_global_builtin_callable(
        js_string!("removeBefore"),
        0,
        NativeFunction::from_copy_closure_with_captures(
            |_, _, names, _| {
                names
                    .borrow_mut()
                    .retain(|name| *name != js_string!("before"));
                Ok(JsValue::undefined())
            },
            source.names.clone(),
        ),
    )?;
    let observed = context.eval(Source::from_bytes(
        r#"
        Object.setPrototypeOf(collection, new Proxy({}, {
            getOwnPropertyDescriptor(_, key) {
                if (key === "99") {
                    removeBefore();
                    Object.defineProperty(collection, "before", {value: 3, configurable: true});
                    Object.defineProperty(collection, "later", {value: 4, configurable: true});
                }
                return undefined;
            }
        }));
        [Reflect.ownKeys(collection).join(","), collection.before, collection.later].join(";")
        "#,
    ))?;
    assert_eq!(
        observed.as_string(),
        Some(js_string!("0,before,99,later;3;4")),
        "a key emitted during the named pass must occur once after reentry",
    );
    Ok(())
}

#[test]
fn named_key_removed_before_visibility_skips_the_later_probe() -> JsResult<()> {
    let mut context = Context::default();
    let source = register_collection(&mut context, vec![js_string!("before"), js_string!("99")]);
    context.register_global_builtin_callable(
        js_string!("removeLater"),
        0,
        NativeFunction::from_copy_closure_with_captures(
            |_, _, names, _| {
                names.borrow_mut().retain(|name| *name != js_string!("99"));
                Ok(JsValue::undefined())
            },
            source.names.clone(),
        ),
    )?;
    let observed = context.eval(Source::from_bytes(
        r#"
        const observed = [];
        Object.setPrototypeOf(collection, new Proxy({}, {
            getOwnPropertyDescriptor(_, key) {
                observed.push(key);
                if (key === "before") removeLater();
                return undefined;
            }
        }));
        [Reflect.ownKeys(collection).join(","), observed.join(",")].join(";")
        "#,
    ))?;
    assert_eq!(
        observed.as_string(),
        Some(js_string!("0,before;before")),
        "after reentry a later name must still be supported before prototype access",
    );
    Ok(())
}

#[test]
fn named_collection_rechecks_values_after_prototype_reentry() -> JsResult<()> {
    let mut context = Context::default();
    let source = register_collection(
        &mut context,
        ["nullable", "ordinary", "present", "deleted"]
            .into_iter()
            .map(JsString::from)
            .collect(),
    );
    context.register_global_builtin_callable(
        js_string!("forgetName"),
        1,
        NativeFunction::from_copy_closure_with_captures(
            |_, args, names, _| {
                if let Some(name) = args.first().and_then(JsValue::as_string) {
                    names.borrow_mut().retain(|candidate| *candidate != name);
                }
                Ok(JsValue::undefined())
            },
            source.names.clone(),
        ),
    )?;
    let observed = context.eval(Source::from_bytes(
        r#"
        Object.setPrototypeOf(collection, new Proxy({}, {
            getOwnPropertyDescriptor(_, key) {
                forgetName(key);
                if (key === "nullable") return undefined;
                Reflect.defineProperty(collection, key, {value: 7, configurable: true});
                return {value: "mask", configurable: true};
            }
        }));
        const blank = Reflect.getOwnPropertyDescriptor(collection, "nullable");
        const ordinary = Reflect.getOwnPropertyDescriptor(collection, "ordinary");
        const present = Reflect.has(collection, "present");
        const deleted = Reflect.deleteProperty(collection, "deleted");
        [
            blank?.value === null && blank.configurable && !blank.enumerable && !blank.writable,
            ordinary?.value === 7,
            present,
            deleted && !Object.hasOwn(collection, "deleted")
        ].join(";")
        "#,
    ))?;
    assert_eq!(
        observed.as_string(),
        Some(js_string!("true;true;true;true")),
        "the selected getter and ordinary fallbacks must read after the prototype callback",
    );
    Ok(())
}

#[test]
#[cfg(feature = "annex-b")]
fn callable_collection_has_html_dda_and_exact_call_identity() -> JsResult<()> {
    use boa_engine::{
        builtins::is_html_dda::IsHTMLDDA,
        object::{builtins::JsFunction, host::readonly_indexed_with_html_dda},
    };

    let mut context = Context::default();
    let callback = context.eval(Source::from_bytes(
        r#"
        const marker = {};
        (function (...args) {
            if (args[0] === "throw") throw marker;
            if (args[0] === "nested") {
                return Reflect.apply(this, {unrelated: true}, args.slice(1));
            }
            return [this, args];
        })
        "#,
    ))?;
    let callback = callback
        .as_object()
        .and_then(JsFunction::from_object)
        .unwrap();
    let source = Collection::new(vec![js_string!("named")]);
    let prototype = JsObject::with_object_proto(context.intrinsics());
    let collection = readonly_indexed_with_html_dda(
        context.root_shape(),
        Some(prototype),
        source.clone(),
        callback.clone(),
    );
    let other =
        readonly_indexed_with_html_dda(context.root_shape(), None, source.clone(), callback);
    let harness = JsObject::from_proto_and_data(None, IsHTMLDDA);
    context.register_global_property(js_string!("collection"), collection, Attribute::all())?;
    context.register_global_property(js_string!("otherCollection"), other, Attribute::all())?;
    context.register_global_property(js_string!("harness"), harness, Attribute::all())?;

    let observed = context.eval(Source::from_bytes(
        r#"
        const arg = {};
        const thrownBy = (action, expected) => {
            try { action(); } catch (error) { return error === expected; }
            return false;
        };
        const exact = (action, holder, args) => {
            try {
                const [seen, values] = action();
                return seen === holder && values.length === args.length
                    && values.every((value, index) => Object.is(value, args[index]));
            } catch (_) { return false; }
        };
        const prototype = Object.getPrototypeOf(collection);
        prototype.inherited = "ordinary";
        const instance = collection;
        const ordinary = Object.create(collection);
        const proxy = new Proxy(collection, {});
        let constructible = true;
        try { new collection(); } catch (error) { constructible = !(error instanceof TypeError); }
        [
            typeof collection === "undefined" && !Boolean(collection),
            collection == null && null == collection && collection == undefined && undefined == collection,
            collection !== null && collection !== undefined && Object.is(collection, instance)
                && collection !== otherCollection,
            typeof ordinary === "object" && Boolean(ordinary) && ordinary != null,
            typeof proxy === "function" && Boolean(proxy) && proxy != null,
            !constructible && collection.inherited === "ordinary" && Object.getPrototypeOf(collection) === prototype,
            collection[0] === "indexed" && collection.named === "native"
                && Reflect.ownKeys(collection).join() === "0,named",
            exact(() => collection(arg, undefined, 3), collection, [arg, undefined, 3]),
            exact(() => Reflect.apply(collection, otherCollection, [arg, 7]), collection, [arg, 7]),
            exact(() => Reflect.apply(otherCollection, collection, []), otherCollection, []),
            exact(() => proxy(arg), collection, [arg]),
            exact(() => collection("nested", arg, null), collection, [arg, null]),
            thrownBy(() => Reflect.apply(otherCollection, arg, ["throw"]), marker),
            exact(() => otherCollection(arg), otherCollection, [arg]),
            typeof harness === "undefined" && !Boolean(harness) && harness == null
                && harness !== undefined && harness() === undefined
        ].join(";")
        "#,
    ))?;
    assert_eq!(
        observed.as_string(),
        Some(js_string!(
            "true;true;true;true;true;true;true;true;true;true;true;true;true;true;true"
        )),
        "a callable collection keeps Annex B, property, identity and callback semantics together",
    );
    source.names.borrow_mut().clear();
    let after = context.eval(Source::from_bytes(
        "collection.named === undefined && Reflect.ownKeys(collection).join() === '0'",
    ))?;
    assert_eq!(after.as_boolean(), Some(true));
    Ok(())
}

#[test]
#[cfg(feature = "annex-b")]
fn callable_collection_keeps_its_callback_for_the_holder_lifetime() {
    use boa_engine::object::{builtins::JsFunction, host::readonly_indexed_with_html_dda};

    let mut context = Context::default();
    let callback = context
        .eval(Source::from_bytes("() => 1"))
        .unwrap()
        .as_object()
        .unwrap();
    let weak = callback.downgrade();
    let collection = readonly_indexed_with_html_dda(
        context.root_shape(),
        None,
        Collection::new(Vec::new()),
        JsFunction::from_object(callback).unwrap(),
    );
    boa_gc::force_collect();
    assert!(weak.upgrade().is_some());
    drop(collection);
    boa_gc::force_collect();
    assert!(weak.upgrade().is_none());
}

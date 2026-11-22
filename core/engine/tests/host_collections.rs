#![allow(unused_crate_dependencies)]
//! Property operations on native legacy collection objects.

use boa_engine::{
    Context, JsObject, JsResult, JsString, JsValue, NativeFunction, Source, js_string,
    object::host::{IndexedProperties, readonly_indexed},
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

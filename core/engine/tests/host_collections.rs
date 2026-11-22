#![allow(unused_crate_dependencies)]
//! Property operations on native legacy collection objects.

use boa_engine::{
    Context, JsObject, JsResult, JsString, JsValue, Source, js_string,
    object::host::{IndexedProperties, readonly_indexed},
    property::Attribute,
};
use boa_gc::{Finalize, Gc, GcRefCell, Trace};

#[derive(Clone, Trace, Finalize)]
struct Collection {
    names: Gc<GcRefCell<Vec<JsString>>>,
    name_reads: Gc<GcRefCell<usize>>,
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
    let source = Collection {
        names: Gc::new(GcRefCell::new(names)),
        name_reads: Gc::new(GcRefCell::new(0)),
    };
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

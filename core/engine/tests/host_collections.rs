#![allow(unused_crate_dependencies)]
//! Assignment through native legacy collection objects.

use boa_engine::{
    Context, JsObject, JsResult, JsString, JsValue, Source, js_string,
    object::host::{IndexedProperties, readonly_indexed},
    property::Attribute,
};
use boa_gc::{Finalize, Gc, GcRefCell, Trace};

#[derive(Trace, Finalize)]
struct Collection {
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
        Ok((*name == js_string!("named")).then(|| js_string!("native").into()))
    }
}

#[test]
fn named_collection_assignment_uses_ordinary_receivers() {
    let mut context = Context::default();
    let name_reads = Gc::new(GcRefCell::new(0));
    let prototype = JsObject::with_object_proto(context.intrinsics());
    let collection = readonly_indexed(
        context.root_shape(),
        Some(prototype),
        Collection {
            name_reads: Gc::clone(&name_reads),
        },
    );
    context
        .register_global_property(js_string!("collection"), collection, Attribute::all())
        .unwrap();

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
        *name_reads.borrow(),
        0,
        "assignment must skip the native name"
    );

    let controls = context
        .eval(Source::from_bytes(
            r#"
            const prototype = Object.getPrototypeOf(collection);
            let called = false;
            Object.defineProperty(prototype, "inherited", {
                set(value) { called = this === collection && value === 17; }
            });
            !Reflect.set(collection, "named", "overwrite") &&
                collection.named === "native" &&
                !Reflect.set(descendant, "0", "overwrite index") &&
                !Object.hasOwn(descendant, "0") &&
                collection[0] === "indexed" &&
                Reflect.set(collection, "ordinary", 42) && collection.ordinary === 42 &&
                Reflect.set(collection, "inherited", 17) && called &&
                !Object.hasOwn(collection, "inherited")
            "#,
        ))
        .unwrap();
    assert_eq!(controls.as_boolean(), Some(true));
}

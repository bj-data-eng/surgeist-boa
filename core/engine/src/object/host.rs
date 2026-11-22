//! Native host objects with selected ECMAScript internal methods.

use boa_gc::{Finalize, Gc, Trace};

use crate::{
    Context, JsNativeError, JsResult, JsValue,
    property::{PropertyDescriptor, PropertyKey},
};

use super::{
    JsData, JsObject, JsPrototype,
    internal_methods::{
        InternalMethodPropertyContext, InternalObjectMethods, ORDINARY_INTERNAL_METHODS,
        ordinary_define_own_property, ordinary_delete, ordinary_get_own_property,
        ordinary_own_property_keys,
    },
    shape::slot::SlotAttributes,
};

/// A live source of contiguous, read-only indexed properties.
///
/// Each method is called afresh for the guest operation. Use the original
/// native collection state and return its existing guest values.
pub trait IndexedProperties: Trace + 'static {
    /// Gets the current number of supported indices.
    fn index_count(&self, context: &mut Context) -> JsResult<u32>;

    /// Gets a current value, or absence if the index is unsupported.
    fn indexed_value(&self, index: u32, context: &mut Context) -> JsResult<Option<JsValue>>;
}

/// Creates a read-only legacy indexed object with ordinary prototype methods.
///
/// Guest code can define ordinary string and symbol properties but cannot
/// create its own index, delete a supported index, or prevent extensions.
#[must_use]
pub fn readonly_indexed<T: IndexedProperties>(prototype: JsPrototype, source: T) -> JsObject {
    JsObject::from_proto_and_data(prototype, ReadOnlyIndices(Gc::new(source)))
}

#[derive(Trace, Finalize)]
struct ReadOnlyIndices<T: IndexedProperties>(Gc<T>);

impl<T: IndexedProperties> JsData for ReadOnlyIndices<T> {
    fn internal_methods(&self) -> &'static InternalObjectMethods {
        &Self::METHODS
    }
}

impl<T: IndexedProperties> ReadOnlyIndices<T> {
    const METHODS: InternalObjectMethods = InternalObjectMethods {
        __get_own_property__: Self::get_own_property,
        __define_own_property__: |object, key, descriptor, context| {
            if matches!(key, PropertyKey::Index(_)) {
                return Ok(false);
            }
            ordinary_define_own_property(object, key, descriptor, context)
        },
        __delete__: Self::delete,
        __own_property_keys__: Self::own_property_keys,
        __prevent_extensions__: |_, _| Ok(false),
        ..ORDINARY_INTERNAL_METHODS
    };

    fn source(object: &JsObject) -> JsResult<Gc<T>> {
        object
            .downcast_ref::<Self>()
            .map(|data| Gc::clone(&data.0))
            .ok_or_else(|| {
                JsNativeError::typ()
                    .with_message("Expected a native indexed object")
                    .into()
            })
    }

    fn get_own_property(
        object: &JsObject,
        key: &PropertyKey,
        context: &mut InternalMethodPropertyContext<'_>,
    ) -> JsResult<Option<PropertyDescriptor>> {
        let PropertyKey::Index(index) = key else {
            return ordinary_get_own_property(object, key, context);
        };
        context
            .slot()
            .attributes
            .insert(SlotAttributes::NOT_CACHEABLE);
        Ok(Self::source(object)?
            .indexed_value(index.get(), context)?
            .map(|value| {
                PropertyDescriptor::builder()
                    .value(value)
                    .writable(false)
                    .enumerable(true)
                    .configurable(true)
                    .build()
            }))
    }

    fn delete(
        object: &JsObject,
        key: &PropertyKey,
        context: &mut InternalMethodPropertyContext<'_>,
    ) -> JsResult<bool> {
        if let PropertyKey::Index(index) = key {
            return Ok(Self::source(object)?
                .indexed_value(index.get(), context)?
                .is_none());
        }
        ordinary_delete(object, key, context)
    }

    fn own_property_keys(object: &JsObject, context: &mut Context) -> JsResult<Vec<PropertyKey>> {
        let count = Self::source(object)?.index_count(context)?;
        let mut keys = ordinary_own_property_keys(object, context)?;
        keys.retain(|key| !matches!(key, PropertyKey::Index(_)));
        let capacity = usize::try_from(count)
            .map_err(|_| JsNativeError::range().with_message("Too many indexed property keys"))?;
        keys.try_reserve(capacity)
            .map_err(|_| JsNativeError::range().with_message("Too many indexed property keys"))?;
        keys.splice(..0, (0..count).map(PropertyKey::from));
        Ok(keys)
    }
}

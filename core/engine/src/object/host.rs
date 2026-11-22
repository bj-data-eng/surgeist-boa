//! Native host objects with selected ECMAScript internal methods.

use std::collections::HashSet;

use boa_gc::{Finalize, Gc, Trace};

use crate::{
    Context, JsNativeError, JsResult, JsString, JsSymbol, JsValue, js_string,
    property::{PropertyDescriptor, PropertyKey},
};

use super::{
    JsData, JsObject, JsPrototype,
    internal_methods::{
        InternalMethodPropertyContext, InternalObjectMethods, ORDINARY_INTERNAL_METHODS,
        immutable_prototype::{
            IMMUTABLE_PROTOTYPE_EXOTIC_INTERNAL_METHODS,
            immutable_prototype_exotic_set_prototype_of,
        },
        ordinary_define_own_property, ordinary_delete, ordinary_get_own_property,
        ordinary_has_property, ordinary_own_property_keys, validate_and_apply_property_descriptor,
    },
    shape::slot::SlotAttributes,
};

/// Creates an extensible host object with an immutable guest-visible prototype.
///
/// A host can construct a global with this object through
/// [`HostHooks::create_global_object`](crate::context::HostHooks::create_global_object).
#[must_use]
pub fn immutable_prototype(prototype: JsPrototype) -> JsObject {
    JsObject::from_proto_and_data(prototype, ImmutablePrototype)
}

/// Creates an empty Window named-properties layer.
///
/// It refuses guest definitions, deletions and attempts to prevent extensions.
/// Ordinary assignment can still invoke inherited setters on the actual receiver.
#[must_use]
pub fn window_properties(prototype: JsObject) -> JsObject {
    let object = JsObject::from_proto_and_data(Some(prototype), WindowProperties);
    object.insert_property(
        JsSymbol::to_string_tag(),
        PropertyDescriptor::builder()
            .value(js_string!("WindowProperties"))
            .writable(false)
            .enumerable(false)
            .configurable(true)
            .build(),
    );
    object
}

#[derive(Clone, Copy, Trace, Finalize)]
#[boa_gc(empty_trace)]
struct ImmutablePrototype;

impl JsData for ImmutablePrototype {
    fn internal_methods(&self) -> &'static InternalObjectMethods {
        &IMMUTABLE_PROTOTYPE_EXOTIC_INTERNAL_METHODS
    }
}

#[derive(Clone, Copy, Trace, Finalize)]
#[boa_gc(empty_trace)]
struct WindowProperties;

impl JsData for WindowProperties {
    fn internal_methods(&self) -> &'static InternalObjectMethods {
        static METHODS: InternalObjectMethods = InternalObjectMethods {
            __set_prototype_of__: immutable_prototype_exotic_set_prototype_of,
            __define_own_property__: |_, _, _, _| Ok(false),
            __delete__: |_, _, _| Ok(false),
            __prevent_extensions__: |_, _| Ok(false),
            ..ORDINARY_INTERNAL_METHODS
        };
        &METHODS
    }
}

/// A live source of contiguous, read-only indexed properties and optional names.
///
/// Each callback receives the native object that owns this source. It observes
/// the current collection state and projects values from that state.
pub trait IndexedProperties: Trace + 'static {
    /// Enables non-enumerable legacy named properties, such as HTMLCollection names.
    const HAS_NAMED_PROPERTIES: bool = false;

    /// Gets the current number of supported indices.
    fn index_count(&self, holder: &JsObject, context: &mut Context) -> JsResult<u32>;

    /// Gets the current supported index keys.
    ///
    /// The default describes a contiguous collection. Sparse sources can
    /// provide their keys directly.
    fn indexed_keys(&self, holder: &JsObject, context: &mut Context) -> JsResult<Vec<u32>> {
        let count = self.index_count(holder, context)?;
        let mut keys = Vec::new();
        keys.try_reserve(count as usize)
            .map_err(|_| JsNativeError::range().with_message("Too many indexed property keys"))?;
        keys.extend(0..count);
        Ok(keys)
    }

    /// Gets a current value, or absence if the index is unsupported.
    fn indexed_value(
        &self,
        holder: &JsObject,
        index: u32,
        context: &mut Context,
    ) -> JsResult<Option<JsValue>>;

    /// Checks whether the index is currently supported.
    fn index_present(
        &self,
        holder: &JsObject,
        index: u32,
        context: &mut Context,
    ) -> JsResult<bool> {
        Ok(self.indexed_value(holder, index, context)?.is_some())
    }

    /// Gets the current supported names in collection order.
    fn named_keys(&self, _holder: &JsObject, _context: &mut Context) -> JsResult<Vec<JsString>> {
        Ok(Vec::new())
    }

    /// Gets a current value, or absence if the name is unsupported.
    fn named_value(
        &self,
        _holder: &JsObject,
        _name: &JsString,
        _context: &mut Context,
    ) -> JsResult<Option<JsValue>> {
        Ok(None)
    }

    /// Checks whether the name is currently supported.
    fn named_present(
        &self,
        holder: &JsObject,
        name: &JsString,
        context: &mut Context,
    ) -> JsResult<bool> {
        Ok(self.named_value(holder, name, context)?.is_some())
    }
}

/// Creates a read-only legacy indexed object with ordinary prototype methods.
///
/// Guest code can define ordinary string and symbol properties. It cannot
/// create its own index, delete a supported index, or prevent extensions.
/// Ordinary properties hide legacy names.
#[must_use]
pub fn readonly_indexed<T: IndexedProperties>(prototype: JsPrototype, source: T) -> JsObject {
    JsObject::from_proto_and_data(prototype, ReadOnlyIndices(Gc::new(source)))
}

/// Returns the source if the object itself carries the specified native data.
#[must_use]
pub fn readonly_indexed_source<T: IndexedProperties>(object: &JsObject) -> Option<Gc<T>> {
    object
        .downcast_ref::<ReadOnlyIndices<T>>()
        .map(|data| Gc::clone(&data.0))
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
        __define_own_property__: Self::define_own_property,
        __has_property__: Self::has_property,
        __delete__: Self::delete,
        __own_property_keys__: Self::own_property_keys,
        __prevent_extensions__: |_, _| Ok(false),
        ..ORDINARY_INTERNAL_METHODS
    };

    fn source(object: &JsObject) -> JsResult<Gc<T>> {
        readonly_indexed_source(object).ok_or_else(|| {
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
        if let PropertyKey::Index(index) = key {
            context
                .slot()
                .attributes
                .insert(SlotAttributes::NOT_CACHEABLE);
            return Ok(Self::source(object)?
                .indexed_value(object, index.get(), context)?
                .map(|value| Self::descriptor(value, true)));
        }
        let ordinary = ordinary_get_own_property(object, key, context)?;
        if let PropertyKey::String(name) = key
            && T::HAS_NAMED_PROPERTIES
            && ordinary.is_none()
        {
            context
                .slot()
                .attributes
                .insert(SlotAttributes::NOT_CACHEABLE);
            if Self::name_is_unmasked(object, key, context)? {
                return Ok(Self::source(object)?
                    .named_value(object, name, context)?
                    .map(|value| Self::descriptor(value, false)));
            }
        }
        Ok(ordinary)
    }

    fn descriptor(value: JsValue, enumerable: bool) -> PropertyDescriptor {
        PropertyDescriptor::builder()
            .value(value)
            .writable(false)
            .enumerable(enumerable)
            .configurable(true)
            .build()
    }

    fn define_own_property(
        object: &JsObject,
        key: &PropertyKey,
        descriptor: PropertyDescriptor,
        context: &mut InternalMethodPropertyContext<'_>,
    ) -> JsResult<bool> {
        if matches!(key, PropertyKey::Index(_)) {
            return Ok(false);
        }
        if let PropertyKey::String(name) = key
            && T::HAS_NAMED_PROPERTIES
        {
            if Self::visible_name_present(object, key, name, context)? {
                return Ok(false);
            }
            let current = ordinary_get_own_property(object, key, context)?;
            let extensible = object.__is_extensible__(context)?;
            return Ok(validate_and_apply_property_descriptor(
                Some((object, key)),
                extensible,
                descriptor,
                current,
                context.slot(),
            ));
        }
        ordinary_define_own_property(object, key, descriptor, context)
    }

    fn has_property(
        object: &JsObject,
        key: &PropertyKey,
        context: &mut InternalMethodPropertyContext<'_>,
    ) -> JsResult<bool> {
        let present = match key {
            PropertyKey::Index(index) => {
                Self::source(object)?.index_present(object, index.get(), context)?
            }
            PropertyKey::String(name) if T::HAS_NAMED_PROPERTIES => {
                ordinary_get_own_property(object, key, context)?.is_some()
                    || Self::visible_name_present(object, key, name, context)?
            }
            _ => return ordinary_has_property(object, key, context),
        };
        context
            .slot()
            .attributes
            .insert(SlotAttributes::NOT_CACHEABLE);
        if present {
            return Ok(true);
        }
        match object.__get_prototype_of__(context)? {
            Some(prototype) => prototype.__has_property__(key, context),
            None => Ok(false),
        }
    }

    fn delete(
        object: &JsObject,
        key: &PropertyKey,
        context: &mut InternalMethodPropertyContext<'_>,
    ) -> JsResult<bool> {
        match key {
            PropertyKey::Index(index) => {
                Ok(!Self::source(object)?.index_present(object, index.get(), context)?)
            }
            PropertyKey::String(name) if T::HAS_NAMED_PROPERTIES => {
                if let Some(descriptor) = ordinary_get_own_property(object, key, context)? {
                    if !descriptor.expect_configurable() {
                        return Ok(false);
                    }
                    object.borrow_mut().remove(key);
                    return Ok(true);
                }
                Ok(!Self::visible_name_present(object, key, name, context)?)
            }
            _ => ordinary_delete(object, key, context),
        }
    }

    fn visible_name_present(
        object: &JsObject,
        key: &PropertyKey,
        name: &JsString,
        context: &mut Context,
    ) -> JsResult<bool> {
        Ok(Self::name_is_unmasked(object, key, context)?
            && Self::source(object)?.named_present(object, name, context)?)
    }

    fn name_is_unmasked(
        object: &JsObject,
        key: &PropertyKey,
        context: &mut Context,
    ) -> JsResult<bool> {
        // Only ordinary fields mask names. In particular, another collection's
        // synthesized property is not an ordinary prototype field.
        let mut visited = vec![object.clone()];
        let mut current = object.clone();
        loop {
            if current.borrow().properties.get(key).is_some() {
                return Ok(false);
            }
            let Some(prototype) = current.__get_prototype_of__(context)? else {
                return Ok(true);
            };
            if visited.contains(&prototype) {
                return Ok(false);
            }
            visited.push(prototype.clone());
            current = prototype;
        }
    }

    fn own_property_keys(object: &JsObject, context: &mut Context) -> JsResult<Vec<PropertyKey>> {
        let source = Self::source(object)?;
        let mut indices = source.indexed_keys(object, context)?;
        indices.retain(|index| *index < u32::MAX);
        indices.sort_unstable();
        indices.dedup();
        let names = if T::HAS_NAMED_PROPERTIES {
            source.named_keys(object, context)?
        } else {
            Vec::new()
        };
        let mut keys = Vec::new();
        let capacity = indices
            .len()
            .checked_add(names.len())
            .ok_or_else(|| JsNativeError::range().with_message("Too many collection properties"))?;
        keys.try_reserve(capacity)
            .map_err(|_| JsNativeError::range().with_message("Too many collection properties"))?;
        keys.extend(indices.into_iter().map(PropertyKey::from));
        let mut seen = HashSet::new();
        for name in names {
            let key = PropertyKey::from(name);
            if matches!(key, PropertyKey::String(_))
                && Self::name_is_unmasked(object, &key, context)?
                && seen.insert(key.clone())
            {
                keys.push(key);
            }
        }
        let ordinary = ordinary_own_property_keys(object, context)?;
        keys.try_reserve(ordinary.len())
            .map_err(|_| JsNativeError::range().with_message("Too many collection properties"))?;
        keys.extend(
            ordinary
                .into_iter()
                .filter(|key| !matches!(key, PropertyKey::Index(_))),
        );
        Ok(keys)
    }
}

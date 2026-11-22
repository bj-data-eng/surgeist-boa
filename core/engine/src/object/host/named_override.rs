use std::{borrow::Cow, collections::HashSet};

use boa_gc::{Finalize, Gc, Trace};

use crate::{
    Context, JsNativeError, JsResult, JsString, JsValue, js_string,
    object::{
        JsData, JsObject, JsPrototype,
        internal_methods::{
            InternalMethodPropertyContext, InternalObjectMethods, ORDINARY_INTERNAL_METHODS,
            ordinary_define_own_property, ordinary_delete, ordinary_get_own_property,
            ordinary_own_property_keys, ordinary_set, ordinary_set_with_own_descriptor,
            validate_and_apply_property_descriptor,
        },
        shape::{RootShape, slot::SlotAttributes},
    },
    property::{PropertyDescriptor, PropertyKey},
};

/// Source of enumerable, read-only names for a named-only Web IDL platform object.
///
/// The callbacks receive the actual holder. Numeric property names are passed
/// as strings, and ordinary own properties hide supported names.
pub trait NamedOverrideProperties: Trace + 'static {
    /// Gets a supported value, or absence for an unsupported name.
    fn named_value(
        &self,
        holder: &JsObject,
        name: &JsString,
        context: &mut Context,
    ) -> JsResult<Option<JsValue>>;

    /// Checks for a supported name without projecting its value.
    fn named_present(
        &self,
        holder: &JsObject,
        name: &JsString,
        context: &mut Context,
    ) -> JsResult<bool>;

    /// Gets the current supported names in interface order.
    fn named_keys(&self, holder: &JsObject, context: &mut Context) -> JsResult<Vec<JsString>>;

    /// Checks immutable declaration metadata without running any guest code.
    fn is_unforgeable_name(&self, _name: &JsString) -> bool {
        false
    }
}

/// Creates a named-only, enumerable Web IDL object with `LegacyOverrideBuiltIns`.
#[must_use]
pub fn named_override<T: NamedOverrideProperties>(
    root_shape: &RootShape,
    prototype: JsPrototype,
    data: T,
) -> JsObject {
    let object =
        JsObject::from_proto_and_data_in(root_shape, prototype, NamedOverride(Gc::new(data)));
    object.borrow_mut().properties.preserve_string_key_order();
    object
}

/// Returns this object's named source if it carries the requested native data.
#[must_use]
pub fn named_override_source<T: NamedOverrideProperties>(object: &JsObject) -> Option<Gc<T>> {
    object
        .downcast_ref::<NamedOverride<T>>()
        .map(|data| Gc::clone(&data.0))
}

#[derive(Trace, Finalize)]
struct NamedOverride<T: NamedOverrideProperties>(Gc<T>);

impl<T: NamedOverrideProperties> JsData for NamedOverride<T> {
    fn internal_methods(&self) -> &'static InternalObjectMethods {
        &Self::METHODS
    }
}

impl<T: NamedOverrideProperties> NamedOverride<T> {
    const METHODS: InternalObjectMethods = InternalObjectMethods {
        __get_own_property__: Self::get_own_property,
        named_descriptor_has_effects: true,
        __define_own_property__: Self::define_own_property,
        __set__: Self::set,
        __delete__: Self::delete,
        __own_property_keys__: Self::own_property_keys,
        __prevent_extensions__: |_, _| Ok(false),
        ..ORDINARY_INTERNAL_METHODS
    };

    fn source(object: &JsObject) -> JsResult<Gc<T>> {
        named_override_source(object).ok_or_else(|| {
            JsNativeError::typ()
                .with_message("Expected a native named object")
                .into()
        })
    }

    fn name(key: &PropertyKey) -> Option<Cow<'_, JsString>> {
        match key {
            PropertyKey::String(name) => Some(Cow::Borrowed(name)),
            PropertyKey::Index(index) => Some(Cow::Owned(js_string!(index.get()))),
            PropertyKey::Symbol(_) => None,
        }
    }

    fn get_own_property(
        object: &JsObject,
        key: &PropertyKey,
        context: &mut InternalMethodPropertyContext<'_>,
    ) -> JsResult<Option<PropertyDescriptor>> {
        // A descendant can cache an inherited slot against its own shape.
        // Removing the real property can reveal a native name on this holder.
        context
            .slot()
            .attributes
            .insert(SlotAttributes::NOT_CACHEABLE);
        let ordinary = ordinary_get_own_property(object, key, context)?;
        if ordinary.is_some() {
            return Ok(ordinary);
        }
        let Some(name) = Self::name(key) else {
            return Ok(None);
        };
        if let Some(value) = Self::source(object)?.named_value(object, &name, context)? {
            return Ok(Some(
                PropertyDescriptor::builder()
                    .value(value)
                    .writable(false)
                    .enumerable(true)
                    .configurable(true)
                    .build(),
            ));
        }
        ordinary_get_own_property(object, key, context)
    }

    fn set(
        object: &JsObject,
        key: PropertyKey,
        value: JsValue,
        receiver: JsValue,
        context: &mut InternalMethodPropertyContext<'_>,
    ) -> JsResult<bool> {
        if matches!(key, PropertyKey::Symbol(_)) {
            return ordinary_set(object, key, value, receiver, context);
        }
        let own = ordinary_get_own_property(object, &key, context)?;
        let result = ordinary_set_with_own_descriptor(object, key, value, receiver, own, context);
        // The native name can become supported without changing the object's
        // shape. A cached slot write would bypass DefineOwnProperty.
        context
            .slot()
            .attributes
            .insert(SlotAttributes::NOT_CACHEABLE);
        result
    }

    fn define_own_property(
        object: &JsObject,
        key: &PropertyKey,
        descriptor: PropertyDescriptor,
        context: &mut InternalMethodPropertyContext<'_>,
    ) -> JsResult<bool> {
        let Some(name) = Self::name(key) else {
            return ordinary_define_own_property(object, key, descriptor, context);
        };
        let source = Self::source(object)?;
        if !source.is_unforgeable_name(&name) && source.named_present(object, &name, context)? {
            return Ok(false);
        }
        let current = ordinary_get_own_property(object, key, context)?;
        let extensible = object.__is_extensible__(context)?;
        let result = validate_and_apply_property_descriptor(
            Some((object, key)),
            extensible,
            descriptor,
            current,
            context.slot(),
        );
        context
            .slot()
            .attributes
            .insert(SlotAttributes::NOT_CACHEABLE);
        Ok(result)
    }

    fn delete(
        object: &JsObject,
        key: &PropertyKey,
        context: &mut InternalMethodPropertyContext<'_>,
    ) -> JsResult<bool> {
        let Some(name) = Self::name(key) else {
            return ordinary_delete(object, key, context);
        };
        let mut current = ordinary_get_own_property(object, key, context)?;
        if current.is_none() {
            let present = Self::source(object)?.named_present(object, &name, context)?;
            current = ordinary_get_own_property(object, key, context)?;
            if present && current.is_none() {
                return Ok(false);
            }
        }
        if let Some(descriptor) = current {
            if !descriptor.expect_configurable() {
                return Ok(false);
            }
            object.borrow_mut().remove(key);
        }
        Ok(true)
    }

    fn own_property_keys(object: &JsObject, context: &mut Context) -> JsResult<Vec<PropertyKey>> {
        let names = Self::source(object)?.named_keys(object, context)?;
        let ordinary = ordinary_own_property_keys(object, context)?;
        let too_many_keys = || JsNativeError::range().with_message("Too many named properties");
        let capacity = names
            .len()
            .checked_add(ordinary.len())
            .ok_or_else(too_many_keys)?;
        let mut keys = Vec::new();
        keys.try_reserve(capacity).map_err(|_| too_many_keys())?;
        let mut seen = HashSet::new();
        seen.try_reserve(names.len()).map_err(|_| too_many_keys())?;
        let object = object.borrow();
        for name in names {
            let key = PropertyKey::from(name);
            if !object.properties.contains_key(&key) && seen.insert(key.clone()) {
                keys.push(key);
            }
        }
        // The shape stores numeric-looking expandos alongside other strings.
        // Restore the canonical key representation at the object boundary.
        keys.extend(ordinary.into_iter().map(|key| match key {
            PropertyKey::String(name) => PropertyKey::from(name),
            key => key,
        }));
        Ok(keys)
    }
}

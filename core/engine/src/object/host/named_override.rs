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

/// Source of enumerable names for a named-only Web IDL platform object.
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

/// Adds a named setter and a named deleter that returns `undefined` in Web IDL.
///
/// Both callbacks receive the actual holder. The setter receives the original
/// JavaScript value and owns its interface-specific IDL conversion.
pub trait MutableNamedOverrideProperties: NamedOverrideProperties {
    /// Sets a new or existing named value.
    fn set_named_value(
        &self,
        holder: &JsObject,
        name: &JsString,
        value: &JsValue,
        context: &mut Context,
    ) -> JsResult<()>;

    /// Deletes a visible supported name.
    fn delete_named_value(
        &self,
        holder: &JsObject,
        name: &JsString,
        context: &mut Context,
    ) -> JsResult<()>;
}

/// Creates a read-only, named-only Web IDL object with `LegacyOverrideBuiltIns`.
#[must_use]
pub fn named_override<T: NamedOverrideProperties>(
    root_shape: &RootShape,
    prototype: JsPrototype,
    data: T,
) -> JsObject {
    NamedOverride::allocate(root_shape, prototype, data, None)
}

/// Creates a mutable, named-only Web IDL object with `LegacyOverrideBuiltIns`.
#[must_use]
pub fn mutable_named_override<T: MutableNamedOverrideProperties>(
    root_shape: &RootShape,
    prototype: JsPrototype,
    data: T,
) -> JsObject {
    NamedOverride::allocate(
        root_shape,
        prototype,
        data,
        Some(&NamedOverride::<T>::MUTATIONS),
    )
}

/// Returns this object's named source if it carries the requested native data.
#[must_use]
pub fn named_override_source<T: NamedOverrideProperties>(object: &JsObject) -> Option<Gc<T>> {
    object
        .downcast_ref::<NamedOverride<T>>()
        .map(|data| Gc::clone(&data.source))
}

type NamedSetter<T> = fn(&T, &JsObject, &JsString, &JsValue, &mut Context) -> JsResult<()>;
type NamedDeleter<T> = fn(&T, &JsObject, &JsString, &mut Context) -> JsResult<()>;

struct NamedMutations<T> {
    set: NamedSetter<T>,
    delete: NamedDeleter<T>,
}

impl<T: NamedOverrideProperties> NamedMutations<T> {
    fn invoke_named_setter(
        &self,
        source: &T,
        holder: &JsObject,
        name: &JsString,
        value: &JsValue,
        context: &mut Context,
    ) -> JsResult<bool> {
        // InvokeNamedSetter determines support before converting the value.
        source.named_present(holder, name, context)?;
        (self.set)(source, holder, name, value, context)?;
        Ok(true)
    }
}

#[derive(Trace, Finalize)]
struct NamedOverride<T: NamedOverrideProperties> {
    source: Gc<T>,
    mutations: Option<&'static NamedMutations<T>>,
}

impl<T: NamedOverrideProperties> JsData for NamedOverride<T> {
    fn internal_methods(&self) -> &'static InternalObjectMethods {
        if self.mutations.is_some() {
            &Self::MUTABLE_METHODS
        } else {
            &Self::METHODS
        }
    }
}

impl<T: MutableNamedOverrideProperties> NamedOverride<T> {
    const MUTATIONS: NamedMutations<T> = NamedMutations {
        set: T::set_named_value,
        delete: T::delete_named_value,
    };
}

impl<T: NamedOverrideProperties> NamedOverride<T> {
    const METHODS: InternalObjectMethods = InternalObjectMethods {
        __get_own_property__: Self::get_own_property::<false>,
        named_descriptor_has_effects: true,
        __define_own_property__: Self::define_own_property,
        __set__: Self::set,
        __delete__: Self::delete::<false>,
        __own_property_keys__: Self::own_property_keys,
        __prevent_extensions__: |_, _| Ok(false),
        ..ORDINARY_INTERNAL_METHODS
    };

    const MUTABLE_METHODS: InternalObjectMethods = InternalObjectMethods {
        __get_own_property__: Self::get_own_property::<true>,
        __delete__: Self::delete::<true>,
        ..Self::METHODS
    };

    fn allocate(
        root_shape: &RootShape,
        prototype: JsPrototype,
        source: T,
        mutations: Option<&'static NamedMutations<T>>,
    ) -> JsObject {
        let data = Self {
            source: Gc::new(source),
            mutations,
        };
        let object = JsObject::from_proto_and_data_in(root_shape, prototype, data);
        object.borrow_mut().properties.preserve_string_key_order();
        object
    }

    fn data(object: &JsObject) -> JsResult<Self> {
        let data = object
            .downcast_ref::<Self>()
            .ok_or_else(|| JsNativeError::typ().with_message("Expected a native named object"))?;
        Ok(Self {
            source: Gc::clone(&data.source),
            mutations: data.mutations,
        })
    }

    fn name(key: &PropertyKey) -> Option<Cow<'_, JsString>> {
        match key {
            PropertyKey::String(name) => Some(Cow::Borrowed(name)),
            PropertyKey::Index(index) => Some(Cow::Owned(js_string!(index.get()))),
            PropertyKey::Symbol(_) => None,
        }
    }

    fn get_own_property<const MUTABLE: bool>(
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
        if !MUTABLE {
            let ordinary = ordinary_get_own_property(object, key, context)?;
            if ordinary.is_some() || matches!(key, PropertyKey::Symbol(_)) {
                return Ok(ordinary);
            }
        }
        let Some(name) = Self::name(key) else {
            return ordinary_get_own_property(object, key, context);
        };
        let data = Self::data(object)?;
        if MUTABLE {
            // Check support before observing the current ordinary descriptor.
            let present = data.source.named_present(object, &name, context)?;
            let ordinary = ordinary_get_own_property(object, key, context)?;
            if !present || ordinary.is_some() {
                return Ok(ordinary);
            }
        }
        if let Some(value) = data.source.named_value(object, &name, context)? {
            return Ok(Some(
                PropertyDescriptor::builder()
                    .value(value)
                    .writable(MUTABLE)
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
        if receiver
            .as_object()
            .is_some_and(|receiver| &receiver == object)
            && let Some(name) = Self::name(&key)
        {
            let data = Self::data(object)?;
            if let Some(mutations) = data.mutations {
                context
                    .slot()
                    .attributes
                    .insert(SlotAttributes::NOT_CACHEABLE);
                return mutations.invoke_named_setter(&data.source, object, &name, &value, context);
            }
        }
        let result = if matches!(key, PropertyKey::Symbol(_)) {
            ordinary_set(object, key, value, receiver, context)
        } else {
            let own = ordinary_get_own_property(object, &key, context)?;
            ordinary_set_with_own_descriptor(object, key, value, receiver, own, context)
        };
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
        context
            .slot()
            .attributes
            .insert(SlotAttributes::NOT_CACHEABLE);
        let Some(name) = Self::name(key) else {
            return ordinary_define_own_property(object, key, descriptor, context);
        };
        let data = Self::data(object)?;
        if !data.source.is_unforgeable_name(&name) {
            let present = data.source.named_present(object, &name, context)?;
            if let Some(mutations) = data.mutations {
                if !descriptor.is_data_descriptor() {
                    return Ok(false);
                }
                let undefined = JsValue::undefined();
                let value = descriptor.value().unwrap_or(&undefined);
                return mutations.invoke_named_setter(&data.source, object, &name, value, context);
            }
            if present {
                return Ok(false);
            }
        }
        let current = ordinary_get_own_property(object, key, context)?;
        let extensible = object.__is_extensible__(context)?;
        Ok(validate_and_apply_property_descriptor(
            Some((object, key)),
            extensible,
            descriptor,
            current,
            context.slot(),
        ))
    }

    fn delete<const MUTABLE: bool>(
        object: &JsObject,
        key: &PropertyKey,
        context: &mut InternalMethodPropertyContext<'_>,
    ) -> JsResult<bool> {
        context
            .slot()
            .attributes
            .insert(SlotAttributes::NOT_CACHEABLE);
        let Some(name) = Self::name(key) else {
            return ordinary_delete(object, key, context);
        };
        let mut current = if MUTABLE {
            // Visibility checks support before consulting an ordinary shadow.
            None
        } else {
            ordinary_get_own_property(object, key, context)?
        };
        if current.is_none() {
            let data = Self::data(object)?;
            let present = data.source.named_present(object, &name, context)?;
            current = ordinary_get_own_property(object, key, context)?;
            if present && current.is_none() {
                let Some(mutations) = data.mutations else {
                    return Ok(false);
                };
                (mutations.delete)(&data.source, object, &name, context)?;
                return Ok(true);
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
        let names = Self::data(object)?.source.named_keys(object, context)?;
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

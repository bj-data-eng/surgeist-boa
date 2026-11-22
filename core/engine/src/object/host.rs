//! Native host objects with selected ECMAScript internal methods.

mod named_override;
mod window;
pub use named_override::{
    MutableNamedOverrideProperties, NamedOverrideProperties, mutable_named_override,
    named_override, named_override_source,
};
pub(crate) use window::is_window_proxy;
pub use window::{WindowOrigin, install_window_index_reader, window_proxy, window_proxy_origin};

use std::collections::{HashMap, hash_map::Entry};

use boa_gc::{Finalize, Gc, GcRefCell, Trace};

use crate::{
    Context, JsNativeError, JsResult, JsString, JsSymbol, JsValue,
    context::ContextCleanupGuard,
    error::RuntimeLimitError,
    js_string,
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
        ordinary_has_property, ordinary_own_property_keys, ordinary_set,
        ordinary_set_with_own_descriptor, validate_and_apply_property_descriptor,
    },
    shape::{RootShape, slot::SlotAttributes},
};
#[cfg(feature = "annex-b")]
use super::{
    builtins::JsFunction,
    internal_methods::{CallValue, InternalMethodCallContext},
};

/// Creates an extensible host object with an immutable guest-visible prototype.
///
/// A host can construct a global with this object through
/// [`HostHooks::create_global_object`](crate::context::HostHooks::create_global_object).
#[must_use]
pub fn immutable_prototype(root_shape: &RootShape, prototype: JsPrototype) -> JsObject {
    JsObject::from_proto_and_data_in(root_shape, prototype, ImmutablePrototype)
}

/// Creates the named-properties layer for the associated Window.
///
/// Give the Window an immutable prototype chain with ordinary descriptors and
/// ordinary prototype reads, apart from this named-properties layer. Install
/// the native name reader before exposing the Window to scripts.
#[must_use]
pub fn window_properties(
    root_shape: &RootShape,
    window: JsObject,
    prototype: JsObject,
) -> JsObject {
    let object = JsObject::from_proto_and_data_in(
        root_shape,
        Some(prototype),
        WindowProperties {
            window,
            reader: GcRefCell::default(),
        },
    );
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

/// Installs this layer's live native reader once. The callable receives the
/// exact name as a string and returns `undefined` for an unsupported name.
#[must_use]
pub fn install_window_name_reader(object: &JsObject, reader: JsObject) -> bool {
    let Some(layer) = object.downcast_ref::<WindowProperties>() else {
        return false;
    };
    if !reader.is_callable() {
        return false;
    }
    let mut slot = layer.reader.borrow_mut();
    if slot.is_some() {
        return false;
    }
    *slot = Some(reader);
    true
}

#[derive(Clone, Copy, Trace, Finalize)]
#[boa_gc(empty_trace)]
struct ImmutablePrototype;

impl JsData for ImmutablePrototype {
    fn internal_methods(&self) -> &'static InternalObjectMethods {
        &IMMUTABLE_PROTOTYPE_EXOTIC_INTERNAL_METHODS
    }
}

#[derive(Trace, Finalize)]
struct WindowProperties {
    window: JsObject,
    reader: GcRefCell<Option<JsObject>>,
}

impl JsData for WindowProperties {
    fn internal_methods(&self) -> &'static InternalObjectMethods {
        static METHODS: InternalObjectMethods = InternalObjectMethods {
            __get_own_property__: WindowProperties::get_own_property,
            named_descriptor_has_effects: true,
            __set_prototype_of__: immutable_prototype_exotic_set_prototype_of,
            __define_own_property__: |_, _, _, _| Ok(false),
            __delete__: |_, _, _| Ok(false),
            __prevent_extensions__: |_, _| Ok(false),
            ..ORDINARY_INTERNAL_METHODS
        };
        &METHODS
    }
}

impl WindowProperties {
    fn get_own_property(
        object: &JsObject,
        key: &PropertyKey,
        context: &mut InternalMethodPropertyContext<'_>,
    ) -> JsResult<Option<PropertyDescriptor>> {
        if matches!(key, PropertyKey::Symbol(_)) {
            return ordinary_get_own_property(object, key, context);
        }

        let source = object.downcast_ref::<Self>().and_then(|layer| {
            layer
                .reader
                .borrow()
                .clone()
                .map(|reader| (layer.window.clone(), reader))
        });
        // A named result (or miss) must never be cached in the ordinary shape.
        context
            .slot()
            .attributes
            .insert(SlotAttributes::NOT_CACHEABLE);
        let Some((window, reader)) = source else {
            return Self::ordinary_descriptor(object, key, context);
        };

        // The Window chain is immutable and these ordinary descriptor probes
        // cannot call script. Avoid projecting a value that an own property masks.
        let mut current = Some(window.clone());
        while let Some(candidate) = current {
            if !candidate.is::<Self>() && candidate.borrow().properties.get(key).is_some() {
                return Self::ordinary_descriptor(object, key, context);
            }
            current = candidate.prototype();
        }

        let name = match key {
            PropertyKey::String(name) => name.clone(),
            PropertyKey::Index(index) => JsString::from(index.get().to_string()),
            PropertyKey::Symbol(_) => return Ok(None),
        };
        let value = reader.call(&window.into(), &[name.into()], context)?;
        if value.is_undefined() {
            return Self::ordinary_descriptor(object, key, context);
        }
        Ok(Some(
            PropertyDescriptor::builder()
                .value(value)
                .writable(true)
                .enumerable(false)
                .configurable(true)
                .build(),
        ))
    }

    fn ordinary_descriptor(
        object: &JsObject,
        key: &PropertyKey,
        context: &mut InternalMethodPropertyContext<'_>,
    ) -> JsResult<Option<PropertyDescriptor>> {
        let result = ordinary_get_own_property(object, key, context);
        context
            .slot()
            .attributes
            .insert(SlotAttributes::NOT_CACHEABLE);
        result
    }
}

/// A live source of contiguous, read-only indexed properties and optional names.
///
/// Each callback receives the native object that owns this source. It observes
/// the current collection state and projects values from that state.
pub trait IndexedProperties: Trace + 'static {
    /// Enables non-enumerable legacy named properties, such as `HTMLCollection` names.
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

    /// Runs the interface's named getter after a reentrant visibility check.
    /// The name can have stopped being supported since visibility accepted it.
    /// Override this if the getter differs from the exact lookup or returns
    /// something other than null when the name is missing.
    fn named_getter(
        &self,
        holder: &JsObject,
        name: &JsString,
        context: &mut Context,
    ) -> JsResult<JsValue> {
        Ok(self
            .named_value(holder, name, context)?
            .unwrap_or_else(JsValue::null))
    }
}

/// Creates a read-only legacy indexed object with ordinary prototype methods.
///
/// Guest code can define ordinary string and symbol properties. It cannot
/// create its own index, delete a supported index, or prevent extensions.
/// Ordinary properties hide legacy names.
#[must_use]
pub fn readonly_indexed<T: IndexedProperties>(
    root_shape: &RootShape,
    prototype: JsPrototype,
    source: T,
) -> JsObject {
    JsObject::from_proto_and_data_in(
        root_shape,
        prototype,
        ReadOnlyIndices {
            source: Gc::new(source),
            #[cfg(feature = "annex-b")]
            call: None,
        },
    )
}

/// Creates a legacy indexed object with the Annex B `[[IsHTMLDDA]]` slot.
/// Calls invoke the retained callback with the actual collection as its
/// receiver and the untouched argument list. The collection cannot construct.
#[cfg(feature = "annex-b")]
#[must_use]
pub fn readonly_indexed_with_html_dda<T: IndexedProperties>(
    root_shape: &RootShape,
    prototype: JsPrototype,
    source: T,
    call: JsFunction,
) -> JsObject {
    JsObject::from_proto_and_data_in(
        root_shape,
        prototype,
        ReadOnlyIndices {
            source: Gc::new(source),
            call: Some(call),
        },
    )
}

/// Returns the source if the object itself carries the specified native data.
#[must_use]
pub fn readonly_indexed_source<T: IndexedProperties>(object: &JsObject) -> Option<Gc<T>> {
    object
        .downcast_ref::<ReadOnlyIndices<T>>()
        .map(|data| Gc::clone(&data.source))
}

#[derive(Trace, Finalize)]
struct ReadOnlyIndices<T: IndexedProperties> {
    source: Gc<T>,
    #[cfg(feature = "annex-b")]
    call: Option<JsFunction>,
}

#[derive(Default)]
struct NamedVisibility<'name> {
    unchecked_name: Option<&'name JsString>,
    reached_effectful_descriptor: bool,
}

impl<'name> NamedVisibility<'name> {
    fn lookup(name: &'name JsString) -> Self {
        Self {
            unchecked_name: Some(name),
            reached_effectful_descriptor: false,
        }
    }
}

impl<T: IndexedProperties> JsData for ReadOnlyIndices<T> {
    fn internal_methods(&self) -> &'static InternalObjectMethods {
        #[cfg(feature = "annex-b")]
        if self.call.is_some() {
            return &Self::HTML_DDA_METHODS;
        }
        &Self::METHODS
    }
}

impl<T: IndexedProperties> ReadOnlyIndices<T> {
    const METHODS: InternalObjectMethods = InternalObjectMethods {
        __get_own_property__: Self::get_own_property,
        named_descriptor_has_effects: T::HAS_NAMED_PROPERTIES,
        __define_own_property__: Self::define_own_property,
        __has_property__: Self::has_property,
        __set__: if T::HAS_NAMED_PROPERTIES {
            Self::set
        } else {
            ordinary_set
        },
        __delete__: Self::delete,
        __own_property_keys__: Self::own_property_keys,
        __prevent_extensions__: |_, _| Ok(false),
        ..ORDINARY_INTERNAL_METHODS
    };

    #[cfg(feature = "annex-b")]
    const HTML_DDA_METHODS: InternalObjectMethods = InternalObjectMethods {
        __call__: Self::call,
        has_is_html_dda: true,
        ..Self::METHODS
    };

    #[cfg(feature = "annex-b")]
    fn call(
        object: &JsObject,
        argument_count: usize,
        context: &mut InternalMethodCallContext<'_>,
    ) -> JsResult<CallValue> {
        let callback = object
            .downcast_ref::<Self>()
            .and_then(|data| data.call.clone())
            .ok_or_else(|| JsNativeError::typ().with_message("Expected a callable collection"))?;
        let stack = &mut context.vm.stack;
        stack.calling_convention_set_this(argument_count, object.clone().into());
        stack.calling_convention_set_function(argument_count, callback.clone().into());
        Ok(callback.__call__(argument_count))
    }

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
        let mut ordinary = ordinary_get_own_property(object, key, context)?;
        if let PropertyKey::String(name) = key
            && T::HAS_NAMED_PROPERTIES
            && ordinary.is_none()
        {
            let mut visibility = NamedVisibility::lookup(name);
            let visible = Self::name_is_unmasked(object, key, &mut visibility, context)?;
            if !visible && visibility.reached_effectful_descriptor {
                ordinary = ordinary_get_own_property(object, key, context)?;
            }
            context
                .slot()
                .attributes
                .insert(SlotAttributes::NOT_CACHEABLE);
            if visible {
                let source = Self::source(object)?;
                let value = if visibility.unchecked_name.is_none() {
                    Some(source.named_getter(object, name, context)?)
                } else {
                    source.named_value(object, name, context)?
                };
                return Ok(value.map(|value| Self::descriptor(value, false)));
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

    fn set(
        object: &JsObject,
        key: PropertyKey,
        value: JsValue,
        receiver: JsValue,
        context: &mut InternalMethodPropertyContext<'_>,
    ) -> JsResult<bool> {
        if matches!(key, PropertyKey::String(_)) {
            // Web IDL legacy platform object [[Set]] requests the target
            // descriptor with ignoreNamedProps set to true.
            let own_desc = ordinary_get_own_property(object, &key, context)?;
            let has_own_desc = own_desc.is_some();
            let result =
                ordinary_set_with_own_descriptor(object, key, value, receiver, own_desc, context);
            if !has_own_desc {
                // A prototype setter can install an own property during the
                // write. Do not cache that slot against the collection's shape.
                context
                    .slot()
                    .attributes
                    .insert(SlotAttributes::NOT_CACHEABLE);
            }
            result
        } else {
            ordinary_set(object, key, value, receiver, context)
        }
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
            // Definition checks support directly. Only a real own property
            // allows an ordinary definition over a supported name.
            let has_own = object.borrow().properties.contains_key(key);
            if !has_own && Self::source(object)?.named_present(object, name, context)? {
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
            PropertyKey::String(name) if T::HAS_NAMED_PROPERTIES => {
                Self::visible_name_present(object, key, name, context)?
                    || ordinary_get_own_property(object, key, context)?.is_some()
            }
            // Numeric membership reaches the indexed own descriptor, including
            // any failure from its value provider.
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
                if Self::visible_name_present(object, key, name, context)? {
                    return Ok(false);
                }
                if let Some(descriptor) = ordinary_get_own_property(object, key, context)? {
                    if !descriptor.expect_configurable() {
                        return Ok(false);
                    }
                    object.borrow_mut().remove(key);
                }
                Ok(true)
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
        let mut visibility = NamedVisibility::lookup(name);
        if !Self::name_is_unmasked(object, key, &mut visibility, context)? {
            return Ok(false);
        }
        match visibility.unchecked_name {
            Some(name) => Self::source(object)?.named_present(object, name, context),
            None => Ok(true),
        }
    }

    fn name_is_unmasked(
        object: &JsObject,
        key: &PropertyKey,
        visibility: &mut NamedVisibility<'_>,
        context: &mut Context,
    ) -> JsResult<bool> {
        // An ordinary own field of the collection masks its synthesized name.
        if object.borrow().properties.get(key).is_some() {
            return Ok(false);
        }
        if context
            .active_named_properties
            .iter()
            .any(|(holder, name)| holder == object && name == key)
        {
            return Ok(false);
        }
        // Proxy forwarding and host descriptors can recurse without adding VM
        // frames. Bound the native traversal as well as the normal VM recursion.
        let limit = context.runtime_limits().recursion_limit().min(64);
        if context.active_named_properties.len() >= limit {
            return Err(RuntimeLimitError::Recursion.into());
        }
        context
            .active_named_properties
            .try_reserve(1)
            .map_err(|_| JsNativeError::range().with_message("Too many active named properties"))?;
        context
            .active_named_properties
            .push((object.clone(), key.clone()));
        let mut guard = ContextCleanupGuard::new(context, |context| {
            drop(context.active_named_properties.pop());
        });
        let context = &mut *guard;

        let mut visited = vec![object.clone()];
        let mut current = object.clone();
        while let Some(prototype) = current.__get_prototype_of__(context)? {
            if visited.contains(&prototype) {
                return Ok(false);
            }
            visited.push(prototype.clone());
            if !prototype.is::<WindowProperties>() {
                if prototype.vtable().named_descriptor_has_effects {
                    // This lookup can run guest code before later candidates
                    // are visited. Check support before entering it.
                    if let Some(name) = visibility.unchecked_name.take()
                        && !Self::source(object)?.named_present(object, name, context)?
                    {
                        return Ok(false);
                    }
                    visibility.reached_effectful_descriptor = true;
                }
                let mut descriptor_context = InternalMethodPropertyContext::new(context);
                if prototype
                    .__get_own_property__(key, &mut descriptor_context)?
                    .is_some()
                {
                    return Ok(false);
                }
            }
            current = prototype;
        }
        Ok(true)
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
        keys.extend(indices.iter().copied().map(PropertyKey::from));
        let mut observed_names = HashMap::new();
        observed_names
            .try_reserve(names.len())
            .map_err(|_| JsNativeError::range().with_message("Too many collection properties"))?;
        let mut visibility = NamedVisibility::default();
        for name in names {
            let key = PropertyKey::from(name.clone());
            // A numeric supported name belongs to the named group unless the
            // same key was already emitted as a supported index.
            if let PropertyKey::Index(index) = &key
                && indices.binary_search(&index.get()).is_ok()
            {
                continue;
            }
            let Entry::Vacant(observed) = observed_names.entry(key.clone()) else {
                continue;
            };
            let still_supported = !visibility.reached_effectful_descriptor
                || source.named_present(object, &name, context)?;
            let visible =
                still_supported && Self::name_is_unmasked(object, &key, &mut visibility, context)?;
            observed.insert(visible);
            if visible {
                keys.push(key);
            }
        }
        let ordinary = ordinary_own_property_keys(object, context)?;
        keys.try_reserve(ordinary.len())
            .map_err(|_| JsNativeError::range().with_message("Too many collection properties"))?;
        // A visibility callback can add a real own property after its name
        // was emitted. Keep its first position in the resulting key list.
        keys.extend(ordinary.into_iter().filter(|key| {
            !matches!(key, PropertyKey::Index(_)) && observed_names.get(key) != Some(&true)
        }));
        Ok(keys)
    }
}

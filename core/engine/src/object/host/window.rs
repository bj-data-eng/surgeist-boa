//! WindowProxy internal methods for hosted Window realms.

use boa_gc::{Finalize, Gc, GcRefCell, Trace};

use crate::{
    Context, JsNativeError, JsResult, JsValue,
    object::{
        JsData, JsObject, JsPrototype,
        internal_methods::{
            InternalMethodPropertyContext, InternalObjectMethods, ORDINARY_INTERNAL_METHODS,
            immutable_prototype::immutable_prototype_exotic_set_prototype_of,
            ordinary_define_own_property, ordinary_delete, ordinary_get_own_property,
            ordinary_get_prototype_of, ordinary_own_property_keys, ordinary_set,
        },
        shape::{RootShape, slot::SlotAttributes},
    },
    property::{PropertyDescriptor, PropertyKey},
};

/// Identity of a host-qualified same-origin group of Window realms.
///
/// The host shares a token only after authenticating the native settings of
/// both realms. Guest code cannot observe or change the token.
#[derive(Clone, Debug, Trace, Finalize)]
pub struct WindowOrigin(Gc<()>);

impl Default for WindowOrigin {
    fn default() -> Self {
        Self(Gc::new(()))
    }
}

/// Wraps an ordinary Window global with its guest-visible WindowProxy.
#[must_use]
pub fn window_proxy(root_shape: &RootShape, window: JsObject, origin: WindowOrigin) -> JsObject {
    JsObject::from_proto_and_data_in(
        root_shape,
        None,
        WindowProxy {
            window,
            origin,
            indices: GcRefCell::default(),
        },
    )
}

/// Returns the native origin token for this WindowProxy.
#[must_use]
pub fn window_proxy_origin(object: &JsObject) -> Option<WindowOrigin> {
    object
        .downcast_ref::<WindowProxy>()
        .map(|proxy| proxy.origin.clone())
}

pub(crate) fn is_window_proxy(object: &JsObject) -> bool {
    object.is::<WindowProxy>()
}

/// Installs the native indexed reader for this WindowProxy once.
///
/// It receives no arguments for the count, one numeric argument to retrieve
/// a child WindowProxy or undefined, and an additional `true` argument to
/// check membership without projecting a child.
#[must_use]
pub fn install_window_index_reader(object: &JsObject, reader: JsObject) -> bool {
    let Some(proxy) = object.downcast_ref::<WindowProxy>() else {
        return false;
    };
    let mut slot = proxy.indices.borrow_mut();
    if slot.is_some() || !reader.is_callable() {
        return false;
    }
    *slot = Some(reader);
    true
}

#[derive(Trace, Finalize)]
struct WindowProxy {
    window: JsObject,
    origin: WindowOrigin,
    indices: GcRefCell<Option<JsObject>>,
}

impl JsData for WindowProxy {
    fn internal_methods(&self) -> &'static InternalObjectMethods {
        static METHODS: InternalObjectMethods = InternalObjectMethods {
            __get_prototype_of__: WindowProxy::get_prototype,
            __set_prototype_of__: immutable_prototype_exotic_set_prototype_of,
            __is_extensible__: |_, _| Ok(true),
            __prevent_extensions__: |_, _| Ok(false),
            named_descriptor_has_effects: true,
            __get_own_property__: WindowProxy::get_own_property,
            __define_own_property__: WindowProxy::define_own_property,
            __has_property__: WindowProxy::has_property,
            __set__: WindowProxy::set,
            __delete__: WindowProxy::delete,
            __own_property_keys__: WindowProxy::own_keys,
            ..ORDINARY_INTERNAL_METHODS
        };
        &METHODS
    }
}

impl WindowProxy {
    fn target(object: &JsObject) -> JsResult<(JsObject, WindowOrigin, Option<JsObject>)> {
        let proxy = object.downcast_ref::<Self>().ok_or_else(invalid_source)?;
        let reader = proxy.indices.borrow().clone();
        Ok((proxy.window.clone(), proxy.origin.clone(), reader))
    }

    fn same_origin(origin: &WindowOrigin, context: &Context) -> bool {
        context
            .realm()
            .global_this()
            .downcast_ref::<Self>()
            .is_some_and(|caller| Gc::ptr_eq(&caller.origin.0, &origin.0))
    }

    fn require_origin(origin: &WindowOrigin, context: &Context) -> JsResult<()> {
        if Self::same_origin(origin, context) {
            Ok(())
        } else {
            Err(JsNativeError::typ()
                .with_message("Window access requires a same-origin Window realm")
                .into())
        }
    }

    fn get_prototype(object: &JsObject, context: &mut Context) -> JsResult<JsPrototype> {
        let (window, origin, _) = Self::target(object)?;
        if Self::same_origin(&origin, context) {
            ordinary_get_prototype_of(&window, context)
        } else {
            Ok(None)
        }
    }

    fn get_own_property(
        object: &JsObject,
        key: &PropertyKey,
        context: &mut InternalMethodPropertyContext<'_>,
    ) -> JsResult<Option<PropertyDescriptor>> {
        let (window, origin, reader) = Self::target(object)?;
        let result = if let PropertyKey::Index(index) = key {
            let value = read(object, reader, &[index.get().into()], context)?;
            if value.is_undefined() {
                Self::require_origin(&origin, context)?;
                None
            } else {
                if !value.as_object().is_some_and(|child| child.is::<Self>()) {
                    return Err(invalid_source());
                }
                Some(
                    PropertyDescriptor::builder()
                        .value(value)
                        .writable(false)
                        .enumerable(true)
                        .configurable(true)
                        .build(),
                )
            }
        } else {
            Self::require_origin(&origin, context)?;
            ordinary_get_own_property(&window, key, context)?
        };
        context
            .slot()
            .attributes
            .insert(SlotAttributes::NOT_CACHEABLE);
        Ok(result)
    }

    fn define_own_property(
        object: &JsObject,
        key: &PropertyKey,
        descriptor: PropertyDescriptor,
        context: &mut InternalMethodPropertyContext<'_>,
    ) -> JsResult<bool> {
        let (window, origin, _) = Self::target(object)?;
        Self::require_origin(&origin, context)?;
        if matches!(key, PropertyKey::Index(_)) {
            return Ok(false);
        }
        let result = ordinary_define_own_property(&window, key, descriptor, context);
        context
            .slot()
            .attributes
            .insert(SlotAttributes::NOT_CACHEABLE);
        result
    }

    fn has_property(
        object: &JsObject,
        key: &PropertyKey,
        context: &mut InternalMethodPropertyContext<'_>,
    ) -> JsResult<bool> {
        if let PropertyKey::Index(index) = key {
            let (_, origin, reader) = Self::target(object)?;
            if index_present(object, reader, index.get(), context)? {
                context
                    .slot()
                    .attributes
                    .insert(SlotAttributes::NOT_CACHEABLE);
                return Ok(true);
            }
            Self::require_origin(&origin, context)?;
            let result = object
                .__get_prototype_of__(context)?
                .map_or(Ok(false), |prototype| {
                    prototype.__has_property__(key, context)
                });
            context
                .slot()
                .attributes
                .insert(SlotAttributes::NOT_CACHEABLE);
            return result;
        }
        super::ordinary_has_property(object, key, context)
    }

    fn set(
        object: &JsObject,
        key: PropertyKey,
        value: JsValue,
        receiver: JsValue,
        context: &mut InternalMethodPropertyContext<'_>,
    ) -> JsResult<bool> {
        let (window, origin, _) = Self::target(object)?;
        Self::require_origin(&origin, context)?;
        if matches!(key, PropertyKey::Index(_)) {
            return Ok(false);
        }
        let result = ordinary_set(&window, key, value, receiver, context);
        context
            .slot()
            .attributes
            .insert(SlotAttributes::NOT_CACHEABLE);
        result
    }

    fn delete(
        object: &JsObject,
        key: &PropertyKey,
        context: &mut InternalMethodPropertyContext<'_>,
    ) -> JsResult<bool> {
        let (window, origin, reader) = Self::target(object)?;
        Self::require_origin(&origin, context)?;
        if let PropertyKey::Index(index) = key {
            return Ok(!index_present(object, reader, index.get(), context)?);
        }
        ordinary_delete(&window, key, context)
    }

    fn own_keys(object: &JsObject, context: &mut Context) -> JsResult<Vec<PropertyKey>> {
        let (window, origin, reader) = Self::target(object)?;
        let count = if reader.is_some() {
            let count = read(object, reader, &[], context)?
                .as_number()
                .ok_or_else(invalid_source)?;
            if !(0.0..=f64::from(u32::MAX)).contains(&count) || count.fract() != 0.0 {
                return Err(invalid_source());
            }
            count as u32
        } else {
            0
        };
        Self::require_origin(&origin, context)?;
        let ordinary = ordinary_own_property_keys(&window, context)?;
        let capacity = (count as usize)
            .checked_add(ordinary.len())
            .ok_or_else(too_many_keys)?;
        let mut keys = Vec::new();
        keys.try_reserve(capacity).map_err(|_| too_many_keys())?;
        keys.extend((0..count).map(PropertyKey::from));
        keys.extend(ordinary);
        Ok(keys)
    }
}

fn read(
    proxy: &JsObject,
    reader: Option<JsObject>,
    arguments: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    reader.map_or(Ok(JsValue::undefined()), |reader| {
        reader.call(&proxy.clone().into(), arguments, context)
    })
}

fn index_present(
    proxy: &JsObject,
    reader: Option<JsObject>,
    index: u32,
    context: &mut Context,
) -> JsResult<bool> {
    if reader.is_none() {
        return Ok(false);
    }
    read(proxy, reader, &[index.into(), true.into()], context)?
        .as_boolean()
        .ok_or_else(invalid_source)
}

fn invalid_source() -> crate::JsError {
    JsNativeError::typ()
        .with_message("Invalid Window observation")
        .into()
}

fn too_many_keys() -> crate::JsError {
    JsNativeError::range()
        .with_message("Too many Window properties")
        .into()
}

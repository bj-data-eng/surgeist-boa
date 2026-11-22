//! Boa's implementation of ECMAScript's `Realm Records`
//!
//! Conceptually, a realm consists of a set of intrinsic objects, an ECMAScript global environment,
//! all of the ECMAScript code that is loaded within the scope of that global environment,
//! and other associated state and resources.
//!
//! A realm is represented in this implementation as a Realm struct with the fields specified from the spec.

use std::{any::TypeId, fmt};

use crate::{
    Context, HostDefined, JsNativeError, JsObject, JsResult, JsString,
    class::Class,
    context::{
        HostHooks,
        intrinsics::{Intrinsics, StandardConstructor},
    },
    environments::DeclarativeEnvironment,
    module::Module,
    object::shape::RootShape,
};
use boa_ast::scope::Scope;
use boa_engine::JsValue;
use boa_engine::property::{Attribute, PropertyDescriptor, PropertyKey};
use boa_gc::{Finalize, Gc, GcRef, GcRefCell, GcRefMut, Trace, WeakGc};
use rustc_hash::FxHashMap;

// Intrinsics are allocated before the realm record exists. This cell connects
// those objects to their realm once, and is traced so the cycle can be collected.
#[derive(Clone, Trace, Finalize)]
pub(crate) struct RealmOrigin(Gc<GcRefCell<Option<Realm>>>);

impl fmt::Debug for RealmOrigin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RealmOrigin").finish_non_exhaustive()
    }
}

impl RealmOrigin {
    fn new() -> Self {
        Self(Gc::new(GcRefCell::default()))
    }

    fn bind(self, realm: Realm) -> JsResult<()> {
        let mut slot = self.0.try_borrow_mut().map_err(|_| {
            JsNativeError::typ().with_message("failed to bind the allocation realm")
        })?;
        if slot.is_some() {
            return Err(JsNativeError::typ()
                .with_message("allocation realm is already bound")
                .into());
        }
        *slot = Some(realm);
        Ok(())
    }

    pub(crate) fn realm(&self) -> Option<Realm> {
        self.0.try_borrow().ok()?.clone()
    }
}

/// Representation of a Realm.
///
/// In the specification these are called Realm Records.
#[derive(Clone, Trace, Finalize)]
pub struct Realm {
    inner: Gc<Inner>,
}

impl Eq for Realm {}

impl PartialEq for Realm {
    fn eq(&self, other: &Self) -> bool {
        Gc::ptr_eq(&self.inner, &other.inner)
    }
}

impl fmt::Debug for Realm {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Realm")
            .field("intrinsics", &self.inner.intrinsics)
            .field("environment", &self.inner.environment)
            .field("global_object", &self.inner.global_object)
            .field("global_this", &self.inner.global_this)
            .finish()
    }
}

#[derive(Trace, Finalize)]
struct Inner {
    intrinsics: Intrinsics,

    /// The global declarative environment of this realm.
    environment: Gc<DeclarativeEnvironment>,

    /// The global scope of this realm.
    /// This is directly related to the global declarative environment.
    // Safety: Nothing in `Scope` needs tracing.
    #[unsafe_ignore_trace]
    scope: Scope,

    global_object: JsObject,
    global_this: JsObject,
    template_map: GcRefCell<FxHashMap<u64, JsObject>>,
    loaded_modules: GcRefCell<FxHashMap<JsString, Module>>,
    host_classes: GcRefCell<FxHashMap<TypeId, StandardConstructor>>,

    host_defined: GcRefCell<HostDefined>,
}

/// A weak reference to a realm that does not keep its environments alive.
#[derive(Clone, Trace, Finalize)]
pub struct WeakRealm {
    inner: WeakGc<Inner>,
}

impl fmt::Debug for WeakRealm {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WeakRealm").finish_non_exhaustive()
    }
}

impl WeakRealm {
    /// Returns a strong reference while the realm is live.
    #[must_use]
    pub fn upgrade(&self) -> Option<Realm> {
        self.inner.upgrade().map(|inner| Realm { inner })
    }
}

impl Realm {
    /// Returns a weak reference that does not keep this realm alive.
    #[must_use]
    pub fn downgrade(&self) -> WeakRealm {
        WeakRealm {
            inner: WeakGc::new(&self.inner),
        }
    }

    /// Create a new [`Realm`].
    #[inline]
    pub fn create(hooks: &dyn HostHooks, root_shape: &RootShape) -> JsResult<Self> {
        let origin = RealmOrigin::new();
        let root_shape = root_shape.with_origin(origin.clone());
        let intrinsics = Intrinsics::uninit(&root_shape).ok_or_else(|| {
            JsNativeError::typ().with_message("failed to create the realm intrinsics")
        })?;

        let global_object = hooks.create_global_object(&intrinsics);
        let global_this = hooks
            .create_global_this(&intrinsics)
            .unwrap_or_else(|| global_object.clone());
        let environment = Gc::new(DeclarativeEnvironment::global());
        let scope = Scope::new_global();

        let realm = Self {
            inner: Gc::new(Inner {
                intrinsics,
                environment,
                scope,
                global_object,
                global_this,
                template_map: GcRefCell::default(),
                loaded_modules: GcRefCell::default(),
                host_classes: GcRefCell::default(),
                host_defined: GcRefCell::default(),
            }),
        };

        origin.bind(realm.clone())?;
        realm.initialize();

        Ok(realm)
    }

    /// Gets the intrinsics of this `Realm`.
    #[inline]
    #[must_use]
    pub fn intrinsics(&self) -> &Intrinsics {
        &self.inner.intrinsics
    }

    /// Returns an immutable reference to the [`ECMAScript specification`][spec] defined
    /// [`\[\[\HostDefined]\]`][`HostDefined`] field of the [`Realm`].
    ///
    /// [spec]: https://tc39.es/ecma262/#table-realm-record-fields
    ///
    /// # Panics
    ///
    /// Panics if [`HostDefined`] field is mutably borrowed.
    #[inline]
    #[must_use]
    pub fn host_defined(&self) -> GcRef<'_, HostDefined> {
        self.inner.host_defined.borrow()
    }

    /// Returns a mutable reference to [`ECMAScript specification`][spec] defined
    /// [`\[\[\HostDefined]\]`][`HostDefined`] field of the [`Realm`].
    ///
    /// [spec]: https://tc39.es/ecma262/#table-realm-record-fields
    ///
    /// # Panics
    ///
    /// Panics if [`HostDefined`] field is borrowed.
    #[inline]
    #[must_use]
    pub fn host_defined_mut(&self) -> GcRefMut<'_, HostDefined> {
        self.inner.host_defined.borrow_mut()
    }

    /// Checks if this `Realm` has the class `C` registered into its class map.
    #[must_use]
    pub fn has_class<C: Class>(&self) -> bool {
        self.inner
            .host_classes
            .borrow()
            .contains_key(&TypeId::of::<C>())
    }

    /// Gets the constructor and prototype of the class `C` if it is registered in the class map.
    #[must_use]
    pub fn get_class<C: Class>(&self) -> Option<StandardConstructor> {
        self.inner
            .host_classes
            .borrow()
            .get(&TypeId::of::<C>())
            .cloned()
    }

    pub(crate) fn environment(&self) -> &Gc<DeclarativeEnvironment> {
        &self.inner.environment
    }

    /// Returns the scope of this realm.
    #[must_use]
    pub fn scope(&self) -> &Scope {
        &self.inner.scope
    }

    pub(crate) fn global_object(&self) -> &JsObject {
        &self.inner.global_object
    }

    pub(crate) fn global_this(&self) -> &JsObject {
        &self.inner.global_this
    }

    pub(crate) fn loaded_modules(&self) -> &GcRefCell<FxHashMap<JsString, Module>> {
        &self.inner.loaded_modules
    }

    /// Resizes the number of bindings on the global environment.
    pub(crate) fn resize_global_env(&self) {
        let binding_number = self.scope().num_bindings();
        let env = self
            .environment()
            .kind()
            .as_global()
            .expect("Realm should only store global environments");
        let mut bindings = env.bindings().borrow_mut();

        if bindings.len() < binding_number as usize {
            bindings.resize(binding_number as usize, None);
        }
    }

    pub(crate) fn push_template(&self, site: u64, template: JsObject) {
        self.inner.template_map.borrow_mut().insert(site, template);
    }

    pub(crate) fn lookup_template(&self, site: u64) -> Option<JsObject> {
        self.inner.template_map.borrow().get(&site).cloned()
    }

    /// Register a property on the global object of this realm.
    ///
    /// It will return an error if the property is already defined.
    pub fn register_property<K, V>(
        &self,
        key: K,
        value: V,
        attribute: Attribute,
        context: &mut Context,
    ) -> JsResult<()>
    where
        K: Into<PropertyKey>,
        V: Into<JsValue>,
    {
        self.global_object().define_property_or_throw(
            key,
            PropertyDescriptor::builder()
                .value(value)
                .writable(attribute.writable())
                .enumerable(attribute.enumerable())
                .configurable(attribute.configurable()),
            context,
        )?;
        Ok(())
    }

    /// Register a class `C` in this realm.
    pub fn register_class<C: Class>(&self, spec: StandardConstructor) {
        self.inner
            .host_classes
            .borrow_mut()
            .insert(TypeId::of::<C>(), spec);
    }

    /// Unregister a class `C` in this realm.
    #[must_use]
    pub fn unregister_class<C: Class>(&self) -> Option<StandardConstructor> {
        self.inner
            .host_classes
            .borrow_mut()
            .remove(&TypeId::of::<C>())
    }

    pub(crate) fn addr(&self) -> *const () {
        let ptr: *const _ = &raw const *self.inner;
        ptr.cast()
    }
}

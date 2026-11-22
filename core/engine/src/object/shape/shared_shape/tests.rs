use crate::{JsObject, JsSymbol, object::shape::slot::SlotAttributes, property::PropertyKey};

use super::{SharedShape, TransitionKey, WeakSharedShape};

#[test]
fn test_prune_property_on_counter_limit() {
    let shape = SharedShape::root();

    for i in 0..255 {
        assert_eq!(
            shape.forward_transitions().property_transitions_count(),
            (i, i as u8)
        );

        shape.insert_property_transition(TransitionKey {
            property_key: PropertyKey::Symbol(JsSymbol::new(None).unwrap()),
            attributes: SlotAttributes::all(),
        });
    }

    assert_eq!(
        shape.forward_transitions().property_transitions_count(),
        (255, 255)
    );

    boa_gc::force_collect();

    {
        shape.insert_property_transition(TransitionKey {
            property_key: PropertyKey::Symbol(JsSymbol::new(None).unwrap()),
            attributes: SlotAttributes::all(),
        });
    }

    assert_eq!(
        shape.forward_transitions().property_transitions_count(),
        (1, 0)
    );

    {
        shape.insert_property_transition(TransitionKey {
            property_key: PropertyKey::Symbol(JsSymbol::new(None).unwrap()),
            attributes: SlotAttributes::all(),
        });
    }

    assert_eq!(
        shape.forward_transitions().property_transitions_count(),
        (2, 1)
    );

    boa_gc::force_collect();

    assert_eq!(
        shape.forward_transitions().property_transitions_count(),
        (2, 1)
    );
}

#[test]
fn test_prune_prototype_on_counter_limit() {
    let shape = SharedShape::root();
    // Keep identities distinct while allowing every transitioned shape to die.
    let prototypes: Vec<_> = (0..257).map(|_| JsObject::with_null_proto()).collect();

    assert_eq!(
        shape.forward_transitions().prototype_transitions_count(),
        (0, 0)
    );

    for (i, prototype) in prototypes[..255].iter().enumerate() {
        assert_eq!(
            shape.forward_transitions().prototype_transitions_count(),
            (i, i as u8)
        );

        shape.change_prototype_transition(Some(prototype.clone()));
    }

    boa_gc::force_collect();

    assert_eq!(
        shape.forward_transitions().prototype_transitions_count(),
        (255, 255)
    );

    {
        shape.change_prototype_transition(Some(prototypes[255].clone()));
    }

    assert_eq!(
        shape.forward_transitions().prototype_transitions_count(),
        (1, 0)
    );

    {
        shape.change_prototype_transition(Some(prototypes[256].clone()));
    }

    assert_eq!(
        shape.forward_transitions().prototype_transitions_count(),
        (2, 1)
    );

    boa_gc::force_collect();

    assert_eq!(
        shape.forward_transitions().prototype_transitions_count(),
        (2, 1)
    );
}

#[test]
fn prototype_transition_reuses_live_shape_without_rooting_dead_prototype() {
    let root = SharedShape::root();
    let prototype = JsObject::with_null_proto();
    let weak_prototype = prototype.downgrade();

    let first = root.change_prototype_transition(Some(prototype.clone()));
    let second = root.change_prototype_transition(Some(prototype.clone()));
    assert_eq!(first.to_addr_usize(), second.to_addr_usize());

    let different = root.change_prototype_transition(Some(JsObject::with_null_proto()));
    assert_ne!(different.to_addr_usize(), first.to_addr_usize());
    let null = root.change_prototype_transition(None);
    assert_ne!(null.to_addr_usize(), first.to_addr_usize());
    assert_eq!(
        root.change_prototype_transition(None).to_addr_usize(),
        null.to_addr_usize()
    );

    drop(prototype);
    drop(first);
    boa_gc::force_collect();
    assert!(
        weak_prototype.upgrade().is_some(),
        "the live shape owns its prototype"
    );

    let weak_shape = WeakSharedShape::from(&second);
    drop(second);
    boa_gc::force_collect();
    assert!(
        weak_shape.upgrade().is_none(),
        "the cache's transition is weak"
    );
    assert!(
        weak_prototype.upgrade().is_none(),
        "the cache key is non-owning"
    );

    assert_eq!(
        root.change_prototype_transition(None).to_addr_usize(),
        null.to_addr_usize()
    );
}

use boa_macros::{Finalize, Trace};

use super::SharedShape;
use crate::realm::RealmOrigin;

/// This is a wrapper around [`SharedShape`] that ensures it's root shape.
///
/// Represent the root shape that [`SharedShape`] transitions start from.
#[derive(Debug, Clone, Trace, Finalize)]
pub struct RootShape {
    shape: SharedShape,
    origin: Option<RealmOrigin>,
}

impl Default for RootShape {
    #[inline]
    fn default() -> Self {
        Self {
            shape: SharedShape::root(),
            origin: None,
        }
    }
}

impl RootShape {
    pub(crate) fn with_origin(&self, origin: RealmOrigin) -> Self {
        Self {
            shape: self.shape.clone(),
            origin: Some(origin),
        }
    }

    pub(crate) fn origin(&self) -> Option<RealmOrigin> {
        self.origin.clone()
    }

    /// Gets the inner [`SharedShape`].
    #[must_use]
    pub const fn shape(&self) -> &SharedShape {
        &self.shape
    }
}

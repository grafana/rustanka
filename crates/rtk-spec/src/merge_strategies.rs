//! Strategies for merging collections.

#[doc(inline)]
pub use k8s_openapi::merge_strategies::*;

pub mod hashmap {
    //! Strategies for merging [`HashMap`]s.
    //! 
    //! Based on the btree implementation in k8s-openapi
    
    use std::collections::hash_map::Entry;
    use std::hash::{ BuildHasher, Hash };
    // used in doc comment above
    #[allow(unused_imports)]
    use std::collections::HashMap;

    use crate::merge_strategies::hashmap::__private::AsOptMap;

    mod __private {
        use std::collections::HashMap;
        use std::hash::BuildHasher;

        pub trait AsOptMap<S> {
            type Key;
            type Value;

            fn set_if_some(&mut self, new: Self);
            fn as_mut_opt(&mut self) -> Option<&mut HashMap<Self::Key, Self::Value, S>>;
            fn into_opt(self) -> Option<HashMap<Self::Key, Self::Value, S>>;
        }

        impl<K, V, S> AsOptMap<S> for HashMap<K, V, S>
        where
            S: BuildHasher,
        {
            type Key = K;
            type Value = V;

            fn set_if_some(&mut self, new: Self) {
                *self = new;
            }

            fn as_mut_opt(&mut self) -> Option<&mut HashMap<K, V, S>> {
                Some(self)
            }

            fn into_opt(self) -> Option<Self> {
                Some(self)
            }
        }

        impl<K, V, S> AsOptMap<S> for Option<HashMap<K, V, S>>
        where
            S: BuildHasher,
        {
            type Key = K;
            type Value = V;

            fn set_if_some(&mut self, new: Self) {
                if new.is_some() {
                    *self = new;
                }
            }

            fn as_mut_opt(&mut self) -> Option<&mut HashMap<K, V, S>> {
                self.as_mut()
            }

            fn into_opt(self) -> Self {
                self
            }
        }
    }

    /// The whole map is treated as one scalar value, and will be replaced with the new (non-[`None`]) value.
    pub fn atomic<M, S>(current: &mut M, new: M) where M: AsOptMap<S> {
        current.set_if_some(new);
    }

    /// Each value will be merged separately.
    pub fn granular<M, S, F>(current: &mut M, new: M, merge_value: F)
    where
        M: AsOptMap<S>,
        M::Key: Hash + Ord,
        S: BuildHasher,
        F: Fn(&mut M::Value, M::Value),
    {
        if let Some(current) = current.as_mut_opt() {
            for (k, new_v) in new.into_opt().into_iter().flatten() {
                match current.entry(k) {
                    Entry::Vacant(entry) => { entry.insert(new_v); }
                    Entry::Occupied(entry) => merge_value(entry.into_mut(), new_v),
                }
            }
        }
        else {
            current.set_if_some(new);
        }
    }

}

use std::{
    any::{Any, TypeId},
    collections::HashMap,
    sync::Arc,
};

/// A heterogeneous map keyed by type. Values are stored as `Arc<T>` so that
/// cloning the map (or sharing it across handlers) is cheap b/c no deep copies.
#[derive(Default)]
pub struct TypeMap {
    map: HashMap<TypeId, Arc<dyn Any + Send + Sync>>,
}

impl TypeMap {
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert a value. Overwrites any previously registered value of the same type.
    pub fn insert<T: Send + Sync + 'static>(&mut self, value: T) {
        self.map.insert(TypeId::of::<T>(), Arc::new(value));
    }

    /// Retrieve a value by type. Returns `None` if nothing has been registered for `T`.
    pub fn get<T: Send + Sync + 'static>(&self) -> Option<Arc<T>> {
        self.map
            .get(&TypeId::of::<T>())
            .and_then(|arc| arc.clone().downcast::<T>().ok())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insert_and_get() {
        let mut map = TypeMap::new();
        map.insert(42u32);
        assert_eq!(*map.get::<u32>().unwrap(), 42);
    }

    #[test]
    fn get_missing_returns_none() {
        let map = TypeMap::new();
        assert!(map.get::<u32>().is_none());
    }

    #[test]
    fn types_are_independent() {
        let mut map = TypeMap::new();
        map.insert(1u32);
        map.insert("hello");
        assert_eq!(*map.get::<u32>().unwrap(), 1);
        assert_eq!(*map.get::<&str>().unwrap(), "hello");
    }

    #[test]
    fn insert_overwrites() {
        let mut map = TypeMap::new();
        map.insert(1u32);
        map.insert(2u32);
        assert_eq!(*map.get::<u32>().unwrap(), 2);
    }

    #[test]
    fn arc_is_shared() {
        let mut map = TypeMap::new();
        map.insert(42u32);
        let a = map.get::<u32>().unwrap();
        let b = map.get::<u32>().unwrap();
        assert!(Arc::ptr_eq(&a, &b));
    }
}

//! Resource table: maps small integer ids (`rid`s) handed to JS onto Rust
//! objects (files, sockets, listeners, ...). JS only ever sees the number.

use std::any::Any;
use std::collections::BTreeMap;
use std::rc::Rc;

/// Anything that can live in the resource table.
pub trait Resource: Any {
    /// Short type name, shown by `harbor.resources()`.
    fn name(&self) -> &'static str;
    /// Called when the resource is removed via `close`. Default: drop.
    fn close(self: Rc<Self>) {}
}

pub type ResourceId = u32;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResourceError {
    BadResource(ResourceId),
    WrongType {
        rid: ResourceId,
        expected: &'static str,
        actual: &'static str,
    },
}

impl std::fmt::Display for ResourceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ResourceError::BadResource(rid) => write!(f, "Bad resource ID {rid}"),
            ResourceError::WrongType {
                rid,
                expected,
                actual,
            } => write!(f, "Resource {rid} is a {actual}, expected {expected}"),
        }
    }
}

#[derive(Default)]
pub struct ResourceTable {
    map: BTreeMap<ResourceId, Rc<dyn Resource>>,
    next_rid: ResourceId,
}

impl ResourceTable {
    pub fn new() -> Self {
        // rids 0,1,2 are reserved for stdio, mirroring file descriptors.
        ResourceTable {
            map: BTreeMap::new(),
            next_rid: 3,
        }
    }

    pub fn add<T: Resource>(&mut self, resource: T) -> ResourceId {
        self.add_rc(Rc::new(resource))
    }

    pub fn add_rc<T: Resource>(&mut self, resource: Rc<T>) -> ResourceId {
        let rid = self.next_rid;
        self.next_rid = self
            .next_rid
            .checked_add(1)
            .expect("resource ids exhausted");
        self.map.insert(rid, resource);
        rid
    }

    pub fn has(&self, rid: ResourceId) -> bool {
        self.map.contains_key(&rid)
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    /// Typed lookup. Returns a clone of the `Rc`, so the caller may hold it
    /// across `.await` points without borrowing the table.
    pub fn get<T: Resource>(&self, rid: ResourceId) -> Result<Rc<T>, ResourceError> {
        let r = self.map.get(&rid).ok_or(ResourceError::BadResource(rid))?;
        let actual = r.name();
        let any: Rc<dyn Any> = r.clone();
        any.downcast::<T>().map_err(|_| ResourceError::WrongType {
            rid,
            expected: std::any::type_name::<T>()
                .rsplit("::")
                .next()
                .unwrap_or("?"),
            actual,
        })
    }

    /// Remove and close a resource.
    pub fn close(&mut self, rid: ResourceId) -> Result<(), ResourceError> {
        let r = self
            .map
            .remove(&rid)
            .ok_or(ResourceError::BadResource(rid))?;
        r.close();
        Ok(())
    }

    /// `(rid, name)` pairs in rid order.
    pub fn entries(&self) -> Vec<(ResourceId, &'static str)> {
        self.map.iter().map(|(k, v)| (*k, v.name())).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    struct Counter(Cell<u32>);
    impl Resource for Counter {
        fn name(&self) -> &'static str {
            "counter"
        }
    }

    struct Closer(Rc<Cell<bool>>);
    impl Resource for Closer {
        fn name(&self) -> &'static str {
            "closer"
        }
        fn close(self: Rc<Self>) {
            self.0.set(true);
        }
    }

    #[test]
    fn add_get_close() {
        let mut t = ResourceTable::new();
        let a = t.add(Counter(Cell::new(1)));
        let b = t.add(Counter(Cell::new(2)));
        assert_eq!((a, b), (3, 4));
        assert_eq!(t.get::<Counter>(b).unwrap().0.get(), 2);
        t.get::<Counter>(a).unwrap().0.set(10);
        assert_eq!(t.get::<Counter>(a).unwrap().0.get(), 10);
        assert_eq!(t.len(), 2);
        t.close(a).unwrap();
        assert_eq!(
            t.get::<Counter>(a).err(),
            Some(ResourceError::BadResource(a))
        );
        assert_eq!(t.close(a), Err(ResourceError::BadResource(a)));
        assert_eq!(t.entries(), vec![(4, "counter")]);
    }

    #[test]
    fn rids_are_never_reused() {
        let mut t = ResourceTable::new();
        let a = t.add(Counter(Cell::new(0)));
        t.close(a).unwrap();
        let b = t.add(Counter(Cell::new(0)));
        assert_ne!(a, b);
    }

    #[test]
    fn wrong_type_is_an_error() {
        let mut t = ResourceTable::new();
        let flag = Rc::new(Cell::new(false));
        let rid = t.add(Closer(flag.clone()));
        match t.get::<Counter>(rid) {
            Err(ResourceError::WrongType { actual, .. }) => assert_eq!(actual, "closer"),
            _ => panic!("expected WrongType"),
        }
        t.close(rid).unwrap();
        assert!(flag.get(), "close hook ran");
    }

    #[test]
    fn held_rc_outlives_close() {
        let mut t = ResourceTable::new();
        let rid = t.add(Counter(Cell::new(7)));
        let held = t.get::<Counter>(rid).unwrap();
        t.close(rid).unwrap();
        // an in-flight op keeps the object alive until it finishes
        assert_eq!(held.0.get(), 7);
        assert!(!t.has(rid));
    }
}

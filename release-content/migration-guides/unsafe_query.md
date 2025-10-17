---
title: "`UnsafeQuery`"
pull_requests: []
---

The safety requirements of `Query` have been strengthened in order to make it easier to reason about safety in the engine.
It is now the case that a `Query` must *always* have all the access to components that are described in its state.
This was previously possible to violate using the `Query::get_unchecked` method and the various `Query::iter_xxx_unsafe` methods
along with `Query::as_readonly` to create a read-only query while there was a live mutable borrow of a component.

Those methods have been deprecated in favor of a new `UnsafeQuery` type.
This type exposes `unsafe` versions of all the methods on `Query`,
and should be used when one needs a query with incomplete access.
It can be obtained using `Query::as_unsafe_query` or `Query::into_unsafe_query`,
which will ensure that the original query is not available until all `unsafe` operations are finished.

```rust
fn system(query: Query<&mut T>, entity1: Entity, entity2: Entity) {
    assert_ne!(entity1, entity2);
    // SAFETY: entity1 and entity2 are different entities,
    // so it's sound to access them both at once.

    // 0.17
    let mut t_1: Mut<T> = unsafe { unsafe_query.get_unchecked(entity1) }.unwrap();
    let mut t_2: Mut<T> = unsafe { unsafe_query.get_unchecked(entity2) }.unwrap();

    // 0.18
    let unsafe_query = query.as_unsafe_query();
    let mut t_1: Mut<T> = unsafe { unsafe_query.get(entity1) }.unwrap();
    let mut t_2: Mut<T> = unsafe { unsafe_query.get(entity2) }.unwrap();
}
```

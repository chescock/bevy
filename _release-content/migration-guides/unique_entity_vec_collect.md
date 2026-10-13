---
title: "`UniqueEntityEquivalentVec` no longer implements `FromIterator`"
pull_requests: []
---

The `FromIterator` impls for `UniqueEntityEquivalentVec` and `Box<UniqueEntityEquivalentSlice>`,
and the `Extend` impls for `UniqueEntityEquivalentVec`, have been removed.

Calls to `collect()` may now error with

```
error[E0277]: a value of type `UniqueEntityEquivalentVec<entity::Entity>` cannot be built from an iterator over elements of type `entity::Entity`
help: the trait `FromIterator<entity::Entity>` is not implemented for `UniqueEntityEquivalentVec<entity::Entity>`
```

These impls were removed because they performed an O(N<sup>2</sup>) check for duplicates.
This could easily cause performance problems for large collections,
but would be hidden behind an innocent-looking `collect()` call.

Most callers should use `collect_set()` if possible, 
and `UniqueEntityEquivalentVec::from_iter_with_hash` to de-duplicate using hashing if not.
If you need exactly the old behavior, call `UniqueEntityEquivalentVec::from_iter_slow`,
and if you want to de-duplicating using sorting, call `UniqueEntityEquivalentVec::from_iter_with_sort`.

For `Box<UniqueEntityEquivalentSlice>`, first create a `UniqueEntityEquivalentVec` as above,
and then call `.into_boxed_slice()`.

For `extend()`, the old behavior is available as `UniqueEntityEquivalentVec::extend_slow`.

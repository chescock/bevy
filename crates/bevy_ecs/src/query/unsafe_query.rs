use crate::{
    batching::BatchingStrategy,
    component::Tick,
    entity::{Entity, EntityDoesNotExistError, EntityEquivalent, EntitySet},
    query::{
        DebugCheckedUnwrap, NopWorldQuery, QueryCombinationIter, QueryData, QueryEntityError,
        QueryFilter, QueryIter, QueryManyIter, QueryManyUniqueIter, QueryParIter, QueryParManyIter,
        QueryParManyUniqueIter, QueryState,
    },
    system::{Query, QueryLens},
    world::unsafe_world_cell::UnsafeWorldCell,
};

/// A [`Query`] that does not guarantee access to all matched entities or components.
///
/// An [`UnsafeQuery`] can be obtained from [`Query::as_unsafe_query`] or [`Query::into_unsafe_query`].
///
/// This is used for advanced scenarios that cannot be expressed with [`Query`],
/// and requires manually proving that components do not alias.
///
/// Callers of any `unsafe` methods on [`UnsafeQuery`] must ensure that there are no aliasing violations.
/// That means they must not create multiple query items for the same entity at the same time,
/// unless the query is read-only.
pub struct UnsafeQuery<'world, 'state, D: QueryData, F: QueryFilter = ()> {
    // SAFETY: Must have access to the components registered in `state`.
    world: UnsafeWorldCell<'world>,
    state: &'state QueryState<D, F>,
    last_run: Tick,
    this_run: Tick,
}

impl<D: QueryData, F: QueryFilter> Clone for UnsafeQuery<'_, '_, D, F> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<D: QueryData, F: QueryFilter> Copy for UnsafeQuery<'_, '_, D, F> {}

impl<D: QueryData, F: QueryFilter> core::fmt::Debug for UnsafeQuery<'_, '_, D, F> {
    fn fmt(&self, f: &mut core::fmt::Formatter) -> core::fmt::Result {
        f.debug_struct("Query")
            .field("state", &self.state)
            .field("last_run", &self.last_run)
            .field("this_run", &self.this_run)
            .field("world", &self.world)
            .finish()
    }
}

impl<'w, 's, D: QueryData, F: QueryFilter> UnsafeQuery<'w, 's, D, F> {
    /// Creates a new [`UnsafeQuery`].
    ///
    /// # Safety
    ///
    /// `world` must be the world used to create `state`.
    #[inline]
    pub(crate) unsafe fn new(
        world: UnsafeWorldCell<'w>,
        state: &'s QueryState<D, F>,
        last_run: Tick,
        this_run: Tick,
    ) -> Self {
        Self {
            world,
            state,
            last_run,
            this_run,
        }
    }

    /// Constructs a safe [`Query`] from an [`UnsafeQuery`].
    ///
    /// # Safety
    ///
    /// This will create a query that could violate memory safety rules. Make sure that this is only
    /// called in ways that ensure the queries have unique mutable access.
    pub unsafe fn query(&self) -> Query<'w, 's, D, F> {
        // SAFETY:
        // - Caller ensures `self.world` has permission to access the required components.
        // - The world matches because it was the same one used to construct self.
        unsafe { Query::new(*self) }
    }

    /// Returns another `Query` from this does not return any data, which can be faster.
    ///
    /// # See also
    ///
    /// [`Query::as_nop`]
    pub(crate) fn as_nop(&self) -> UnsafeQuery<'w, 's, NopWorldQuery<D>, F> {
        let new_state = self.state.as_nop();
        // SAFETY: The world matches because it was the same one used to construct self.
        unsafe { UnsafeQuery::new(self.world, new_state, self.last_run, self.this_run) }
    }

    /// Returns another `Query` from this that fetches the read-only version of the query items.
    ///
    /// # See also
    ///
    /// [`Query::as_readonly`]
    pub fn as_readonly(&self) -> UnsafeQuery<'w, 's, D::ReadOnly, F> {
        let new_state = self.state.as_readonly();
        // SAFETY: The world matches because it was the same one used to construct self.
        unsafe { UnsafeQuery::new(self.world, new_state, self.last_run, self.this_run) }
    }

    /// Returns a [`QueryCombinationIter`] over all combinations of `K` query items without repetition.
    /// This consumes the [`Query`] to return results with the actual "inner" world lifetime.
    ///
    /// This iterator is always guaranteed to return results from each unique pair of matching entities.
    /// Iteration order is not guaranteed.
    ///
    /// # Safety
    ///
    /// Caller must ensure that there are no conflicting accesses created by this query.
    /// That means the `UnsafeQuery` has access to any entities queried,
    /// and that no concurrent access to the same entities are made through the `UnsafeQuery` unless the query is read-only.
    ///
    /// # See also
    ///
    /// [`Query::iter_combinations`] for the safe version.
    #[inline]
    pub unsafe fn iter_combinations<const K: usize>(
        &self,
    ) -> QueryCombinationIter<'w, 's, D, F, K> {
        // SAFETY: Caller ensures `self.world` has permission to access the required components.
        unsafe { QueryCombinationIter::new(self.world, self.state, self.last_run, self.this_run) }
    }

    /// Returns an iterator over the query items generated from an [`Entity`] list.
    /// This consumes the [`Query`] to return results with the actual "inner" world lifetime.
    ///
    /// Items are returned in the order of the list of entities, and may not be unique if the input
    /// doesn't guarantee uniqueness. Entities that don't match the query are skipped.
    ///
    /// # Safety
    ///
    /// Caller must ensure that there are no conflicting accesses created by this query.
    /// That means the `UnsafeQuery` has access to any entities queried,
    /// and that no concurrent access to the same entities are made through the `UnsafeQuery` unless the query is read-only.
    ///
    /// # See also
    ///
    /// [`Query::iter_many`] for the safe version.
    #[inline]
    pub unsafe fn iter_many<EntityList: IntoIterator<Item: EntityEquivalent>>(
        &self,
        entities: EntityList,
    ) -> QueryManyIter<'w, 's, D, F, EntityList::IntoIter> {
        // SAFETY: Caller ensures `self.world` has permission to access the required components.
        unsafe {
            QueryManyIter::new(
                self.world,
                self.state,
                entities,
                self.last_run,
                self.this_run,
            )
        }
    }

    /// Returns an iterator over the unique query items generated from an [`EntitySet`].
    /// This consumes the [`Query`] to return results with the actual "inner" world lifetime.
    ///
    /// Items are returned in the order of the list of entities. Entities that don't match the query are skipped.
    ///
    /// # Safety
    ///
    /// Caller must ensure that there are no conflicting accesses created by this query.
    /// That means the `UnsafeQuery` has access to any entities queried,
    /// and that no concurrent access to the same entities are made through the `UnsafeQuery` unless the query is read-only.
    ///
    /// # See also
    ///
    /// [`Query::iter_many_unique`] for the safe version.
    #[inline]
    pub unsafe fn iter_many_unique<EntityList: EntitySet>(
        &self,
        entities: EntityList,
    ) -> QueryManyUniqueIter<'w, 's, D, F, EntityList::IntoIter> {
        // SAFETY: Caller ensures `self.world` has permission to access the required components.
        unsafe {
            QueryManyUniqueIter::new(
                self.world,
                self.state,
                entities,
                self.last_run,
                self.this_run,
            )
        }
    }

    /// Returns a parallel iterator over the query results for the given [`World`](crate::world::World).
    /// This consumes the [`Query`] to return results with the actual "inner" world lifetime.
    ///
    /// This parallel iterator is always guaranteed to return results from each matching entity once and
    /// only once.  Iteration order and thread assignment is not guaranteed.
    ///
    /// If the `multithreaded` feature is disabled, iterating with this operates identically to [`Iterator::for_each`]
    /// on [`QueryIter`].
    ///
    /// # Safety
    ///
    /// Caller must ensure that there are no conflicting accesses created by this query.
    /// That means the `UnsafeQuery` has access to any entities queried,
    /// and that no concurrent access to the same entities are made through the `UnsafeQuery` unless the query is read-only.
    ///
    /// # See also
    ///
    /// [`Query::par_iter`] for the safe version.
    #[inline]
    pub unsafe fn par_iter(&self) -> QueryParIter<'w, 's, D, F> {
        QueryParIter {
            world: self.world,
            state: self.state,
            last_run: self.last_run,
            this_run: self.this_run,
            batching_strategy: BatchingStrategy::new(),
        }
    }

    /// Returns a parallel iterator over the read-only query items generated from an [`Entity`] list.
    ///
    /// Entities that don't match the query are skipped. Iteration order and thread assignment is not guaranteed.
    ///
    /// If the `multithreaded` feature is disabled, iterating with this operates identically to [`Iterator::for_each`]
    /// on [`QueryManyIter`].
    ///
    /// # Safety
    ///
    /// Caller must ensure that there are no conflicting accesses created by this query.
    /// That means the `UnsafeQuery` has access to any entities queried,
    /// and that no concurrent access to the same entities are made through the `UnsafeQuery` unless the query is read-only.
    ///
    /// # See also
    ///
    /// [`Query::par_iter_many`] for the safe version.
    #[inline]
    pub unsafe fn par_iter_many<EntityList: IntoIterator<Item: EntityEquivalent>>(
        &self,
        entities: EntityList,
    ) -> QueryParManyIter<'w, 's, D, F, EntityList::Item> {
        QueryParManyIter {
            world: self.world,
            state: self.state,
            entity_list: entities.into_iter().collect(),
            last_run: self.last_run,
            this_run: self.this_run,
            batching_strategy: BatchingStrategy::new(),
        }
    }

    /// Returns a parallel iterator over the unique read-only query items generated from an [`EntitySet`].
    ///
    /// Entities that don't match the query are skipped. Iteration order and thread assignment is not guaranteed.
    ///
    /// If the `multithreaded` feature is disabled, iterating with this operates identically to [`Iterator::for_each`]
    /// on [`QueryManyUniqueIter`].
    ///
    /// # Safety
    ///
    /// Caller must ensure that there are no conflicting accesses created by this query.
    /// That means the `UnsafeQuery` has access to any entities queried,
    /// and that no concurrent access to the same entities are made through the `UnsafeQuery` unless the query is read-only.
    ///
    /// # See also
    ///
    /// [`Query::par_iter_many`] for the safe version.
    #[inline]
    pub unsafe fn par_iter_many_unique<EntityList: EntitySet<Item: Sync>>(
        &self,
        entities: EntityList,
    ) -> QueryParManyUniqueIter<'w, 's, D, F, EntityList::Item> {
        QueryParManyUniqueIter {
            world: self.world,
            state: self.state,
            entity_list: entities.into_iter().collect(),
            last_run: self.last_run,
            this_run: self.this_run,
            batching_strategy: BatchingStrategy::new(),
        }
    }

    /// Returns the query item for the given [`Entity`].
    /// This consumes the [`Query`] to return results with the actual "inner" world lifetime.
    ///
    /// In case of a nonexisting entity or mismatched component, a [`QueryEntityError`] is returned instead.
    ///
    /// This is always guaranteed to run in `O(1)` time.
    ///
    /// # Safety
    ///
    /// Caller must ensure that there are no conflicting accesses created by this query.
    /// That means the `UnsafeQuery` has access to any entities queried,
    /// and that no concurrent access to the same entities are made through the `UnsafeQuery` unless the query is read-only.
    ///
    /// # See also
    ///
    /// [`Query::get`] for the safe version.
    #[inline]
    pub unsafe fn get(&self, entity: Entity) -> Result<D::Item<'w, 's>, QueryEntityError> {
        // SAFETY: Caller ensures `self.world` has permission to access the required components.
        unsafe {
            let location = self
                .world
                .entities()
                .get(entity)
                .ok_or(EntityDoesNotExistError::new(entity, self.world.entities()))?;
            if !self
                .state
                .matched_archetypes
                .contains(location.archetype_id.index())
            {
                return Err(QueryEntityError::QueryDoesNotMatch(
                    entity,
                    location.archetype_id,
                ));
            }
            let archetype = self
                .world
                .archetypes()
                .get(location.archetype_id)
                .debug_checked_unwrap();
            let mut fetch = D::init_fetch(
                self.world,
                &self.state.fetch_state,
                self.last_run,
                self.this_run,
            );
            let mut filter = F::init_fetch(
                self.world,
                &self.state.filter_state,
                self.last_run,
                self.this_run,
            );

            let table = self
                .world
                .storages()
                .tables
                .get(location.table_id)
                .debug_checked_unwrap();
            D::set_archetype(&mut fetch, &self.state.fetch_state, archetype, table);
            F::set_archetype(&mut filter, &self.state.filter_state, archetype, table);

            if F::filter_fetch(
                &self.state.filter_state,
                &mut filter,
                entity,
                location.table_row,
            ) {
                Ok(D::fetch(
                    &self.state.fetch_state,
                    &mut fetch,
                    entity,
                    location.table_row,
                ))
            } else {
                Err(QueryEntityError::QueryDoesNotMatch(
                    entity,
                    location.archetype_id,
                ))
            }
        }
    }

    /// Returns `true` if there are no query items.
    ///
    /// This is equivalent to `self.iter().next().is_none()`, and thus the worst case runtime will be `O(n)`
    /// where `n` is the number of *potential* matches. This can be notably expensive for queries that rely
    /// on non-archetypal filters such as [`Added`], [`Changed`] or [`Spawned`] which must individually check
    /// each query result for a match.
    ///
    /// [`Added`]: crate::query::Added
    /// [`Changed`]: crate::query::Changed
    /// [`Spawned`]: crate::query::Spawned
    ///
    /// # Safety
    ///
    /// Caller must ensure that there are no conflicting accesses created by this query.
    /// That means the `UnsafeQuery` has access to any entities queried,
    /// and that no concurrent access to the same entities are made through the `UnsafeQuery` unless the query is read-only.
    #[inline]
    pub unsafe fn is_empty(&self) -> bool {
        // SAFETY: Caller ensures `self.world` has permission to access the required components.
        unsafe { self.as_nop().iter() }.next().is_none()
    }

    /// Returns `true` if the given [`Entity`] matches the query.
    ///
    /// This is always guaranteed to run in `O(1)` time.
    ///
    /// # Safety
    ///
    /// Caller must ensure that there are no conflicting accesses created by this query.
    /// That means the `UnsafeQuery` has access to any entities queried,
    /// and that no concurrent access to the same entities are made through the `UnsafeQuery` unless the query is read-only.
    #[inline]
    pub unsafe fn contains(&self, entity: Entity) -> bool {
        // SAFETY: Caller ensures `self.world` has permission to access the required components.
        unsafe { self.as_nop().get(entity) }.is_ok()
    }

    /// Counts the number of entities that match the query.
    ///
    /// This is equivalent to `self.iter().count()` but may be more efficient in some cases.
    ///
    /// If [`F::IS_ARCHETYPAL`](QueryFilter::IS_ARCHETYPAL) is `true`,
    /// this will do work proportional to the number of matched archetypes or tables, but will not iterate each entity.
    /// If it is `false`, it will have to do work for each entity.
    ///
    /// # Safety
    ///
    /// Caller must ensure that there are no conflicting accesses created by this query.
    /// That means the `UnsafeQuery` has access to any entities queried,
    /// and that no concurrent access to the same entities are made through the `UnsafeQuery` unless the query is read-only.
    pub unsafe fn count(&self) -> usize {
        // SAFETY: Caller ensures `self.world` has permission to access the required components.
        let iter = unsafe { self.as_nop().iter() };
        if F::IS_ARCHETYPAL {
            // For archetypal queries, the `size_hint()` is exact,
            // and we can get the count from the archetype and table counts.
            iter.size_hint().0
        } else {
            // If we have non-archetypal filters, we have to check each entity.
            iter.count()
        }
    }

    /// Returns a [`QueryLens`] that can be used to construct a new [`Query`] giving more
    /// restrictive access to the entities matched by the current query.
    ///
    /// # Safety
    ///
    /// Caller must have access to all entities and components matched by the query,
    /// as `QueryLens` offers access to them.
    ///
    /// # See also
    ///
    /// [`Query::transmute_lens`] for the safe version.
    #[track_caller]
    pub unsafe fn transmute_lens<NewD: QueryData>(&self) -> QueryLens<'w, NewD> {
        // SAFETY: Caller ensures `self.world` has permission to access the required components.
        unsafe { self.transmute_lens_filtered::<NewD, ()>() }
    }

    /// Equivalent to [`Self::transmute_lens`] but also includes a [`QueryFilter`] type.
    /// This consumes the [`Query`] to return results with the actual "inner" world lifetime.
    ///
    /// See [`Self::transmute_lens`] for a description of allowed transmutes.
    ///
    /// Note that the lens will iterate the same tables and archetypes as the original query. This means that
    /// additional archetypal query terms like [`With`](crate::query::With) and [`Without`](crate::query::Without)
    /// will not necessarily be respected and non-archetypal terms like [`Added`](crate::query::Added),
    /// [`Changed`](crate::query::Changed) and [`Spawned`](crate::query::Spawned) will only be respected if they
    /// are in the type signature.
    ///
    /// # Safety
    ///
    /// Caller must have access to all entities and components matched by the query,
    /// as `QueryLens` offers access to them.
    ///
    /// # See also
    ///
    /// [`Query::transmute_lens_filtered`] for the safe version.
    pub unsafe fn transmute_lens_filtered<NewD: QueryData, NewF: QueryFilter>(
        &self,
    ) -> QueryLens<'w, NewD, NewF> {
        let state = self.state.transmute_filtered::<NewD, NewF>(self.world);
        // SAFETY: Caller ensures `self.world` has permission to access the required components.
        unsafe { QueryLens::new(self.world, state, self.last_run, self.this_run) }
    }

    /// Returns a [`QueryLens`] that can be used to get a query with the combined fetch.
    ///
    /// For example, this can take a `Query<&A>` and a `Query<&B>` and return a `Query<(&A, &B)>`.
    /// The returned query will only return items with both `A` and `B`. Note that since filters
    /// are dropped, non-archetypal filters like `Added`, `Changed` and `Spawned` will not be respected.
    /// To maintain or change filter terms see `Self::join_filtered`.
    ///
    /// # Safety
    ///
    /// Caller must have access to all entities and components matched by the query,
    /// as `QueryLens` offers access to them.
    ///
    /// # See also
    ///
    /// [`Query::join`] for the safe version.
    pub unsafe fn join<OtherD: QueryData, NewD: QueryData>(
        &self,
        other: UnsafeQuery<'w, '_, OtherD>,
    ) -> QueryLens<'w, NewD> {
        // SAFETY: Caller ensures `self.world` has permission to access the required components.
        unsafe { self.join_filtered(other) }
    }

    /// Equivalent to [`Self::join`] but also includes a [`QueryFilter`] type.
    /// This consumes the [`Query`] to return results with the actual "inner" world lifetime.
    ///
    /// Note that the lens with iterate a subset of the original queries' tables
    /// and archetypes. This means that additional archetypal query terms like
    /// `With` and `Without` will not necessarily be respected and non-archetypal
    /// terms like `Added`, `Changed` and `Spawned` will only be respected if they
    /// are in the type signature.
    ///
    /// # Safety
    ///
    /// Caller must have access to all entities and components matched by the query,
    /// as `QueryLens` offers access to them.
    ///
    /// # See also
    ///
    /// [`Query::join_filtered`] for the safe version.
    pub unsafe fn join_filtered<
        OtherD: QueryData,
        OtherF: QueryFilter,
        NewD: QueryData,
        NewF: QueryFilter,
    >(
        &self,
        other: UnsafeQuery<'w, '_, OtherD, OtherF>,
    ) -> QueryLens<'w, NewD, NewF> {
        let state = self
            .state
            .join_filtered::<OtherD, OtherF, NewD, NewF>(self.world, other.state);
        // SAFETY: Caller ensures `self.world` has permission to access the required components.
        unsafe { QueryLens::new(self.world, state, self.last_run, self.this_run) }
    }

    /// Returns an [`Iterator`] over the query items.
    ///
    /// This iterator is always guaranteed to return results from each matching entity once and only once.
    /// Iteration order is not guaranteed.
    ///
    /// # Safety
    ///
    /// Caller must ensure that there are no conflicting accesses created by this query.
    /// That means the `UnsafeQuery` has access to any entities queried,
    /// and that no concurrent access to the same entities are made through the `UnsafeQuery` unless the query is read-only.
    ///
    /// # See also
    ///
    /// [`Query::iter`] for the safe version.
    #[inline]
    pub unsafe fn iter(&self) -> QueryIter<'w, 's, D, F> {
        // SAFETY: Caller ensures `self.world` has permission to access the required components.
        unsafe { QueryIter::new(self.world, self.state, self.last_run, self.this_run) }
    }
}

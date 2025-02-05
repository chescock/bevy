use alloc::vec::Vec;

use crate::{
    archetype::Archetype,
    component::{ComponentId, Immutable, Tick},
    prelude::Component,
    query::{QueryData, QueryFilter, QueryState, ReadOnlyQueryData, With},
    system::{init_query_param, Query, Res, SystemMeta, SystemParam},
    world::{unsafe_world_cell::UnsafeWorldCell, World},
};
use bevy_platform_support::sync::Arc;

use super::Index;

/// This system parameter allows querying by an indexable component value.
///
/// # Examples
///
/// ```rust
/// # use bevy_ecs::prelude::*;
/// # let mut world = World::new();
/// #[derive(Component, PartialEq, Eq, Hash, Clone)]
/// #[component(immutable)]
/// struct Player(u8);
///
/// // Indexing is opt-in through `World::add_index`
/// world.add_index::<Player>();
/// # for i in 0..6 {
/// #   for _ in 0..(i + 1) {
/// #       world.spawn(Player(i));
/// #   }
/// # }
/// #
/// # world.flush();
///
/// fn find_all_player_one_entities(mut by_player: QueryByIndex<Player, Entity>) {
///     for entity in by_player.at(&Player(0)).iter() {
///         println!("{entity:?} belongs to Player 1!");
///     }
/// #   assert_eq!((
/// #       by_player.at(&Player(0)).iter().count(),
/// #       by_player.at(&Player(1)).iter().count(),
/// #       by_player.at(&Player(2)).iter().count(),
/// #       by_player.at(&Player(3)).iter().count(),
/// #       by_player.at(&Player(4)).iter().count(),
/// #       by_player.at(&Player(5)).iter().count(),
/// #    ), (1, 2, 3, 4, 5, 6));
/// }
/// # world.run_system_cached(find_all_player_one_entities);
/// ```
pub struct QueryByIndex<
    'world,
    'state,
    C: Component<Mutability = Immutable>,
    D: QueryData + 'static,
    F: QueryFilter + 'static = (),
> {
    world: UnsafeWorldCell<'world>,
    empty_query_state: &'state QueryState<D, (F, With<C>)>,
    query_states: &'state [QueryState<D, (F, With<C>)>],
    last_run: Tick,
    this_run: Tick,
    index: &'world Index<C>,
}

impl<C: Component<Mutability = Immutable>, D: ReadOnlyQueryData, F: QueryFilter> Clone
    for QueryByIndex<'_, '_, C, D, F>
{
    fn clone(&self) -> Self {
        *self
    }
}

impl<C: Component<Mutability = Immutable>, D: ReadOnlyQueryData, F: QueryFilter> Copy
    for QueryByIndex<'_, '_, C, D, F>
{
}

impl<'w, 's, C: Component<Mutability = Immutable>, D: QueryData, F: QueryFilter>
    QueryByIndex<'w, 's, C, D, F>
{
    /// Return a [`Query`] only returning entities with a component `C` of the provided value.
    ///
    /// # See also
    /// - [`at_inner`](Self::at) for mutable query items with the actual "inner" world lifetime.
    /// - [`at`](Self::at) for read-only query items.
    ///
    /// # Examples
    ///
    /// ```rust
    /// # use bevy_ecs::prelude::*;
    /// # let mut world = World::new();
    /// #[derive(Component, PartialEq, Eq, Hash, Clone)]
    /// #[component(immutable)]
    /// enum FavoriteColor { Red, Green, Blue }
    ///
    /// world.add_index::<FavoriteColor>();
    ///
    /// fn find_red_fans(mut query: QueryByIndex<FavoriteColor, Entity>) {
    ///     for entity in query.at_mut(&FavoriteColor::Red).iter_mut() {
    ///         println!("{entity:?} likes the color Red!");
    ///     }
    /// }
    /// ```
    pub fn at_mut(&mut self, value: &C) -> Query<'_, 's, D, (F, With<C>)> {
        self.reborrow().at_inner(value)
    }

    /// Return a read-only [`Query`] only returning entities with a component `C` of the provided value.
    ///
    /// # See also
    /// - [`at_inner`](Self::at) for mutable query items with the actual "inner" world lifetime.
    /// - [`at_mut`](Self::at) for mutable query items.
    ///
    /// # Examples
    ///
    /// ```rust
    /// # use bevy_ecs::prelude::*;
    /// # let mut world = World::new();
    /// #[derive(Component, PartialEq, Eq, Hash, Clone)]
    /// #[component(immutable)]
    /// enum FavoriteColor { Red, Green, Blue }
    ///
    /// world.add_index::<FavoriteColor>();
    ///
    /// fn find_red_fans(query: QueryByIndex<FavoriteColor, Entity>) {
    ///     for entity in query.at(&FavoriteColor::Red).iter() {
    ///         println!("{entity:?} likes the color Red!");
    ///     }
    /// }
    /// ```
    pub fn at(&self, value: &C) -> Query<'_, 's, D::ReadOnly, (F, With<C>)> {
        self.as_readonly().at_inner(value)
    }

    /// Return a [`Query`] only returning entities with a component `C` of the provided value.
    /// This version is `unsafe` because it only requires `&self`.
    /// It can be used to perform mutable queries on different index values concurrently.
    ///
    /// # Safety
    ///
    /// This function makes it possible to violate Rust's aliasing guarantees.
    /// You must make sure this call does not result in multiple mutable references to the same component.
    ///
    /// # See also
    /// - [`at_inner`](Self::at) for mutable query items with the actual "inner" world lifetime.
    /// - [`at`](Self::at) for read-only query items.
    /// - [`at_mut`](Self::at) for mutable query items.
    ///
    /// # Examples
    ///
    /// ```rust
    /// # use bevy_ecs::prelude::*;
    /// # let mut world = World::new();
    /// #[derive(Component, PartialEq, Eq, Hash, Clone)]
    /// #[component(immutable)]
    /// enum FavoriteColor { Red, Green, Blue }
    ///
    /// #[derive(Component)]
    /// struct FavoriteNumber(usize);
    ///
    /// world.add_index::<FavoriteColor>();
    ///
    /// fn find_red_fans(query: QueryByIndex<FavoriteColor, &mut FavoriteNumber>) {
    ///     // SAFETY: The first call to `at_unsafe` only queries entities with `FavoriteColor::Red`,
    ///     // and the second only queries entities with `FavoriteColor::Green`,
    ///     // so we never have multiple references to the same entity at the same time.
    ///     for red_number in unsafe { query.at_unsafe(&FavoriteColor::Red) }.iter_mut() {
    ///         for green_number in unsafe { query.at_unsafe(&FavoriteColor::Green) }.iter_mut() {
    ///         }
    ///     }
    /// }
    /// ```
    pub unsafe fn at_unsafe(&self, value: &C) -> Query<'_, 's, D, (F, With<C>)> {
        // SAFETY: The caller promises that this will not result in multiple mutable references.
        unsafe { self.reborrow_unsafe() }.at_inner(value)
    }

    /// Return a [`Query`] only returning entities with a component `C` of the provided value,
    /// with the actual "inner" world lifetime.
    /// This is most useful when working with a [`SystemState`](crate::system::SystemState)
    /// and using [`Query::iter_inner()`](crate::system::Query::iter_inner).
    ///
    /// # See also
    /// - [`at`](Self::at) for read-only query items.
    /// - [`at_mut`](Self::at) for mutable query items.
    ///
    /// # Examples
    ///
    /// ```rust
    /// # use bevy_ecs::{prelude::*, system::SystemState};
    /// # let mut world = World::new();
    /// #
    /// #[derive(Component, PartialEq, Eq, Hash, Clone)]
    /// #[component(immutable)]
    /// enum FavoriteColor { Red, Green, Blue }
    ///
    /// #[derive(Component)]
    /// struct FavoriteNumber(usize);
    ///
    /// world.add_index::<FavoriteColor>();
    ///
    /// let number: Option<&FavoriteNumber> = {
    ///     // `state` is only alive during this block,
    ///     // but we use it to return a reference from `world`.
    ///     // Using `at()` instead of `at_inner()` fails with "`query` does not live long enough".
    ///     let mut state =
    ///         SystemState::<QueryByIndex<FavoriteColor, &FavoriteNumber>>::new(&mut world);
    ///     let query = state.get_mut(&mut world);
    ///     query.at_inner(&FavoriteColor::Red).iter_inner().next()
    /// };
    /// ```
    pub fn at_inner(self, value: &C) -> Query<'w, 's, D, (F, With<C>)> {
        let index = self.index.mapping.get(value);
        // If the value was not found in the mapping, there are no matching entities and we can use the empty state.
        // If the value was found but was out of range, then we have not seen any archetypes matching it yet,
        // so we can still use the empty state.
        let state = index
            .and_then(|index| self.query_states.get(index))
            .unwrap_or(self.empty_query_state);
        // SAFETY: We have registered all of the query's world accesses,
        // so the caller ensures that `world` has permission to access any
        // world data that the query needs.
        unsafe { Query::new(self.world, state, self.last_run, self.this_run) }
    }

    /// Returns another `QueryByIndex` from this that fetches the read-only version of the query items.
    pub fn as_readonly(&self) -> QueryByIndex<'_, 's, C, D::ReadOnly, F> {
        // SAFETY: The reborrowed query is converted to read-only, so it cannot perform mutable access,
        // and the original query is held with a shared borrow, so it cannot perform mutable access either.
        unsafe { self.reborrow_unsafe() }.into_readonly()
    }

    /// Returns another `QueryByIndex` from this that fetches the read-only version of the query items.
    pub fn into_readonly(self) -> QueryByIndex<'w, 's, C, D::ReadOnly, F> {
        QueryByIndex {
            world: self.world,
            empty_query_state: self.empty_query_state.as_readonly(),
            query_states: QueryState::as_readonly_slice(self.query_states),
            last_run: self.last_run,
            this_run: self.this_run,
            index: self.index,
        }
    }

    /// Returns a new `QueryByIndex` reborrowing the access from this one. The current query will be unusable
    /// while the new one exists.
    pub fn reborrow(&mut self) -> QueryByIndex<'_, 's, C, D, F> {
        // SAFETY: this query is exclusively borrowed while the new one exists, so
        // no overlapping access can occur.
        unsafe { self.reborrow_unsafe() }
    }

    /// Returns a new `Query` reborrowing the access from this one.
    /// The current query will still be usable while the new one exists, but must not be used in a way that violates aliasing.
    ///
    /// # Safety
    ///
    /// This function makes it possible to violate Rust's aliasing guarantees.
    /// You must make sure this call does not result in a mutable or shared reference to a component with a mutable reference.
    ///
    /// # See also
    ///
    /// - [`reborrow`](Self::reborrow) for the safe versions.
    pub unsafe fn reborrow_unsafe(&self) -> QueryByIndex<'_, 's, C, D, F> {
        QueryByIndex {
            world: self.world,
            empty_query_state: self.empty_query_state,
            query_states: self.query_states,
            last_run: self.last_run,
            this_run: self.this_run,
            index: self.index,
        }
    }
}

#[doc(hidden)]
pub struct QueryByIndexState<
    C: Component<Mutability = Immutable>,
    D: QueryData + 'static,
    F: QueryFilter + 'static,
> {
    /// A `QueryState` for the underlying query that never match any archetypes.
    empty_query_state: QueryState<D, (F, With<C>)>,
    /// A list of `QueryState`s that each match the archetypes for a single index value.
    query_states: Vec<QueryState<D, (F, With<C>)>>,
    /// The `SystemParam::State` for a `Res<Index<C>>` parameter.
    index_state: ComponentId,
    /// A copy of the markers from the `Index<C>`.
    /// We need these in `new_archetype`, but cannot access the `Index` then.
    markers: Arc<[ComponentId]>,
}

// SAFETY: We rely on the known-safe implementations of `SystemParam` for `Res` and `Query`.
unsafe impl<C: Component<Mutability = Immutable>, D: QueryData + 'static, F: QueryFilter + 'static>
    SystemParam for QueryByIndex<'_, '_, C, D, F>
where
    QueryState<D, (F, With<C>)>: Clone,
{
    type State = QueryByIndexState<C, D, F>;
    type Item<'w, 's> = QueryByIndex<'w, 's, C, D, F>;

    fn init_state(world: &mut World, system_meta: &mut SystemMeta) -> Self::State {
        let Some(index) = world.get_resource::<Index<C>>() else {
            panic!(
                "Index not setup prior to usage. Please call `app.add_index::<{}>()` during setup",
                disqualified::ShortName::of::<C>(),
            );
        };

        let markers = index.markers.clone();

        // Start with an uninitialized `QueryState`.
        // Any existing archetypes in the world will be populated when the system calls `new_archetype` during initialization.
        let empty_query_state = QueryState::new_uninitialized(world);
        init_query_param(world, system_meta, &empty_query_state);
        let index_state = <Res<Index<C>> as SystemParam>::init_state(world, system_meta);
        QueryByIndexState {
            empty_query_state,
            query_states: Vec::new(),
            index_state,
            markers,
        }
    }

    unsafe fn new_archetype(
        state: &mut Self::State,
        archetype: &Archetype,
        system_meta: &mut SystemMeta,
    ) {
        if state.empty_query_state.archetype_matches(archetype) {
            // This archetype matches the overall query.
            // We only want to add it to the one `QueryState` that matches the index value,
            // so that it will only be returned when querying by that value.

            // We can determine the index for an archetype by looking at which
            // marker components are present and adding up the relevant bits.
            let indexed_markers = state.markers.iter().enumerate();
            let index = indexed_markers
                .filter(|(_, &id)| archetype.contains(id))
                .map(|(i, _)| 1 << i)
                .sum::<usize>();

            // Grow the list of query states if necessary,
            // cloning the empty state so that they don't match anything yet.
            if index >= state.query_states.len() {
                state
                    .query_states
                    .resize_with(index + 1, || state.empty_query_state.clone());
            }

            // SAFETY:
            // - Caller ensures `archetype` is from the right world.
            // - We called `archetype_matches` on an identical `QueryState` above.
            unsafe {
                state.query_states[index].new_archetype_unchecked(archetype);
                state.query_states[index].update_archetype_component_access(
                    archetype,
                    system_meta.archetype_component_access_mut(),
                );
            }
        }
    }

    #[inline]
    unsafe fn validate_param(
        state: &Self::State,
        system_meta: &SystemMeta,
        world: UnsafeWorldCell,
    ) -> bool {
        <Res<Index<C>> as SystemParam>::validate_param(&state.index_state, system_meta, world)
    }

    unsafe fn get_param<'world, 'state>(
        state: &'state mut Self::State,
        system_meta: &SystemMeta,
        world: UnsafeWorldCell<'world>,
        change_tick: Tick,
    ) -> Self::Item<'world, 'state> {
        let index = <Res<Index<C>> as SystemParam>::get_param(
            &mut state.index_state,
            system_meta,
            world,
            change_tick,
        )
        .into_inner();

        QueryByIndex {
            world,
            empty_query_state: &state.empty_query_state,
            query_states: &state.query_states,
            last_run: system_meta.last_run,
            this_run: change_tick,
            index,
        }
    }
}

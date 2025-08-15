//! Centralized storage for observers, allowing for efficient look-ups.
//!
//! This has multiple levels:
//! - [`World::observers`] provides access to [`Observers`], which is a central storage for all observers.
//! - [`Observers`] contains multiple distinct caches in the form of [`CachedObservers`].
//!     - Most observers are looked up by the [`ComponentId`] of the event they are observing
//!     - Lifecycle observers have their own fields to save lookups.
//! - [`CachedObservers`] contains maps of [`ObserverRunner`]s, which are the actual functions that will be run when the observer is triggered.
//!     - These are split by target type, in order to allow for different lookup strategies.
//!     - [`CachedComponentObservers`] is one of these maps, which contains observers that are specifically targeted at a component.

use bevy_platform::{collections::HashMap, sync::Arc};

use crate::{
    archetype::{ArchetypeFlags, ArchetypeId, Archetypes},
    change_detection::MaybeLocation,
    component::ComponentId,
    entity::{EntityHashMap, EntityIndexMap},
    observer::{ObserverRunner, ObserverTrigger},
    prelude::*,
    world::DeferredWorld,
};

use alloc::vec::Vec;

/// An internal lookup table tracking all of the observers in the world.
///
/// Stores a cache mapping trigger ids to the registered observers.
/// Some observer kinds (like [lifecycle](crate::lifecycle) observers) have a dedicated field,
/// saving lookups for the most common triggers.
///
/// This can be accessed via [`World::observers`].
#[derive(Default, Debug)]
pub struct Observers {
    // Cached ECS observers to save a lookup most common triggers.
    add: CachedObservers,
    insert: CachedObservers,
    replace: CachedObservers,
    remove: CachedObservers,
    despawn: CachedObservers,
    // Map from trigger type to set of observers listening to that trigger
    cache: HashMap<EventKey, CachedObservers>,
}

impl Observers {
    pub(crate) fn get_observers_mut(&mut self, event_key: EventKey) -> &mut CachedObservers {
        use crate::lifecycle::*;

        match event_key {
            ADD => &mut self.add,
            INSERT => &mut self.insert,
            REPLACE => &mut self.replace,
            REMOVE => &mut self.remove,
            DESPAWN => &mut self.despawn,
            _ => self.cache.entry(event_key).or_default(),
        }
    }

    /// Attempts to get the observers for the given `event_key`.
    ///
    /// When accessing the observers for lifecycle events, such as [`Add`], [`Insert`], [`Replace`], [`Remove`], and [`Despawn`],
    /// use the [`EventKey`] constants from the [`lifecycle`](crate::lifecycle) module.
    pub fn try_get_observers(&self, event_key: EventKey) -> Option<&CachedObservers> {
        use crate::lifecycle::*;

        match event_key {
            ADD => Some(&self.add),
            INSERT => Some(&self.insert),
            REPLACE => Some(&self.replace),
            REMOVE => Some(&self.remove),
            DESPAWN => Some(&self.despawn),
            _ => self.cache.get(&event_key),
        }
    }

    pub(crate) fn invoke_query_observers(
        mut world: DeferredWorld,
        event_key: EventKey,
        target: Entity,
        observers: impl IntoIterator<Item = RunnableObserver>,
        caller: MaybeLocation,
    ) {
        for runnable_observer in observers {
            (runnable_observer.runner)(
                world.reborrow(),
                ObserverTrigger {
                    observer: runnable_observer.observer,
                    event_key,
                    components: Default::default(),
                    current_target: Some(target),
                    original_target: Some(target),
                    caller,
                },
                (&mut ()).into(),
                &mut false,
            );
        }
    }

    /// This will run the observers of the given `event_key`, targeting the given `entity` and `components`.
    pub(crate) fn invoke<T>(
        mut world: DeferredWorld,
        event_key: EventKey,
        current_target: Option<Entity>,
        original_target: Option<Entity>,
        components: impl Iterator<Item = ComponentId> + Clone,
        data: &mut T,
        propagate: &mut bool,
        caller: MaybeLocation,
    ) {
        // SAFETY: You cannot get a mutable reference to `observers` from `DeferredWorld`
        let (mut world, observers) = unsafe {
            let world = world.as_unsafe_world_cell();
            // SAFETY: There are no outstanding world references
            world.increment_trigger_id();
            let observers = world.observers();
            let Some(observers) = observers.try_get_observers(event_key) else {
                return;
            };
            // SAFETY: The only outstanding reference to world is `observers`
            (world.into_deferred(), observers)
        };

        let trigger_for_components = components.clone();

        let mut trigger_observer = |(&observer, runner): (&Entity, &ObserverRunner)| {
            (runner)(
                world.reborrow(),
                ObserverTrigger {
                    observer,
                    event_key,
                    components: components.clone().collect(),
                    current_target,
                    original_target,
                    caller,
                },
                data.into(),
                propagate,
            );
        };
        // Trigger observers listening for any kind of this trigger
        observers
            .global_observers
            .iter()
            .for_each(&mut trigger_observer);

        // Trigger entity observers listening for this kind of trigger
        if let Some(target_entity) = current_target {
            if let Some(map) = observers.entity_observers.get(&target_entity) {
                map.iter().for_each(&mut trigger_observer);
            }
        }

        // Trigger observers listening to this trigger targeting a specific component
        trigger_for_components.for_each(|id| {
            if let Some(component_observers) = observers.component_observers.get(&id) {
                component_observers
                    .global_observers
                    .iter()
                    .for_each(&mut trigger_observer);

                if let Some(target_entity) = current_target {
                    if let Some(map) = component_observers
                        .entity_component_observers
                        .get(&target_entity)
                    {
                        map.iter().for_each(&mut trigger_observer);
                    }
                }
            }
        });
    }

    pub(crate) fn is_archetype_cached(event_key: EventKey) -> Option<ArchetypeFlags> {
        use crate::lifecycle::*;

        match event_key {
            ADD => Some(ArchetypeFlags::ON_ADD_OBSERVER),
            INSERT => Some(ArchetypeFlags::ON_INSERT_OBSERVER),
            REPLACE => Some(ArchetypeFlags::ON_REPLACE_OBSERVER),
            REMOVE => Some(ArchetypeFlags::ON_REMOVE_OBSERVER),
            DESPAWN => Some(ArchetypeFlags::ON_DESPAWN_OBSERVER),
            _ => None,
        }
    }

    pub(crate) fn update_archetype_flags(
        &self,
        component_id: ComponentId,
        flags: &mut ArchetypeFlags,
    ) {
        if self.add.component_observers.contains_key(&component_id) {
            flags.insert(ArchetypeFlags::ON_ADD_OBSERVER);
        }

        if self.insert.component_observers.contains_key(&component_id) {
            flags.insert(ArchetypeFlags::ON_INSERT_OBSERVER);
        }

        if self.replace.component_observers.contains_key(&component_id) {
            flags.insert(ArchetypeFlags::ON_REPLACE_OBSERVER);
        }

        if self.remove.component_observers.contains_key(&component_id) {
            flags.insert(ArchetypeFlags::ON_REMOVE_OBSERVER);
        }

        if self.despawn.component_observers.contains_key(&component_id) {
            flags.insert(ArchetypeFlags::ON_DESPAWN_OBSERVER);
        }
    }

    pub(crate) fn get_archetype_observers(
        &self,
        components: impl Iterator<Item = ComponentId>,
    ) -> Arc<ArchetypeObservers> {
        let enter = todo!();
        let leave = todo!();
        // TODO: dedup arcs
        //  which requires a manual Eq/Hash impl to use pointer equality on arcs
        Arc::new(ArchetypeObservers { enter, leave })
    }

    pub(crate) fn get_edge_observers(
        &self,
        archetypes: &Archetypes,
        source: ArchetypeId,
        target: ArchetypeId,
        keep: &[ComponentId],
        replace: &[ComponentId],
    ) -> Arc<ArchetypeEdgeObservers> {
        let source = archetypes[source].observers.clone();
        let target = archetypes[target].observers.clone();

        // TODO: need to also fire if components change!
        // ... this is tricky because the edge is *shared* between InsertMode::Keep and InsertMode::Replace
        // so we need separate lists?
        // note that we need to check components even for Keep because it may fire on Option or Has
        // the replace list is a *superset* of the other, so maybe keep them as diffs?
        // no, easier to duplicate

        // We need an Access... so maybe replace the EntitySet with an EntityMap<Access> ???
        // then we'd have it here

        // TODO: We need two *different* matches fns
        //  new components trigger on both read or archetypal access and on both replace and keep (which are the same)
        //  existing components only trigger on *read* access, not *archetypal*, and only on replace
        //   because replacing them doesn't affect the value of `Has`
        let matches = |components, observer| false;

        let mut enter_keep = Vec::new();
        let mut enter_replace = Vec::new();
        for runnable_observer in target.iter_enter() {
            if !source.enter.contains_key(&runnable_observer.observer)
                || matches(keep, runnable_observer.observer)
            {
                enter_keep.push(runnable_observer);
                enter_replace.push(runnable_observer);
            } else if matches(replace, runnable_observer.observer) {
                enter_replace.push(runnable_observer);
            }
        }

        let mut leave_keep = Vec::new();
        let mut leave_replace = Vec::new();
        for runnable_observer in source.iter_leave() {
            if !target.leave.contains_key(&runnable_observer.observer)
                || matches(keep, runnable_observer.observer)
            {
                leave_keep.push(runnable_observer);
                leave_replace.push(runnable_observer);
            } else if matches(replace, runnable_observer.observer) {
                leave_replace.push(runnable_observer);
            }
        }

        // TODO: dedup arcs
        //  which requires a manual Eq/Hash impl to use pointer equality on arcs
        Arc::new(ArchetypeEdgeObservers {
            source,
            target,
            enter_keep,
            enter_replace,
            leave_keep,
            leave_replace,
        })
    }
}

/// Collection of [`ObserverRunner`] for [`Observer`] registered to a particular event.
///
/// This is stored inside of [`Observers`], specialized for each kind of observer.
#[derive(Default, Debug)]
pub struct CachedObservers {
    // Observers listening for any time this event is fired, regardless of target
    // This will also respond to events targeting specific components or entities
    pub(super) global_observers: ObserverMap,
    // Observers listening for this trigger fired at a specific component
    pub(super) component_observers: HashMap<ComponentId, CachedComponentObservers>,
    // Observers listening for this trigger fired at a specific entity
    pub(super) entity_observers: EntityHashMap<ObserverMap>,
}

impl CachedObservers {
    /// Returns the observers listening for this trigger, regardless of target.
    /// These observers will also respond to events targeting specific components or entities.
    pub fn global_observers(&self) -> &ObserverMap {
        &self.global_observers
    }

    /// Returns the observers listening for this trigger targeting components.
    pub fn get_component_observers(&self) -> &HashMap<ComponentId, CachedComponentObservers> {
        &self.component_observers
    }

    /// Returns the observers listening for this trigger targeting entities.
    pub fn entity_observers(&self) -> &HashMap<ComponentId, CachedComponentObservers> {
        &self.component_observers
    }
}

/// Map between an observer entity and its [`ObserverRunner`]
pub type ObserverMap = EntityHashMap<ObserverRunner>;

/// Collection of [`ObserverRunner`] for [`Observer`] registered to a particular event targeted at a specific component.
///
/// This is stored inside of [`CachedObservers`].
#[derive(Default, Debug)]
pub struct CachedComponentObservers {
    // Observers listening to events targeting this component, but not a specific entity
    pub(super) global_observers: ObserverMap,
    // Observers listening to events targeting this component on a specific entity
    pub(super) entity_component_observers: EntityHashMap<ObserverMap>,
}

impl CachedComponentObservers {
    /// Returns the observers listening for this trigger, regardless of target.
    /// These observers will also respond to events targeting specific entities.
    pub fn global_observers(&self) -> &ObserverMap {
        &self.global_observers
    }

    /// Returns the observers listening for this trigger targeting this component on a specific entity.
    pub fn entity_component_observers(&self) -> &EntityHashMap<ObserverMap> {
        &self.entity_component_observers
    }
}

#[derive(Copy, Clone)]
pub(crate) struct RunnableObserver {
    observer: Entity,
    runner: ObserverRunner,
}

// TODO: Should these types be in Observers instead?
// that's where we use the internals
// archetype graph just needs the largely-public API of iterating edge observers

/// A set of query observers that observe a specific archetype.
pub(crate) struct ArchetypeObservers {
    enter: EntityIndexMap<ObserverRunner>,
    leave: EntityIndexMap<ObserverRunner>,
}

impl ArchetypeObservers {
    pub fn iter_enter(&self) -> impl Iterator<Item = RunnableObserver> {
        self.enter
            .iter()
            .map(|(&observer, &runner)| RunnableObserver { observer, runner })
    }

    pub fn iter_leave(&self) -> impl Iterator<Item = RunnableObserver> {
        self.leave
            .iter()
            .map(|(&observer, &runner)| RunnableObserver { observer, runner })
    }
}

/// A set of query observers that should trigger on a specific archetype edge.
pub struct ArchetypeEdgeObservers {
    /// The set of query observers on the source archetype.
    /// If this does not match the current value, then this value is stale and should be recalculated.
    source: Arc<ArchetypeObservers>,
    /// The set of query observers on the target archetype.
    /// If this does not match the current value, then this value is stale and should be recalculated.
    target: Arc<ArchetypeObservers>,
    pub(crate) enter_keep: Vec<RunnableObserver>,
    pub(crate) enter_replace: Vec<RunnableObserver>,
    pub(crate) leave_keep: Vec<RunnableObserver>,
    pub(crate) leave_replace: Vec<RunnableObserver>,
}

impl ArchetypeEdgeObservers {
    pub fn is_valid(
        &self,
        source: &Arc<ArchetypeObservers>,
        target: &Arc<ArchetypeObservers>,
    ) -> bool {
        Arc::ptr_eq(&self.source, source) && Arc::ptr_eq(&self.target, target)
    }
}
